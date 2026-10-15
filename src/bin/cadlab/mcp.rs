//! MCP server over stdio (newline-delimited JSON-RPC 2.0).
//!
//! A small in-house implementation (DECISIONS D14): synchronous, no async runtime. A reader
//! thread parses incoming messages and handles cancellation; the main thread runs requests in
//! order. Tools are generated from the registry: one tool per command group, plus `describe`,
//! `call` and `batch`.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use cadlab::Diagnostic;
use cadlab::command::{CancelToken, Failure, Outcome, Progress, Registry, RunOptions, Session, Step};
use cadlab::supplier::Suppliers;
use serde_json::{Value, json};

/// Protocol revisions this server speaks, newest first.
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "cadlab is a headless electronics CAD tool. Start by creating or opening a project \
with the `project` tool (action `new` or `open`); later calls act on that project unless `project` names another \
directory. Each tool groups related actions: pass `action` and its `args`. Use `describe` to get the full argument \
schema of any action. All lengths need units (\"0.2mm\", \"8mil\"). Use `dry_run: true` to preview a change, \
`history` to undo. Errors include a `hint` on how to fix them; hints name commands as `group.action {json args}`: \
call the tool `group` with that `action` and those `args` (or `call`). Long check results take `codes`, `severity`, \
`limit` and `offset` to narrow them.";

/// Groups whose tool runs a default action when `action` is left out (as the CLI does).
const DEFAULT_ACTIONS: &[(&str, &str)] = &[("history", "list"), ("drc", "run")];

type Writer = Arc<Mutex<std::io::Stdout>>;

fn send(out: &Writer, msg: &Value) {
    let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
    let _ = writeln!(o, "{msg}");
    let _ = o.flush();
}

/// Message passed from the reader thread to the main loop.
struct Incoming {
    msg: Value,
    cancel: CancelToken,
}

/// Runs the server until stdin closes. Returns the process exit code.
pub fn serve(registry: &'static Registry, autosave: bool) -> u8 {
    let out: Writer = Arc::new(Mutex::new(std::io::stdout()));
    let tokens: Arc<Mutex<HashMap<String, CancelToken>>> = Arc::default();
    let (tx, rx) = mpsc::channel::<Incoming>();

    {
        let out = out.clone();
        let tokens = tokens.clone();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() {
                    continue;
                }
                let msg: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        send(&out, &error_response(Value::Null, -32700, &format!("parse error: {e}")));
                        continue;
                    }
                };
                if msg.get("method").and_then(Value::as_str) == Some("notifications/cancelled") {
                    let id = msg.pointer("/params/requestId").map(Value::to_string).unwrap_or_default();
                    if let Some(t) = tokens.lock().unwrap_or_else(|e| e.into_inner()).get(&id) {
                        t.cancel();
                    }
                    continue;
                }
                let cancel = CancelToken::new();
                if let Some(id) = msg.get("id") {
                    tokens.lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_string(), cancel.clone());
                }
                if tx.send(Incoming { msg, cancel }).is_err() {
                    break;
                }
            }
        });
    }

    let mut server = Server {
        registry,
        autosave,
        sessions: Vec::new(),
        current: None,
        out: out.clone(),
        suppliers: Suppliers::from_env(),
    };
    for Incoming { msg, cancel } in rx {
        let id = msg.get("id").cloned();
        let response = server.handle(&msg, &cancel);
        if let Some(id) = id {
            tokens.lock().unwrap_or_else(|e| e.into_inner()).remove(&id.to_string());
            if let Some(r) = response {
                send(&out, &r);
            }
        }
    }
    0
}

struct Server {
    registry: &'static Registry,
    autosave: bool,
    /// Open projects, by absolute directory.
    sessions: Vec<(PathBuf, Session)>,
    current: Option<PathBuf>,
    out: Writer,
    suppliers: Suppliers,
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn result_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// Sends `notifications/progress` for a request that asked for it.
struct McpProgress {
    out: Writer,
    token: Option<Value>,
}

impl Progress for McpProgress {
    fn report(&self, done: u64, total: Option<u64>, message: &str) {
        if let Some(t) = &self.token {
            let mut p = json!({"progressToken": t, "progress": done, "message": message});
            if let Some(total) = total {
                p["total"] = json!(total);
            }
            send(&self.out, &json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": p}));
        }
    }
}

impl Server {
    fn new_session(&self) -> Session {
        let mut s = Session::new();
        s.suppliers = self.suppliers.clone();
        s
    }

    fn handle(&mut self, msg: &Value, cancel: &CancelToken) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str);
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let Some(method) = method else {
            // A response to a request we never sent, or garbage.
            return id.map(|id| error_response(id, -32600, "invalid request: missing method"));
        };
        let id = id?; // Other notifications (initialized, ...) need no answer.
        let r = match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
                let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).unwrap_or(&PROTOCOL_VERSIONS[0]);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "cadlab", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": self.tools()})),
            "tools/call" => self.call_tool(&params, cancel),
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        Some(match r {
            Ok(v) => result_response(id, v),
            Err((code, m)) => error_response(id, code, &m),
        })
    }

    fn tools(&self) -> Vec<Value> {
        let project_prop = json!({"type": "string", "description": "Project directory to act on (default: the most recently opened or created project)"});
        let dry_run_prop = json!({"type": "boolean", "description": "Run, report the result, then roll back"});
        let mut tools = Vec::new();
        for (group, entries) in self.registry.groups() {
            let mut desc = match cadlab::commands::group_summary(group) {
                Some(summary) => format!("{summary}. Actions:"),
                None => format!("{group} commands. Actions:"),
            };
            for e in &entries {
                desc += &format!("\n- {}: {}. args: {}", e.action(), e.summary, args_signature(&e.input_schema));
            }
            if let Some((_, a)) = DEFAULT_ACTIONS.iter().find(|(g, _)| *g == group) {
                desc += &format!("\nWithout `action`: `{a}`.");
            }
            desc += "\nCall `describe` with \"<group>.<action>\" for the full schema.";
            let actions: Vec<&str> = entries.iter().map(|e| e.action()).collect();
            let mut tool = json!({
                "name": group,
                "description": desc,
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "action": {"type": "string", "enum": actions, "description": "Action to run, from the list in the description"},
                        "args": {"type": "object", "description": "Arguments of the action, as listed in the description"},
                        "project": project_prop,
                        "dry_run": dry_run_prop,
                    },
                },
            });
            if !DEFAULT_ACTIONS.iter().any(|(g, _)| *g == group) {
                tool["inputSchema"]["required"] = json!(["action"]);
            }
            tools.push(tool);
        }
        tools.push(json!({
            "name": "describe",
            "description": "List all commands, or return the full argument JSON Schema of one command (\"project.new\") or group (\"project\").",
            "inputSchema": {"type": "object", "properties": {
                "command": {"type": "string", "description": "Command (\"group.action\") or group to describe; leave out to list every command"},
                "output_schema": {"type": "boolean", "description": "Also return the JSON Schema of the command's output (large; default false)"},
            }},
        }));
        tools.push(json!({
            "name": "call",
            "description": "Run any command by its full name (\"group.action\") with JSON arguments.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Full command name, \"group.action\" (e.g. \"net.connect\")"},
                    "args": {"type": "object", "description": "Arguments of the command (`describe` gives its schema)"},
                    "project": project_prop,
                    "dry_run": dry_run_prop,
                },
                "required": ["command"],
            },
        }));
        tools.push(json!({
            "name": "batch",
            "description": "Run several commands as one all-or-nothing transaction (a single undo step). Session commands (project.new/open/save, history.*) are not allowed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "steps": {
                        "type": "array",
                        "description": "Commands to run in order",
                        "items": {"type": "object", "properties": {
                            "cmd": {"type": "string", "description": "Full command name, \"group.action\""},
                            "args": {"type": "object", "description": "Arguments of the command"},
                        }, "required": ["cmd"]},
                    },
                    "project": project_prop,
                    "dry_run": dry_run_prop,
                },
                "required": ["steps"],
            },
        }));
        tools
    }

    fn call_tool(&mut self, params: &Value, cancel: &CancelToken) -> Result<Value, (i64, String)> {
        let name = params.get("name").and_then(Value::as_str).ok_or((-32602, "missing tool name".to_string()))?;
        if let Some(a) = params.get("arguments")
            && !a.is_object()
            && !a.is_null()
        {
            return Ok(tool_error(
                "mcp.invalid_arguments",
                "tool arguments must be a JSON object",
                "pass {\"action\": ..., \"args\": {...}}",
            ));
        }
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let project = args.get("project").and_then(Value::as_str).map(PathBuf::from);
        let opts = RunOptions { dry_run: args.get("dry_run").and_then(Value::as_bool).unwrap_or(false) };
        let progress = McpProgress { out: self.out.clone(), token: params.pointer("/_meta/progressToken").cloned() };
        let cmd_args = args.get("args").cloned().unwrap_or(json!({}));
        match name {
            "describe" => Ok(self.describe(
                args.get("command").and_then(Value::as_str),
                args.get("output_schema").and_then(Value::as_bool).unwrap_or(false),
            )),
            "call" => {
                let Some(cmd) = args.get("command").and_then(Value::as_str) else {
                    return Ok(tool_error(
                        "mcp.missing_command",
                        "missing `command`",
                        "name the command as \"group.action\" (`describe` without arguments lists them)",
                    ));
                };
                Ok(self.run(cmd, cmd_args, project.as_deref(), opts, &progress, cancel))
            }
            "batch" => {
                let steps: Vec<Step> = match serde_json::from_value(args.get("steps").cloned().unwrap_or(json!([]))) {
                    Ok(s) => s,
                    Err(e) => {
                        return Ok(tool_error(
                            "args.invalid",
                            &format!("invalid steps: {e}"),
                            "`steps` is a list of {\"cmd\": \"group.action\", \"args\": {...}}",
                        ));
                    }
                };
                Ok(self.run_batch(steps, project.as_deref(), opts))
            }
            group if self.registry.groups().contains_key(group) => {
                let action = match args.get("action").and_then(Value::as_str) {
                    Some(a) => a,
                    None => match DEFAULT_ACTIONS.iter().find(|(g, _)| *g == group) {
                        Some((_, a)) => a,
                        None => {
                            let actions: Vec<&str> = self.registry.groups()[group].iter().map(|e| e.action()).collect();
                            return Ok(tool_error(
                                "mcp.missing_action",
                                &format!("missing `action` for the `{group}` tool"),
                                &format!("pass one of: {}", actions.join(", ")),
                            ));
                        }
                    },
                };
                // `{"action": "board.info"}` or `{"action": "info"}` both mean `board.info`.
                let action = action.strip_prefix(&format!("{group}.")).unwrap_or(action);
                let cmd = format!("{group}.{action}");
                Ok(self.run(&cmd, cmd_args, project.as_deref(), opts, &progress, cancel))
            }
            _ => {
                let names: Vec<&str> =
                    self.registry.groups().keys().copied().chain(["describe", "call", "batch"]).collect();
                let s = cadlab::suggest::did_you_mean(name, names, 3);
                let mut m = format!("unknown tool: {name}");
                if !s.is_empty() {
                    m += &format!(
                        " (did you mean {}?)",
                        s.iter().map(|x| format!("`{x}`")).collect::<Vec<_>>().join(", ")
                    );
                }
                Err((-32602, m))
            }
        }
    }

    fn describe(&self, name: Option<&str>, output_schema: bool) -> Value {
        let Some(name) = name else {
            let list: Vec<Value> =
                self.registry.iter().map(|e| json!({"name": e.name, "kind": e.kind, "summary": e.summary})).collect();
            let text =
                self.registry.iter().map(|e| format!("{}: {}", e.name, e.summary)).collect::<Vec<_>>().join("\n");
            return tool_ok(text, json!({"commands": list}));
        };
        let entries: Vec<_> = match self.registry.get(name) {
            Some(e) => vec![e],
            None => self.registry.iter().filter(|e| e.group() == name).collect(),
        };
        if entries.is_empty() {
            if ["describe", "call", "batch"].contains(&name) {
                return tool_error(
                    "describe.meta_tool",
                    &format!("`{name}` is a tool of this server, not a cadlab command"),
                    "its input schema is in the tools list (tools/list)",
                );
            }
            let names: Vec<&str> =
                self.registry.iter().map(|e| e.name).chain(self.registry.groups().keys().copied()).collect();
            let s = cadlab::suggest::did_you_mean(name, names, 3);
            let d =
                Diagnostic::error("command.unknown", format!("unknown command or group `{name}`")).with_suggestions(&s);
            let hint = d.hint.clone().unwrap_or_else(|| "call `describe` without `command` to list them".into());
            return tool_error("command.unknown", &d.message, &hint);
        }
        let text = entries
            .iter()
            .map(|e| {
                let mut t = crate::describe_text(e).replacen(
                    &format!("\n  cli: cadlab {} {}", e.group(), e.action()),
                    &format!(
                        "\n  tool `{}`, action `{}`; cli: cadlab {} {}",
                        e.group(),
                        e.action(),
                        e.group(),
                        e.action()
                    ),
                    1,
                );
                if !output_schema {
                    t += &format!("\n  returns: {}", args_signature(&e.output_schema));
                }
                t
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let described: Vec<Value> = entries
            .iter()
            .map(|e| {
                let mut d = e.describe();
                if !output_schema && let Some(o) = d.as_object_mut() {
                    o.remove("output_schema");
                }
                d
            })
            .collect();
        tool_ok(text, json!({"commands": described}))
    }

    /// Finds or opens the session for `project` (or the current one).
    fn session_for(&mut self, project: Option<&Path>) -> Result<usize, Failure> {
        let path = match project {
            Some(p) => std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()),
            None => match &self.current {
                Some(c) => c.clone(),
                None => {
                    // No project yet: an empty session, so the command reports `project.none`.
                    self.sessions.push((PathBuf::new(), self.new_session()));
                    return Ok(self.sessions.len() - 1);
                }
            },
        };
        if let Some(i) = self.sessions.iter().position(|(p, _)| *p == path) {
            return Ok(i);
        }
        let (mut s, _) = Session::open(&path).map_err(|error| Failure {
            command: "project.open".into(),
            step: None,
            error,
            diagnostics: vec![],
        })?;
        s.suppliers = self.suppliers.clone();
        self.sessions.push((path, s));
        Ok(self.sessions.len() - 1)
    }

    fn drop_empty_sessions(&mut self) {
        self.sessions.retain(|(p, _)| !p.as_os_str().is_empty());
    }

    fn run(
        &mut self,
        cmd: &str,
        args: Value,
        project: Option<&Path>,
        opts: RunOptions,
        progress: &McpProgress,
        cancel: &CancelToken,
    ) -> Value {
        // Creating or opening a project starts a fresh session, which becomes current.
        if cmd == "project.new" || cmd == "project.open" {
            if cmd == "project.open"
                && let Some(p) = args.get("path").and_then(Value::as_str)
            {
                let abs = std::path::absolute(p).unwrap_or_else(|_| PathBuf::from(p));
                if self.sessions.iter().any(|(sp, _)| *sp == abs) {
                    // Already open (possibly with unsaved changes): switch to it.
                    self.current = Some(abs.clone());
                    return self.run("project.info", json!({}), Some(&abs), opts, progress, cancel);
                }
            }
            let mut s = self.new_session();
            let r = self.registry.execute_with(&mut s, cmd, args, opts, progress, cancel);
            let r = self.mcp_hints(r);
            return match r {
                Ok(o) => {
                    let root = s.root().map(Path::to_path_buf).unwrap_or_default();
                    if let Err(f) = self.maybe_save(&mut s) {
                        return failure_result(&f);
                    }
                    self.sessions.retain(|(p, _)| *p != root);
                    self.sessions.push((root.clone(), s));
                    self.current = Some(root);
                    outcome_result(&o)
                }
                Err(f) => failure_result(&f),
            };
        }
        let i = match self.session_for(project) {
            Ok(i) => i,
            Err(f) => return failure_result(&f),
        };
        let r = self.registry.execute_with(&mut self.sessions[i].1, cmd, args, opts, progress, cancel);
        let r = self.mcp_hints(r);
        let result = match r {
            Ok(o) => match self.maybe_save_at(i) {
                Ok(()) => outcome_result(&o),
                Err(f) => failure_result(&f),
            },
            Err(f) => failure_result(&f),
        };
        if project.is_some() && !self.sessions[i].0.as_os_str().is_empty() {
            self.current = Some(self.sessions[i].0.clone());
        }
        self.drop_empty_sessions();
        result
    }

    fn run_batch(&mut self, steps: Vec<Step>, project: Option<&Path>, opts: RunOptions) -> Value {
        let i = match self.session_for(project) {
            Ok(i) => i,
            Err(f) => return failure_result(&f),
        };
        let r = self.registry.execute_batch(&mut self.sessions[i].1, steps, opts);
        let r = match r {
            Ok(mut outs) => {
                for o in &mut outs {
                    hints_for_mcp(self.registry, &mut o.diagnostics);
                }
                Ok(outs)
            }
            Err(mut f) => {
                hints_for_mcp(self.registry, std::slice::from_mut(&mut *f.error.diagnostic));
                hints_for_mcp(self.registry, &mut f.diagnostics);
                Err(f)
            }
        };
        let result = match r {
            Ok(outs) => match self.maybe_save_at(i) {
                Ok(()) => {
                    let text = outs.iter().map(|o| text_of(&o.summary, &o.diagnostics)).collect::<Vec<_>>().join("\n");
                    tool_ok(text, json!({"ok": true, "results": outs}))
                }
                Err(f) => failure_result(&f),
            },
            Err(f) => failure_result(&f),
        };
        self.drop_empty_sessions();
        result
    }

    /// Rewrites the hints of a result for MCP clients (see [`hint_for_mcp`]).
    fn mcp_hints(&self, r: Result<Outcome, Failure>) -> Result<Outcome, Failure> {
        match r {
            Ok(mut o) => {
                hints_for_mcp(self.registry, &mut o.diagnostics);
                Ok(o)
            }
            Err(mut f) => {
                hints_for_mcp(self.registry, std::slice::from_mut(&mut *f.error.diagnostic));
                hints_for_mcp(self.registry, &mut f.diagnostics);
                Err(f)
            }
        }
    }

    fn maybe_save_at(&mut self, i: usize) -> Result<(), Failure> {
        let autosave = self.autosave;
        let s = &mut self.sessions[i].1;
        if autosave && s.is_dirty() {
            s.save().map_err(|error| Failure {
                command: "project.save".into(),
                step: None,
                error,
                diagnostics: vec![],
            })?;
        }
        Ok(())
    }

    fn maybe_save(&self, s: &mut Session) -> Result<(), Failure> {
        if self.autosave && s.is_dirty() {
            s.save().map_err(|error| Failure {
                command: "project.save".into(),
                step: None,
                error,
                diagnostics: vec![],
            })?;
        }
        Ok(())
    }
}

/// Compact argument signature from a schema: `{path: string, name?: string}`.
fn args_signature(schema: &Value) -> String {
    let required: Vec<&str> =
        schema["required"].as_array().map(|r| r.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let Some(props) = schema["properties"].as_object() else {
        return "{}".into();
    };
    let parts: Vec<String> = props
        .iter()
        .map(|(k, v)| {
            let opt = if required.contains(&k.as_str()) { "" } else { "?" };
            format!("{k}{opt}: {}", crate::type_label(v))
        })
        .collect();
    format!("{{{}}}", parts.join(", "))
}

fn text_of(summary: &str, diagnostics: &[Diagnostic]) -> String {
    let mut t = summary.to_string();
    for d in diagnostics {
        t += &format!("\n{d}");
    }
    t
}

fn tool_ok(text: String, structured: Value) -> Value {
    json!({"content": [{"type": "text", "text": text}], "structuredContent": structured, "isError": false})
}

/// An error of the server itself (not of a command), shaped like a command's error: kind,
/// code, message and hint.
fn tool_error(code: &str, message: &str, hint: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": format!("error[{code}]: {message}\n  hint: {hint}")}],
        "structuredContent": {"ok": false, "error": {
            "kind": "invalid_args", "severity": "error", "code": code, "message": message, "hint": hint,
        }},
        "isError": true,
    })
}

fn hints_for_mcp(registry: &Registry, ds: &mut [Diagnostic]) {
    for d in ds {
        if let Some(h) = &d.hint {
            d.hint = Some(hint_for_mcp(registry, h));
        }
    }
}

/// Hints name commands the way the CLI takes them (`net.set +5V --driven`). For MCP clients
/// each such span becomes the command with its JSON arguments (`net.set {"nets":["+5V"],
/// "driven":true}`), parsed with the CLI's own argument mapping; spans that do not parse stay
/// as they are.
pub(crate) fn hint_for_mcp(registry: &Registry, hint: &str) -> String {
    let mut out = String::new();
    let mut rest = hint;
    while let Some(a) = rest.find('`') {
        let Some(b) = rest[a + 1..].find('`') else { break };
        let span = &rest[a + 1..a + 1 + b];
        out += &rest[..a];
        out.push('`');
        out += &cli_span_to_json(registry, span).unwrap_or_else(|| span.to_string());
        out.push('`');
        rest = &rest[a + 2 + b..];
    }
    out + rest
}

fn cli_span_to_json(registry: &Registry, span: &str) -> Option<String> {
    let tokens = hint_tokens(span)?;
    let (name, args) = tokens.split_first()?;
    if args.is_empty() {
        return None;
    }
    let entry = registry.get(name)?;
    let cmd = crate::cli::entry_command(entry).no_binary_name(true).mut_args(|a| a.required(false));
    let m = cmd.try_get_matches_from(args).ok()?;
    let v = crate::cli::to_args(entry, &m).ok()?;
    if v.as_object().is_some_and(|o| o.is_empty()) {
        return None;
    }
    Some(format!("{name} {v}"))
}

/// Words of a hint span: whitespace-separated, with `"..."` and `<...>` kept whole (quotes
/// removed). None when quotes are unbalanced.
fn hint_tokens(span: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut angle = false;
    for ch in span.chars() {
        match ch {
            '"' if !angle => quoted = !quoted,
            '<' if !quoted => {
                angle = true;
                cur.push(ch);
            }
            '>' if !quoted && angle => {
                angle = false;
                cur.push(ch);
            }
            c if c.is_whitespace() && !quoted && !angle => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if quoted || angle {
        return None;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Some(out)
}

fn outcome_result(o: &Outcome) -> Value {
    let mut v = tool_ok(
        text_of(&o.summary, &o.diagnostics),
        json!({"ok": true, "command": o.command, "output": o.output, "diagnostics": o.diagnostics}),
    );
    // Rendered images are sent as image content so multimodal clients can look at them (every
    // sheet of a multi-sheet schematic).
    let pages: Vec<&str> =
        o.output.get("pages").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    let paths: Vec<&str> =
        if pages.is_empty() { o.output.get("png").and_then(Value::as_str).into_iter().collect() } else { pages };
    let mut total = 0;
    for path in paths.into_iter().filter(|p| p.ends_with(".png")) {
        let Ok(bytes) = std::fs::read(path) else { continue };
        total += bytes.len();
        if total > MAX_IMAGE_BYTES {
            break;
        }
        v["content"].as_array_mut().expect("content array").push(json!({
            "type": "image",
            "data": base64(&bytes),
            "mimeType": "image/png",
        }));
    }
    v
}

/// Images larger than this are referenced by path only.
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// Standard base64 (RFC 4648) with padding.
fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn failure_result(f: &Failure) -> Value {
    let mut text = f.error.to_string();
    if let Some(i) = f.step {
        text = format!("batch step {}: {text}", i + 1);
    }
    for d in &f.diagnostics {
        text += &format!("\n{d}");
    }
    let mut s = json!({"ok": false, "command": f.command, "error": f.error, "diagnostics": f.diagnostics});
    if let Some(i) = f.step {
        s["step"] = json!(i);
    }
    json!({"content": [{"type": "text", "text": text}], "structuredContent": s, "isError": true})
}

#[cfg(test)]
mod tests {
    #[test]
    fn cli_hints_become_json_arguments() {
        let r = cadlab::registry();
        let h = super::hint_for_mcp(r, "mark it: `net.set +5V --driven`");
        assert_eq!(h, r#"mark it: `net.set {"nets":["+5V"],"driven":true}`"#);
        // Placeholders and quoted words stay whole; plain command names stay as they are.
        let h = super::hint_for_mcp(
            r,
            r#"`bom.replace WS2812B "LED <value> <package>"`, or `part.set WS2812B --footprint <name>`; see `circuit.list`"#,
        );
        assert_eq!(
            h,
            r#"`bom.replace {"from":"WS2812B","to":"LED <value> <package>"}`, or `part.set {"id":"WS2812B","footprint":"<name>"}`; see `circuit.list`"#
        );
        // Unknown flags leave the span alone.
        assert_eq!(super::hint_for_mcp(r, "`net.set X --bogus`"), "`net.set X --bogus`");
    }

    #[test]
    fn base64_rfc4648() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(super::base64(i.as_bytes()), o);
        }
    }
}
