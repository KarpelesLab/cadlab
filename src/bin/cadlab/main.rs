//! The `cadlab` binary: a CLI and an MCP server over the same command registry.

mod cli;
mod mcp;

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use cadlab::command::{Failure, Outcome, Registry, RunOptions, Session, Step};
use cadlab::model::find_project_root;
use cadlab::{Diagnostic, Severity};
use clap::ArgMatches;
use serde_json::{Value, json};

/// Exit codes (docs/INTERFACES.md).
mod exit {
    pub const OK: u8 = 0;
    pub const COMMAND_ERROR: u8 = 1;
    pub const USAGE: u8 = 2;
    pub const CHECKS_FAILED: u8 = 3;
}

struct Globals {
    project: Option<PathBuf>,
    json: bool,
    dry_run: bool,
    quiet: bool,
}

fn main() -> ExitCode {
    let registry = cadlab::registry();
    let matches = match cli::build(registry).try_get_matches() {
        Ok(m) => m,
        Err(e) => {
            let _ = e.print();
            return ExitCode::from(if e.use_stderr() { exit::USAGE } else { exit::OK });
        }
    };
    let g = Globals {
        project: matches.get_one::<String>("project").map(PathBuf::from),
        json: matches.get_flag("json"),
        dry_run: matches.get_flag("dry_run"),
        quiet: matches.get_flag("quiet"),
    };
    ExitCode::from(dispatch(registry, &g, &matches))
}

fn dispatch(registry: &'static Registry, g: &Globals, m: &ArgMatches) -> u8 {
    let (sub, sm) = m.subcommand().expect("subcommand required");
    match sub {
        "mcp" => mcp::serve(registry, !sm.get_flag("no_autosave")),
        "describe" => describe(registry, g, sm.get_one::<String>("command").map(String::as_str)),
        "call" => {
            let name = sm.get_one::<String>("command").unwrap();
            let args = match sm.get_one::<String>("args").map(String::as_str) {
                None => Ok(json!({})),
                Some("-") => read_stdin().and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string())),
                Some(s) => serde_json::from_str(s).map_err(|e| e.to_string()),
            };
            match args {
                Ok(args) => run_one(registry, g, name, args),
                Err(e) => usage_error(g, &format!("invalid JSON arguments: {e}")),
            }
        }
        "batch" => {
            let file = sm.get_one::<String>("file").unwrap();
            let text = if file == "-" {
                read_stdin()
            } else {
                std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))
            };
            match text.and_then(|t| parse_batch(&t)) {
                Ok(steps) => run_batch(registry, g, steps),
                Err(e) => usage_error(g, &e),
            }
        }
        "undo" | "redo" => {
            let steps = sm.get_one::<u32>("steps").copied().unwrap_or(1);
            run_one(registry, g, &format!("history.{sub}"), json!({ "steps": steps }))
        }
        group => {
            let Some((action, am)) = sm.subcommand() else {
                // Only `history` allows no action.
                return run_one(registry, g, "history.list", json!({}));
            };
            let name = format!("{group}.{action}");
            let entry = registry.get(&name).expect("generated from the registry");
            match cli::to_args(entry, am) {
                Ok(args) => run_one(registry, g, &name, args),
                Err(e) => usage_error(g, &e),
            }
        }
    }
}

/// Opens the session a command runs in: the project from `-p` or found upward from the current
/// directory. Commands that create or open projects start from an empty session.
fn open_session(g: &Globals, command: &str) -> Result<(Session, Vec<Diagnostic>), Failure> {
    if matches!(command, "project.new" | "project.open") {
        return Ok((Session::new(), vec![]));
    }
    let root = match &g.project {
        Some(p) => Some(p.clone()),
        None => std::env::current_dir().ok().and_then(|d| find_project_root(&d)),
    };
    match root {
        Some(r) => Session::open(&r).map_err(|error| Failure {
            command: command.into(),
            step: None,
            error,
            diagnostics: vec![],
        }),
        None => Ok((Session::new(), vec![])),
    }
}

fn run_one(registry: &Registry, g: &Globals, name: &str, args: Value) -> u8 {
    let (mut session, mut warnings) = match open_session(g, name) {
        Ok(s) => s,
        Err(f) => return report_failure(g, &f),
    };
    match registry.execute(&mut session, name, args, RunOptions { dry_run: g.dry_run }) {
        Ok(mut o) => {
            if let Err(f) = autosave(&mut session, name) {
                return report_failure(g, &f);
            }
            warnings.append(&mut o.diagnostics);
            o.diagnostics = warnings;
            report_outcome(g, &o)
        }
        Err(mut f) => {
            warnings.append(&mut f.diagnostics);
            f.diagnostics = warnings;
            report_failure(g, &f)
        }
    }
}

fn run_batch(registry: &Registry, g: &Globals, steps: Vec<Step>) -> u8 {
    let (mut session, warnings) = match open_session(g, "batch") {
        Ok(s) => s,
        Err(f) => return report_failure(g, &f),
    };
    match registry.execute_batch(&mut session, steps, RunOptions { dry_run: g.dry_run }) {
        Ok(outs) => {
            if let Err(f) = autosave(&mut session, "batch") {
                return report_failure(g, &f);
            }
            let has_errors = outs
                .iter()
                .flat_map(|o| &o.diagnostics)
                .chain(&warnings)
                .any(|d| d.severity == Severity::Error);
            if g.json {
                println!("{}", json!({ "ok": true, "results": outs, "diagnostics": warnings }));
            } else {
                print_diagnostics(&warnings);
                for o in &outs {
                    if !g.quiet {
                        println!("{}", o.summary);
                    }
                    print_diagnostics(&o.diagnostics);
                }
            }
            if has_errors { exit::CHECKS_FAILED } else { exit::OK }
        }
        Err(f) => report_failure(g, &f),
    }
}

fn autosave(session: &mut Session, command: &str) -> Result<(), Failure> {
    if session.is_dirty() {
        session.save().map_err(|error| Failure {
            command: command.into(),
            step: None,
            error,
            diagnostics: vec![],
        })?;
    }
    Ok(())
}

fn report_outcome(g: &Globals, o: &Outcome) -> u8 {
    if g.json {
        println!(
            "{}",
            json!({ "ok": true, "command": o.command, "output": o.output, "diagnostics": o.diagnostics })
        );
    } else {
        if !g.quiet {
            println!("{}", o.summary);
        }
        print_diagnostics(&o.diagnostics);
    }
    if o.diagnostics.iter().any(|d| d.severity == Severity::Error) {
        exit::CHECKS_FAILED
    } else {
        exit::OK
    }
}

fn report_failure(g: &Globals, f: &Failure) -> u8 {
    if g.json {
        let mut v = json!({ "ok": false, "command": f.command, "error": f.error, "diagnostics": f.diagnostics });
        if let Some(i) = f.step {
            v["step"] = json!(i);
        }
        println!("{v}");
    } else {
        print_diagnostics(&f.diagnostics);
        match f.step {
            Some(i) => eprintln!("batch step {}: {}", i + 1, f.error),
            None => eprintln!("{}", f.error),
        }
    }
    exit::COMMAND_ERROR
}

fn usage_error(g: &Globals, msg: &str) -> u8 {
    if g.json {
        println!(
            "{}",
            json!({ "ok": false, "error": { "kind": "invalid_args", "severity": "error", "code": "usage", "message": msg } })
        );
    } else {
        eprintln!("error: {msg}");
    }
    exit::USAGE
}

fn print_diagnostics(ds: &[Diagnostic]) {
    for d in ds {
        eprintln!("{d}");
    }
}

fn read_stdin() -> Result<String, String> {
    let mut s = String::new();
    if std::io::stdin().is_terminal() {
        eprintln!("(reading from stdin; end with Ctrl-D)");
    }
    std::io::stdin().read_to_string(&mut s).map_err(|e| e.to_string())?;
    Ok(s)
}

/// Parses a batch: JSONL (one step per line, `#` comments) or a JSON array.
fn parse_batch(text: &str) -> Result<Vec<Step>, String> {
    if text.trim_start().starts_with('[') {
        return serde_json::from_str(text).map_err(|e| format!("invalid batch: {e}"));
    }
    let mut steps = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        steps.push(serde_json::from_str(l).map_err(|e| format!("line {}: {e}", i + 1))?);
    }
    Ok(steps)
}

fn describe(registry: &Registry, g: &Globals, name: Option<&str>) -> u8 {
    let Some(name) = name else {
        let list: Vec<Value> = registry
            .iter()
            .map(|e| json!({"name": e.name, "kind": e.kind, "summary": e.summary}))
            .collect();
        if g.json {
            println!("{}", Value::Array(list));
        } else {
            let w = registry.iter().map(|e| e.name.len()).max().unwrap_or(0);
            for e in registry.iter() {
                println!("{:w$}  {}", e.name, e.summary);
            }
        }
        return exit::OK;
    };
    let entries: Vec<_> = match registry.get(name) {
        Some(e) => vec![e],
        None => registry.iter().filter(|e| e.group() == name).collect(),
    };
    if entries.is_empty() {
        let names: Vec<&str> = registry
            .iter()
            .map(|e| e.name)
            .chain(registry.groups().keys().copied())
            .collect();
        let s = cadlab::suggest::did_you_mean(name, names, 3);
        let d = Diagnostic::error("command.unknown", format!("unknown command or group `{name}`")).with_suggestions(&s);
        if g.json {
            println!("{}", json!({"ok": false, "error": d}));
        } else {
            eprintln!("{d}");
        }
        return exit::COMMAND_ERROR;
    }
    if g.json {
        let v: Vec<Value> = entries.iter().map(|e| e.describe()).collect();
        println!("{}", if v.len() == 1 { v[0].clone() } else { Value::Array(v) });
    } else {
        for e in entries {
            println!("{}\n", describe_text(e));
        }
    }
    exit::OK
}

/// Human-readable description of a command's arguments.
pub fn describe_text(e: &cadlab::command::Entry) -> String {
    let mut s = format!(
        "{} ({:?}): {}\n  cli: cadlab {} {}",
        e.name,
        e.kind,
        e.summary,
        e.group(),
        e.action()
    );
    for p in e.positional {
        s += &format!(" <{p}>");
    }
    let required: Vec<&str> = e.input_schema["required"]
        .as_array()
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if let Some(props) = e.input_schema["properties"].as_object() {
        if !props.is_empty() {
            s += "\n  arguments:";
        }
        for (k, v) in props {
            let req = if required.contains(&k.as_str()) {
                " (required)"
            } else {
                ""
            };
            let desc = v.get("description").and_then(Value::as_str).unwrap_or("");
            s += &format!("\n    {k}: {}{req}  {desc}", type_label(v));
        }
    }
    s
}

/// Compact type label for a property schema: `string`, `string[]`, `mm|mil|...`.
pub fn type_label(v: &Value) -> String {
    if let Some(variants) = v.get("anyOf").or_else(|| v.get("oneOf")).and_then(Value::as_array) {
        let variants: Vec<&Value> = variants
            .iter()
            .filter(|x| x.get("type") != Some(&json!("null")))
            .collect();
        // Tagged unions: name the tag and its values (`{family: chip|qfn|..., ...}`).
        let tag = variants
            .first()
            .and_then(|f| f["properties"].as_object())
            .and_then(|props| {
                props
                    .keys()
                    .find(|k| {
                        variants
                            .iter()
                            .all(|x| x["properties"][k.as_str()].get("const").is_some())
                    })
                    .cloned()
            });
        if let Some(tag) = tag.filter(|_| variants.len() > 1) {
            let values: Vec<&str> = variants
                .iter()
                .filter_map(|x| x["properties"][tag.as_str()]["const"].as_str())
                .collect();
            return format!("{{{tag}: {}, ...}}", values.join("|"));
        }
        let parts: Vec<String> = variants.into_iter().map(type_label).collect();
        return parts.join("|");
    }
    if let Some(c) = v.get("const") {
        return c.as_str().map_or_else(|| c.to_string(), String::from);
    }
    if let Some(e) = v.get("enum").and_then(Value::as_array) {
        return e.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("|");
    }
    let t: Vec<&str> = match v.get("type") {
        Some(Value::String(t)) => vec![t],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).filter(|t| *t != "null").collect(),
        _ => vec!["any"],
    };
    match t.as_slice() {
        ["array"] => format!("{}[]", v.get("items").map(type_label).unwrap_or_else(|| "any".into())),
        ["object"] => match (v.get("additionalProperties"), v["properties"].as_object()) {
            (Some(a), _) if a.is_object() => format!("map<string, {}>", type_label(a)),
            (_, Some(props)) if !props.is_empty() => {
                let required: Vec<&str> = v["required"]
                    .as_array()
                    .map(|r| r.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                let fields: Vec<String> = props
                    .keys()
                    .map(|k| {
                        if required.contains(&k.as_str()) {
                            k.clone()
                        } else {
                            format!("{k}?")
                        }
                    })
                    .collect();
                format!("{{{}}}", fields.join(", "))
            }
            _ => "object".into(),
        },
        ts => ts.join("|"),
    }
}
