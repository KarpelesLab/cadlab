//! Builds the clap command tree from the registry, and turns parsed arguments back into the
//! JSON arguments a command expects.
//!
//! Mapping from a command's input schema:
//! - boolean → `--flag` (or `--flag=false`)
//! - string / number / integer / enum → `--name <VALUE>`
//! - array → `--name a,b` or `--name a --name b`; `--name ""` gives an empty list
//! - object (map) → `--name key=value` (repeatable; `key=` sets null, which removes)
//! - fixed-size tuple of scalars (points) → `--name 12.7mm,8mm` (or JSON)
//! - anything else → `--name '<json>'`, or `--name @file.json` to read the JSON from a file
//!
//! Names listed in the command's `POSITIONAL` become positional arguments.

use cadlab::command::{Entry, Registry};
use clap::{Arg, ArgAction, ArgMatches, Command as ClapCommand};
use serde_json::{Map, Value};

/// What a schema property maps to on the command line.
#[derive(Clone, Debug, PartialEq)]
enum ArgKind {
    Bool,
    Integer,
    Number,
    String {
        choices: Vec<String>,
    },
    Array(Box<ArgKind>),
    Map,
    /// A fixed-size array of scalars (`[x, y]` points): `12.7mm,8mm` or JSON.
    Tuple(usize),
    Json,
}

impl ArgKind {
    fn of(schema: &Value) -> ArgKind {
        // Option<T> is `{"type": [T, "null"]}` or `{"anyOf": [T, {"type": "null"}]}`.
        if let Some(variants) = schema.get("anyOf").or_else(|| schema.get("oneOf")).and_then(Value::as_array) {
            let non_null: Vec<&Value> =
                variants.iter().filter(|v| v.get("type") != Some(&Value::from("null"))).collect();
            if let [one] = non_null.as_slice() {
                return ArgKind::of(one);
            }
            // Enums of string constants.
            let consts: Option<Vec<String>> =
                non_null.iter().map(|v| v.get("const").and_then(Value::as_str).map(String::from)).collect();
            return match consts {
                Some(choices) => ArgKind::String { choices },
                None => ArgKind::Json,
            };
        }
        if let Some(e) = schema.get("enum").and_then(Value::as_array) {
            let choices = e.iter().filter_map(Value::as_str).map(String::from).collect();
            return ArgKind::String { choices };
        }
        let types: Vec<&str> = match schema.get("type") {
            Some(Value::String(t)) => vec![t.as_str()],
            Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).filter(|t| *t != "null").collect(),
            _ => vec![],
        };
        match types.as_slice() {
            ["boolean"] => ArgKind::Bool,
            ["integer"] => ArgKind::Integer,
            ["number"] => ArgKind::Number,
            ["string"] => ArgKind::String { choices: vec![] },
            ["array"]
                if schema.get("prefixItems").and_then(Value::as_array).is_some_and(|items| {
                    items.iter().all(|i| ArgKind::of(i) == ArgKind::String { choices: vec![] })
                }) =>
            {
                ArgKind::Tuple(schema["prefixItems"].as_array().map_or(0, Vec::len))
            }
            ["array"] => {
                let item = schema.get("items").map(ArgKind::of).unwrap_or(ArgKind::Json);
                match item {
                    ArgKind::Array(_) | ArgKind::Map | ArgKind::Json => ArgKind::Json,
                    k => ArgKind::Array(Box::new(k)),
                }
            }
            ["object"] if schema.get("additionalProperties").is_some_and(|a| ArgKind::of(a).is_scalar()) => {
                ArgKind::Map
            }
            // Strings-or-numbers (angles) are passed as strings; commands parse them.
            ["string", "number"] | ["number", "string"] => ArgKind::String { choices: vec![] },
            _ => ArgKind::Json,
        }
    }

    fn is_scalar(&self) -> bool {
        matches!(self, ArgKind::Bool | ArgKind::Integer | ArgKind::Number | ArgKind::String { .. })
    }

    fn convert(&self, raw: &str) -> Result<Value, String> {
        match self {
            ArgKind::Bool => {
                raw.parse::<bool>().map(Value::from).map_err(|_| format!("expected true or false, got `{raw}`"))
            }
            ArgKind::Integer => {
                raw.parse::<i64>().map(Value::from).map_err(|_| format!("expected an integer, got `{raw}`"))
            }
            ArgKind::Number => raw
                .parse::<f64>()
                .ok()
                .and_then(|f| serde_json::Number::from_f64(f).map(Value::Number))
                .ok_or_else(|| format!("expected a number, got `{raw}`")),
            ArgKind::String { .. } => Ok(Value::from(raw)),
            ArgKind::Tuple(n) => {
                if raw.trim_start().starts_with('[') {
                    return ArgKind::Json.convert(raw);
                }
                let parts: Vec<Value> = raw.split(',').map(|p| Value::from(p.trim())).collect();
                if parts.len() != *n {
                    return Err(format!("expected {n} comma-separated values (`12.7mm,8mm`), got `{raw}`"));
                }
                Ok(Value::Array(parts))
            }
            ArgKind::Json => json_arg(raw),
            ArgKind::Array(_) | ArgKind::Map => unreachable!("handled by the caller"),
        }
    }
}

/// A JSON argument: inline JSON, or `@path` to read it from a file.
pub fn json_arg(raw: &str) -> Result<Value, String> {
    let text = match raw.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path).map_err(|e| format!("cannot read `{path}`: {e}"))?,
        None => raw.to_string(),
    };
    // A bare word is a JSON string (`--outline board` for `"board"`): the keyword forms of
    // JSON-valued arguments read as they are documented.
    let bare = !raw.starts_with('@')
        && !raw.is_empty()
        && raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'))
        && !matches!(raw, "true" | "false" | "null")
        && raw.parse::<f64>().is_err();
    if bare {
        return Ok(Value::from(raw));
    }
    serde_json::from_str(&text).map_err(|e| {
        let what = raw.strip_prefix('@').map_or_else(|| "the value".to_string(), |path| format!("`{path}`"));
        format!("expected JSON ({what}: {e}); pass inline JSON or @file.json")
    })
}

fn kebab(s: &str) -> String {
    s.replace('_', "-")
}

fn properties(e: &Entry) -> Vec<(String, Value, bool)> {
    let required: Vec<&str> =
        e.input_schema["required"].as_array().map(|r| r.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    e.input_schema["properties"]
        .as_object()
        .map(|props| props.iter().map(|(k, v)| (k.clone(), v.clone(), required.contains(&k.as_str()))).collect())
        .unwrap_or_default()
}

/// The clap subcommand for one registry entry.
pub fn entry_command(e: &Entry) -> ClapCommand {
    let mut cmd = ClapCommand::new(e.action().to_string()).about(e.summary.to_string());
    if e.action().contains('_') {
        cmd = cmd.visible_alias(e.action().replace('_', "-"));
    }
    for (name, schema, required) in properties(e) {
        let kind = ArgKind::of(&schema);
        let help = schema.get("description").and_then(Value::as_str).unwrap_or("").to_string();
        let mut arg = Arg::new(name.clone()).help(help).required(required);
        let positional = e.positional.iter().position(|p| *p == name);
        match positional {
            Some(i) => arg = arg.index(i + 1).value_name(name.to_uppercase()),
            None => arg = arg.long(kebab(&name)),
        }
        arg = match &kind {
            ArgKind::Bool => arg.num_args(0..=1).default_missing_value("true").value_name("BOOL"),
            ArgKind::String { choices } if !choices.is_empty() => {
                arg.value_parser(clap::builder::PossibleValuesParser::new(choices.clone()))
            }
            ArgKind::Array(_) => arg.action(ArgAction::Append).value_delimiter(',').value_name("A,B,..."),
            ArgKind::Map => arg.action(ArgAction::Append).value_name("KEY=VALUE"),
            ArgKind::Tuple(2) => arg.value_name("X,Y"),
            ArgKind::Tuple(_) => arg.value_name("A,B,..."),
            ArgKind::Json => arg.value_name("JSON|@FILE"),
            _ => arg,
        };
        cmd = cmd.arg(arg);
    }
    cmd
}

/// Global options shared by every subcommand.
pub fn global_args(cmd: ClapCommand) -> ClapCommand {
    const HEADING: &str = "Global options";
    cmd.arg(
        Arg::new("project")
            .help_heading(HEADING)
            .short('p')
            .long("project")
            .global(true)
            .value_name("DIR")
            .help("Project directory (default: search upward from the current directory for cadlab.toml)"),
    )
    .arg(
        Arg::new("json")
            .help_heading(HEADING)
            .long("json")
            .global(true)
            .action(ArgAction::SetTrue)
            .help("Machine-readable JSON output on stdout"),
    )
    .arg(
        Arg::new("dry_run")
            .help_heading(HEADING)
            .long("dry-run")
            .global(true)
            .action(ArgAction::SetTrue)
            .help("Run, report the result, then roll back"),
    )
    .arg(
        Arg::new("quiet")
            .help_heading(HEADING)
            .short('q')
            .long("quiet")
            .global(true)
            .action(ArgAction::SetTrue)
            .help("Only print errors"),
    )
}

/// Top-level shortcuts for the checks agents run most (`cadlab erc` was the first thing tried).
pub const ALIASES: &[(&str, &str)] = &[("erc", "circuit.erc"), ("lint", "circuit.lint")];

/// The full clap tree.
pub fn build(registry: &Registry) -> ClapCommand {
    let mut root = ClapCommand::new("cadlab")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Headless electronics CAD: parts, circuits, boards, routing, fab outputs")
        .subcommand_required(true)
        .arg_required_else_help(true);
    root = global_args(root);
    for (group, entries) in registry.groups() {
        let about = cadlab::commands::group_summary(group).map_or_else(|| format!("{group} commands"), String::from);
        let mut g = ClapCommand::new(group.to_string()).about(about);
        // `cadlab history` alone lists history; `cadlab drc` alone runs the check.
        if group == "history" || group == "drc" {
            g = g.args_conflicts_with_subcommands(true);
            if let Some(run) = entries.iter().find(|e| e.name == "drc.run") {
                for a in entry_command(run).get_arguments() {
                    g = g.arg(a.clone());
                }
            }
        } else {
            g = g.subcommand_required(true).arg_required_else_help(true);
        }
        for e in entries {
            g = g.subcommand(entry_command(e));
        }
        root = root.subcommand(g);
    }
    for (alias, target) in ALIASES {
        if let Some(e) = registry.get(target) {
            let action = target.replace('.', " ").replace('_', "-");
            root = root.subcommand(entry_command(e).name(*alias).about(format!("{} (alias of `{action}`)", e.summary)));
        }
    }
    root.subcommand(ClapCommand::new("undo").about("Undo the last change (alias of `history undo`)").arg(steps_arg()))
        .subcommand(ClapCommand::new("redo").about("Redo an undone change (alias of `history redo`)").arg(steps_arg()))
        .subcommand(
            ClapCommand::new("call")
                .about("Run any command with JSON arguments")
                .arg(Arg::new("command").required(true).help("Command name, e.g. project.set"))
                .arg(Arg::new("args").help("Arguments as a JSON object, or - to read stdin (default: {})")),
        )
        .subcommand(
            ClapCommand::new("batch")
                .about("Run commands from a JSONL file as one transaction")
                .long_about(
                    "Run commands from a file as one all-or-nothing transaction (one undo step).\n\
                     Each line is {\"cmd\": \"...\", \"args\": {...}}; blank lines and lines starting with # are ignored.\n\
                     A JSON array of such objects is also accepted.",
                )
                .arg(Arg::new("file").required(true).help("Batch file, or - for stdin")),
        )
        .subcommand(
            ClapCommand::new("describe")
                .about("List commands, or show one command's arguments and schema")
                .arg(Arg::new("command").help("Command name (e.g. project.new) or group (e.g. project)")),
        )
        .subcommand(crate::settings::command())
        .subcommand(
            ClapCommand::new("mcp")
                .about("Run the MCP server on stdio")
                .arg(
                    Arg::new("no_autosave")
                        .long("no-autosave")
                        .action(ArgAction::SetTrue)
                        .help("Keep changes in memory until project.save is called"),
                ),
        )
}

fn steps_arg() -> Arg {
    Arg::new("steps").long("steps").value_parser(clap::value_parser!(u32)).help("Number of steps")
}

/// Converts the matches of a generated subcommand back to JSON arguments.
pub fn to_args(e: &Entry, m: &ArgMatches) -> Result<Value, String> {
    let mut out = Map::new();
    for (name, schema, _) in properties(e) {
        let kind = ArgKind::of(&schema);
        let Some(raw) = m.get_many::<String>(&name) else {
            continue;
        };
        let raw: Vec<&String> = raw.collect();
        let flag =
            if e.positional.contains(&name.as_str()) { name.to_uppercase() } else { format!("--{}", kebab(&name)) };
        let err = |msg: String| format!("{flag}: {msg}");
        let v = match &kind {
            ArgKind::Array(item) => {
                if raw.len() == 1 && raw[0].is_empty() {
                    Value::Array(vec![])
                } else {
                    Value::Array(raw.iter().map(|r| item.convert(r)).collect::<Result<_, _>>().map_err(err)?)
                }
            }
            ArgKind::Map => {
                let item = ArgKind::of(&schema["additionalProperties"]);
                let mut map = Map::new();
                for r in raw {
                    if r.trim_start().starts_with('{') {
                        let Value::Object(o) = serde_json::from_str(r).map_err(|e| err(e.to_string()))? else {
                            return Err(err("expected a JSON object".into()));
                        };
                        map.extend(o);
                        continue;
                    }
                    let (k, v) = r.split_once('=').ok_or_else(|| err(format!("expected KEY=VALUE, got `{r}`")))?;
                    let v = if v.is_empty() { Value::Null } else { item.convert(v).map_err(err)? };
                    map.insert(k.to_string(), v);
                }
                Value::Object(map)
            }
            k => k.convert(raw[raw.len() - 1]).map_err(err)?,
        };
        out.insert(name, v);
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kinds() {
        assert_eq!(ArgKind::of(&json!({"type": "boolean"})), ArgKind::Bool);
        assert_eq!(ArgKind::of(&json!({"type": ["string", "null"]})), ArgKind::String { choices: vec![] });
        assert_eq!(
            ArgKind::of(&json!({"type": ["array", "null"], "items": {"type": "string"}})),
            ArgKind::Array(Box::new(ArgKind::String { choices: vec![] }))
        );
        assert_eq!(
            ArgKind::of(&json!({"anyOf": [{"type": "string", "enum": ["mm", "mil"]}, {"type": "null"}]})),
            ArgKind::String { choices: vec!["mm".into(), "mil".into()] }
        );
        assert_eq!(
            ArgKind::of(&json!({"type": "object", "additionalProperties": {"type": ["string", "null"]}})),
            ArgKind::Map
        );
        assert_eq!(ArgKind::of(&json!({"type": "object", "properties": {}})), ArgKind::Json);
    }

    #[test]
    fn every_command_builds() {
        let cmd = build(cadlab::registry());
        cmd.debug_assert();
        // No command argument may shadow a global option.
        for e in cadlab::registry().iter() {
            for (name, _, _) in properties(e) {
                assert!(
                    !["project", "json", "dry_run", "quiet"].contains(&name.as_str()),
                    "{}: `{name}` clashes with a global option",
                    e.name
                );
            }
        }
    }
}
