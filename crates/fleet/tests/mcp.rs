// `fleet mcp` over stdio against a canned fleet (FLEET_FIXTURE): the handshake, the generated
// tool list, and calls that run `fleet` underneath. No tmux, no ssh, no model.
#![allow(deprecated)]

mod common;

use common::{Env, stdout};
use serde_json::{Value, json};

fn session(env: &Env, requests: &[Value]) -> Vec<Value> {
    let fleet = json!([{
        "pid": 42, "session_id": "aaaaaaaa-1111", "name": "app-9d", "cwd": "/tmp", "status": "idle",
        "backend": "tmux", "handle": "%99", "tmux_session": "fleet-test-mcp",
        "name_source": "user", "title": "fix the login loop"
    }]);
    let fixture = common::fixture(env.dir.path(), &fleet.to_string());
    let input: String = requests.iter().map(|r| format!("{r}\n")).collect();
    let out = env
        .cmd()
        .arg("mcp")
        .env("FLEET_FIXTURE", fixture)
        .env("HOME", env.dir.path())
        .write_stdin(input)
        .timeout(std::time::Duration::from_secs(30))
        .output()
        .unwrap();
    assert!(out.status.success());
    let mut replies: Vec<Value> = stdout(&out)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    // Calls answer from their own threads, so order by id.
    replies.sort_by_key(|r| r["id"].as_i64());
    replies
}

fn call(id: i64, name: &str, arguments: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments } })
}

fn text(reply: &Value) -> &str {
    reply["result"]["content"][0]["text"].as_str().unwrap()
}

#[test]
fn handshake_and_tool_list() {
    let env = Env::new();
    env.write_config(&common::two_hosts());
    let r = session(
        &env,
        &[
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": { "protocolVersion": "2025-06-18", "capabilities": {} } }),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            json!({ "jsonrpc": "2.0", "id": 3, "method": "nope" }),
        ],
    );
    assert_eq!(r.len(), 3, "notifications get no reply: {r:?}");
    assert_eq!(r[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(r[0]["result"]["serverInfo"]["name"], "fleet");

    let tools = r[1]["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for n in [
        "list",
        "peek",
        "send",
        "spawn",
        "tmux_list",
        "stack_spawn",
        "pr_checkout",
    ] {
        assert!(names.contains(&n), "{n} missing from {names:?}");
    }
    for n in [
        "watch",
        "enter",
        "ssh",
        "tmux_enter",
        "web_serve",
        "stack_edit",
        "mcp",
        "_probe",
    ] {
        assert!(!names.contains(&n), "{n} needs a terminal: {names:?}");
    }
    let send = tools.iter().find(|t| t["name"] == "send").unwrap();
    assert_eq!(send["inputSchema"]["required"], json!(["target", "text"]));
    assert_eq!(send["annotations"]["readOnlyHint"], false);

    assert_eq!(r[2]["error"]["code"], -32601);
}

#[test]
fn calls_run_the_cli() {
    let env = Env::new();
    env.write_config(&common::two_hosts());
    let r = session(
        &env,
        &[
            call(1, "list", json!({ "json": true })),
            call(2, "send", json!({ "target": "app-9d", "text": "hello" })),
            call(3, "send", json!({ "target": "app-9d" })),
            call(4, "no_such_tool", json!({})),
        ],
    );

    assert_eq!(r[0]["result"]["isError"], false, "{r:?}");
    let rows: Value = serde_json::from_str(text(&r[0])).unwrap();
    assert_eq!(rows[0]["session_id"], "aaaaaaaa-1111");
    assert_eq!(rows[0]["host"], "laptop");

    // A fixture handle is never driven: the CLI's refusal comes back as a tool error.
    assert_eq!(r[1]["result"]["isError"], true);
    assert!(
        text(&r[1]).contains("fleet send -- app-9d hello"),
        "{}",
        text(&r[1])
    );

    assert_eq!(r[2]["result"]["isError"], true);
    assert!(text(&r[2]).contains("missing required argument \"text\""));

    assert_eq!(r[3]["error"]["code"], -32602);
}

// `fleet mcp --http` end to end: a real listener on a free port, raw HTTP requests.
#[test]
fn http_transport() {
    use std::io::{Read, Write};

    let env = Env::new();
    env.write_config(&common::two_hosts());
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let token = "x".repeat(40);
    let mut server = env
        .std_cmd()
        .args([
            "mcp",
            "--http",
            "--port",
            &port.to_string(),
            "--tools",
            "list,peek",
        ])
        .env("FLEET_MCP_TOKEN", &token)
        .env("FLEET_FIXTURE", common::fixture(env.dir.path(), "[]"))
        .env("HOME", env.dir.path())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let request = |auth: &str, body: &str| -> String {
        for _ in 0..100 {
            if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                write!(
                    s,
                    "POST /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {auth}\r\n\
                     Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
                let mut out = String::new();
                s.read_to_string(&mut out).unwrap();
                return out;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("server never listened");
    };

    let denied = request("nope", "{}");
    let list = request(&token, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
    let called = request(
        &token,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list","arguments":{"json":true}}}"#,
    );
    let _ = server.kill();
    let _ = server.wait();

    assert!(denied.starts_with("HTTP/1.1 401"), "{denied}");
    let body =
        |r: &str| -> Value { serde_json::from_str(r.split("\r\n\r\n").nth(1).unwrap()).unwrap() };
    let names: Vec<String> = body(&list)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["list", "peek"]);
    assert_eq!(body(&called)["result"]["isError"], false, "{called}");
}
