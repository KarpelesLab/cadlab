//! End-to-end test of the MCP server over stdio.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    fn start(cwd: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cadlab"))
            .arg("mcp")
            .current_dir(cwd)
            .env("XDG_CONFIG_HOME", cwd.join(".no-config"))
            .env("XDG_CACHE_HOME", cwd.join(".no-cache"))
            .env("XDG_DATA_HOME", cwd.join(".no-data"))
            .env_remove("CADLAB_CATALOGS")
            .env_remove("DIGIKEY_CLIENT_ID")
            .env_remove("DIGIKEY_CLIENT_SECRET")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Client { child, stdin, stdout, next_id: 1 }
    }

    fn send(&mut self, v: Value) {
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "server closed stdout");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}));
        assert!(r.get("error").is_none(), "protocol error: {r}");
        r["result"].clone()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Client::start(dir.path());

    let init = c.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "cadlab");
    c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    assert_eq!(c.request("ping", json!({}))["result"], json!({}));

    let tools = c.request("tools/list", json!({}));
    let names: Vec<&str> =
        tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "project",
            "part",
            "footprint",
            "circuit",
            "net",
            "netclass",
            "block",
            "bom",
            "board",
            "place",
            "track",
            "via",
            "lib",
            "drc",
            "zone",
            "keepout",
            "render",
            "schematic",
            "export",
            "fab",
            "history",
            "describe",
            "call",
            "batch"
        ]
    );
    for t in tools["result"]["tools"].as_array().unwrap() {
        assert_eq!(t["inputSchema"]["type"], "object");
        // Clients such as the Claude API reject top-level oneOf/anyOf/allOf.
        for k in ["oneOf", "anyOf", "allOf"] {
            assert!(t["inputSchema"].get(k).is_none(), "{} has top-level {k}", t["name"]);
        }
    }

    // Nothing open yet.
    let r = c.tool("project", json!({"action": "info"}));
    assert_eq!(r["isError"], true);
    assert_eq!(r["structuredContent"]["error"]["code"], "project.none");

    // Create, then later calls use it.
    let path = dir.path().join("board1");
    let r = c.tool("project", json!({"action": "new", "args": {"path": path, "targets": ["jlcpcb"]}}));
    assert_eq!(r["isError"], false, "{r}");
    assert!(path.join("cadlab.toml").is_file());

    let r = c.tool("project", json!({"action": "set", "args": {"description": "from mcp"}}));
    assert_eq!(r["structuredContent"]["output"]["description"], "from mcp");
    // Autosave is on by default.
    assert!(std::fs::read_to_string(path.join("cadlab.toml")).unwrap().contains("from mcp"));

    let r = c.tool("project", json!({"action": "set", "args": {"name": "dry"}, "dry_run": true}));
    assert_eq!(r["structuredContent"]["output"]["name"], "dry");
    let r = c.tool("project", json!({"action": "info"}));
    assert_eq!(r["structuredContent"]["output"]["name"], "board1");

    let r = c.tool("history", json!({"action": "undo"}));
    assert_eq!(r["isError"], false, "{r}");
    let r = c.tool("call", json!({"command": "project.info"}));
    assert!(r["structuredContent"]["output"].get("description").is_none());

    let r = c.tool(
        "batch",
        json!({"steps": [
            {"cmd": "project.set", "args": {"name": "batched"}},
            {"cmd": "project.set", "args": {"metadata": {"rev": "B"}}}
        ]}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let r = c.tool("project", json!({"action": "info"}));
    assert_eq!(r["structuredContent"]["output"]["name"], "batched");
    assert_eq!(r["structuredContent"]["output"]["undo_depth"], 1);

    // A second project, then address the first by path.
    let path2 = dir.path().join("board2");
    c.tool("project", json!({"action": "new", "args": {"path": path2}}));
    let r = c.tool("project", json!({"action": "info", "project": path}));
    assert_eq!(r["structuredContent"]["output"]["name"], "batched");

    // describe
    let r = c.tool("describe", json!({"command": "project.set"}));
    assert_eq!(r["structuredContent"]["commands"][0]["name"], "project.set");
    assert!(r["content"][0]["text"].as_str().unwrap().contains("display_units"));

    // Invalid arguments are tool errors (visible to the model), with a hint.
    let r = c.tool("project", json!({"action": "set", "args": {"nmae": "x"}}));
    assert_eq!(r["isError"], true);
    assert!(r["content"][0]["text"].as_str().unwrap().contains("hint"));

    // Unknown tools and methods are protocol errors.
    let r = c.request("tools/call", json!({"name": "nope", "arguments": {}}));
    assert_eq!(r["error"]["code"], -32602);
    let r = c.request("resources/list", json!({}));
    assert_eq!(r["error"]["code"], -32601);
}
