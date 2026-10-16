//! `fleet mcp`: the CLI as a Model Context Protocol server on stdio.
//!
//! One tool per leaf subcommand, generated from the clap tree so it can't drift from the CLI.
//! A call runs this binary with the matching argv (no tty, `NO_COLOR`, `FLEET_NO_TTY`) and
//! returns its stdout, stderr and exit code. Interactive commands (attach, editor, dashboard,
//! long-running servers) are left out. Wire format: newline-delimited JSON-RPC 2.0.

use std::any::TypeId;
use std::io::{BufRead, Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::ArgAction;
use serde_json::{Map, Value, json};

use crate::error::{Error, Result};

const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Commands that need a terminal or never return: attach, editor, dashboard, servers.
const SKIP_COMMANDS: &[&str] = &[
    "watch",
    "enter",
    "last",
    "new",
    "ssh",
    "tmux enter",
    "tmux last",
    "web serve",
    "stack edit",
    "config edit",
    "mcp",
];

/// Flags that need a terminal, per command.
const SKIP_ARGS: &[(&str, &str)] = &[("brief", "edit"), ("exec", "tty")];

/// Flags always passed, per command (and hidden from the schema).
const FORCED_ARGS: &[(&str, &str, &str)] = &[("tmux new", "detach", "--detach")];

/// Commands that read stdin (`brief --set`, `stack set`, `handoff --file -`, `group --input -`).
const STDIN_COMMANDS: &[&str] = &["brief", "stack set", "handoff", "group"];

/// Commands that only read.
const READ_ONLY: &[&str] = &[
    "list",
    "peek",
    "messages",
    "usage",
    "doctor",
    "prs",
    "harness list",
    "stack list",
    "stack show",
    "tmux list",
    "repos status",
    "config path",
    "config show",
    "config get",
    "skill show",
];

const INSTRUCTIONS: &str = "fleet supervises Claude Code (and Codex, pi) sessions and tmux sessions \
across machines. Each tool is one `fleet` subcommand (tool `tmux_list` = `fleet tmux list`). \
Every tool takes `host` (a configured host name or ssh destination; default: this machine, or \
defaultHost for tmux commands) and `dry_run`. Pass `json: true` where offered for machine-readable \
output. Session targets: a title, session name, session-id prefix or pid; `pid:<n>` and \
`id:<session id>` match exactly and never fall through; an ambiguous target errors with \
candidates — never guess, re-ask with a more specific one. `send`, `keys`, `answer`, `rename`, \
`spawn` and `handoff` type into live agents.";

#[derive(Debug, Clone)]
enum Kind {
    Flag,
    Value { multi: bool },
}

#[derive(Debug, Clone)]
struct ArgSpec {
    id: String,
    kind: Kind,
    long: Option<String>,
    short: Option<char>,
    positional: Option<usize>,
    last: bool,
    required: bool,
    schema: Value,
}

#[derive(Debug, Clone)]
struct Tool {
    name: String,
    /// The subcommand path, e.g. `["tmux", "list"]`.
    path: Vec<String>,
    description: String,
    args: Vec<ArgSpec>,
    forced: Vec<String>,
    stdin: bool,
    read_only: bool,
}

impl Tool {
    fn key(&self) -> String {
        self.path.join(" ")
    }

    fn input_schema(&self) -> Value {
        let mut props = Map::new();
        let mut required = Vec::new();
        for a in &self.args {
            props.insert(a.id.clone(), a.schema.clone());
            if a.required {
                required.push(Value::String(a.id.clone()));
            }
        }
        if self.stdin {
            props.insert(
                "stdin".into(),
                json!({ "type": "string", "description": "Text fed to the command's stdin (for `--set`, `--file -`, `--input -`)" }),
            );
        }
        props.insert(
            "host".into(),
            json!({ "type": "string", "description": "Run on this host (a configured name or ssh destination) — `fleet -H <host>`" }),
        );
        props.insert(
            "dry_run".into(),
            json!({ "type": "boolean", "description": "Print what would run, run nothing — `fleet -n`" }),
        );
        json!({
            "type": "object",
            "properties": props,
            "required": required,
            "additionalProperties": false,
        })
    }

    fn describe(&self) -> Value {
        json!({
            "name": self.name,
            "title": format!("fleet {}", self.key()),
            "description": self.description,
            "inputSchema": self.input_schema(),
            "annotations": {
                "title": format!("fleet {}", self.key()),
                "readOnlyHint": self.read_only,
            },
        })
    }

    /// The argv after `fleet` for one call's arguments.
    fn argv(&self, input: &Map<String, Value>, default_host: Option<&str>) -> Result<Vec<String>> {
        let known = |k: &str| {
            matches!(k, "host" | "dry_run")
                || (self.stdin && k == "stdin")
                || self.args.iter().any(|a| a.id == k)
        };
        if let Some(k) = input.keys().find(|k| !known(k)) {
            return Err(Error::Other(format!("unknown argument \"{k}\"")));
        }

        let mut argv = Vec::new();
        if let Some(h) = input
            .get("host")
            .and_then(Value::as_str)
            .filter(|h| !h.trim().is_empty())
            .or(default_host)
        {
            argv.push("-H".into());
            argv.push(h.to_string());
        }
        if input.get("dry_run").and_then(Value::as_bool) == Some(true) {
            argv.push("-n".into());
        }
        argv.extend(self.path.iter().cloned());
        argv.extend(self.forced.iter().cloned());

        let mut positionals: Vec<(&ArgSpec, Vec<String>)> = Vec::new();
        for a in &self.args {
            let v = input.get(&a.id).filter(|v| !v.is_null());
            if a.required && v.is_none() {
                return Err(Error::Other(format!(
                    "missing required argument \"{}\"",
                    a.id
                )));
            }
            let Some(v) = v else { continue };
            let flag = match (&a.long, a.short) {
                (Some(l), _) => format!("--{l}"),
                (None, Some(s)) => format!("-{s}"),
                (None, None) => String::new(),
            };
            match a.kind {
                Kind::Flag => match v {
                    Value::Bool(true) => argv.push(flag),
                    Value::Bool(false) => {}
                    _ => return Err(Error::Other(format!("\"{}\" must be a boolean", a.id))),
                },
                Kind::Value { multi } => {
                    let values = strings(&a.id, v, multi)?;
                    if a.positional.is_some() {
                        positionals.push((a, values));
                    } else {
                        for val in values {
                            if a.long.is_some() {
                                argv.push(format!("{flag}={val}"));
                            } else {
                                argv.push(flag.clone());
                                argv.push(val);
                            }
                        }
                    }
                }
            }
        }

        // Positionals go after `--` so a value starting with `-` stays a value. A `last` one
        // (`tmux new … -- <cmd>`) is the only thing after it.
        positionals.sort_by_key(|(a, _)| a.positional);
        let declared: Vec<&ArgSpec> = {
            let mut p: Vec<&ArgSpec> = self
                .args
                .iter()
                .filter(|a| a.positional.is_some())
                .collect();
            p.sort_by_key(|a| a.positional);
            p
        };
        // A later positional without the earlier ones would land in the wrong slot.
        let given: Vec<&str> = positionals.iter().map(|(a, _)| a.id.as_str()).collect();
        for (i, a) in declared.iter().enumerate() {
            if !a.last
                && !given.contains(&a.id.as_str())
                && declared[i + 1..]
                    .iter()
                    .any(|b| !b.last && given.contains(&b.id.as_str()))
            {
                return Err(Error::Other(format!(
                    "\"{}\" is needed when a later positional argument is given",
                    a.id
                )));
            }
        }
        let has_last = positionals.iter().any(|(a, _)| a.last);
        if has_last {
            for (a, vals) in positionals.iter().filter(|(a, _)| !a.last) {
                if vals.iter().any(|v| v.starts_with('-')) {
                    return Err(Error::Other(format!("\"{}\" can't start with '-'", a.id)));
                }
                argv.extend(vals.iter().cloned());
            }
            argv.push("--".into());
            for (_, vals) in positionals.iter().filter(|(a, _)| a.last) {
                argv.extend(vals.iter().cloned());
            }
        } else if !positionals.is_empty() {
            argv.push("--".into());
            for (_, vals) in positionals {
                argv.extend(vals);
            }
        }
        Ok(argv)
    }
}

/// A JSON argument as the strings clap will parse.
fn strings(id: &str, v: &Value, multi: bool) -> Result<Vec<String>> {
    let one = |v: &Value| match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err(Error::Other(format!("\"{id}\" must be a string or number"))),
    };
    match v {
        Value::Array(items) if multi => items.iter().map(one).collect(),
        Value::Array(_) => Err(Error::Other(format!("\"{id}\" takes one value"))),
        _ => Ok(vec![one(v)?]),
    }
}

fn is_integer(arg: &clap::Arg) -> bool {
    let t = arg.get_value_parser().type_id();
    [
        TypeId::of::<u8>(),
        TypeId::of::<u16>(),
        TypeId::of::<u32>(),
        TypeId::of::<u64>(),
        TypeId::of::<usize>(),
        TypeId::of::<i32>(),
        TypeId::of::<i64>(),
    ]
    .iter()
    .any(|id| t == *id)
}

fn arg_spec(arg: &clap::Arg) -> Option<ArgSpec> {
    if arg.is_hide_set() || arg.is_global_set() {
        return None;
    }
    let kind = match arg.get_action() {
        ArgAction::SetTrue => Kind::Flag,
        ArgAction::Set => Kind::Value {
            multi: arg.get_num_args().is_some_and(|r| r.max_values() > 1),
        },
        ArgAction::Append => Kind::Value { multi: true },
        _ => return None, // help, version, count
    };
    let mut desc = arg.get_help().map(|h| h.to_string()).unwrap_or_default();
    if desc.is_empty() && arg.get_id() == "json" {
        desc = "Machine-readable output".into();
    }
    let defaults: Vec<String> = arg
        .get_default_values()
        .iter()
        .map(|d| d.to_string_lossy().into_owned())
        .collect();
    if !defaults.is_empty() && !matches!(kind, Kind::Flag) {
        desc.push_str(&format!(" (default: {})", defaults.join(", ")));
    }
    let scalar = |integer: bool| {
        let enum_values: Vec<String> = arg
            .get_possible_values()
            .iter()
            .filter(|p| !p.is_hide_set())
            .map(|p| p.get_name().to_string())
            .collect();
        if !enum_values.is_empty() {
            json!({ "type": "string", "enum": enum_values })
        } else if integer {
            json!({ "type": "integer", "minimum": 0 })
        } else {
            json!({ "type": "string" })
        }
    };
    let mut schema = match kind {
        Kind::Flag => json!({ "type": "boolean" }),
        Kind::Value { multi: false } => scalar(is_integer(arg)),
        Kind::Value { multi: true } => json!({ "type": "array", "items": scalar(is_integer(arg)) }),
    };
    if !desc.is_empty() {
        schema["description"] = Value::String(desc);
    }
    Some(ArgSpec {
        id: arg.get_id().to_string(),
        kind,
        long: arg.get_long().map(str::to_string),
        short: arg.get_short(),
        positional: arg.is_positional().then(|| arg.get_index().unwrap_or(0)),
        last: arg.is_last_set(),
        required: arg.is_required_set(),
        schema,
    })
}

fn collect(cmd: &clap::Command, path: &mut Vec<String>, out: &mut Vec<Tool>) {
    for sub in cmd.get_subcommands() {
        let name = sub.get_name();
        if sub.is_hide_set() || name == "help" {
            continue;
        }
        path.push(name.to_string());
        let key = path.join(" ");
        if !SKIP_COMMANDS.contains(&key.as_str()) {
            if sub.has_subcommands() {
                collect(sub, path, out);
            } else {
                out.push(tool(sub, path, &key));
            }
        }
        path.pop();
    }
}

fn tool(cmd: &clap::Command, path: &[String], key: &str) -> Tool {
    let forced: Vec<(&str, &str)> = FORCED_ARGS
        .iter()
        .filter(|(c, _, _)| *c == key)
        .map(|(_, id, flag)| (*id, *flag))
        .collect();
    let args = cmd
        .get_arguments()
        .filter(|a| {
            let id = a.get_id().as_str();
            !SKIP_ARGS.contains(&(key, id)) && !forced.iter().any(|(f, _)| *f == id)
        })
        .filter_map(arg_spec)
        .collect();
    let mut description = cmd
        .get_long_about()
        .or(cmd.get_about())
        .map(|s| s.to_string())
        .unwrap_or_default();
    description.push_str(&format!("\n\nRuns `fleet {key}`."));
    Tool {
        name: path.join("_").replace('-', "_"),
        path: path.to_vec(),
        description,
        args,
        forced: forced.iter().map(|(_, f)| f.to_string()).collect(),
        stdin: STDIN_COMMANDS.contains(&key),
        read_only: READ_ONLY.contains(&key),
    }
}

/// Every tool the CLI exposes, in CLI order.
fn tools(mut root: clap::Command) -> Vec<Tool> {
    root.build();
    let mut out = Vec::new();
    collect(&root, &mut Vec::new(), &mut out);
    out
}

fn timeout() -> Duration {
    let secs = std::env::var("FLEET_MCP_TIMEOUT")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(600);
    Duration::from_secs(secs)
}

/// Run `fleet <argv>` and shape the outcome as a tool result.
fn run(argv: &[String], stdin: Option<&str>) -> Value {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return text_result(&format!("can't find the fleet binary: {e}"), true),
    };
    let child = Command::new(exe)
        .args(argv)
        .env("NO_COLOR", "1")
        .env("FLEET_NO_TTY", "1")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return text_result(&format!("can't run fleet: {e}"), true),
    };
    if let (Some(mut w), Some(text)) = (child.stdin.take(), stdin) {
        let text = text.to_string();
        std::thread::spawn(move || {
            let _ = w.write_all(text.as_bytes());
        });
    }
    let drain = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|r| Box::new(r) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|r| Box::new(r) as Box<dyn Read + Send>),
    );

    let deadline = Instant::now() + timeout();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => break None,
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();

    let code = status.and_then(|s| s.code());
    let mut text = stdout.trim_end().to_string();
    let stderr = stderr.trim_end();
    if !stderr.is_empty() {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str("[stderr]\n");
        text.push_str(stderr);
    }
    let failed = code != Some(0);
    if failed {
        let why = match (status, code) {
            (None, _) => format!("timed out after {}s", timeout().as_secs()),
            (_, Some(c)) => format!("exit code {c}"),
            (_, None) => "killed by a signal".to_string(),
        };
        text = format!("`fleet {}` failed: {why}\n\n{text}", argv.join(" "));
    }
    if text.is_empty() {
        text = "(no output)".into();
    }
    text_result(&text, failed)
}

fn text_result(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn reply(id: &Value, result: std::result::Result<Value, (i64, String)>) -> Value {
    match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    }
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|v| PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "fleet", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

/// Tools a remote (`--http`) server leaves out unless `--tools` names them: they change the
/// machine itself, not the sessions on it (and every `*_install_service`).
const HTTP_EXCLUDED: &[&str] = &[
    "exec",
    "install",
    "init",
    "config_set",
    "web_build",
    "skill_install",
];

fn http_excluded(name: &str) -> bool {
    HTTP_EXCLUDED.contains(&name) || name.ends_with("_install_service")
}

/// `fleet mcp --http`: where to listen and the bearer token every request must carry.
pub struct HttpOpts {
    pub bind: String,
    pub port: u16,
    pub token: String,
}

pub struct ServeOpts {
    /// From `fleet -H <host> mcp`: the host of calls that name none.
    pub default_host: Option<String>,
    /// Only these tools (`stack_*` globs); empty = all (over HTTP: all but [`HTTP_EXCLUDED`]).
    pub tools: Vec<String>,
    /// Never these tools.
    pub exclude: Vec<String>,
    /// Serve Streamable HTTP instead of stdio.
    pub http: Option<HttpOpts>,
}

fn matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

struct Server {
    tools: Vec<Tool>,
    default_host: Option<String>,
    /// Remote callers can't wait long (an agent runtime gives a call ~30s): `no_wait` defaults on.
    remote: bool,
}

impl Server {
    fn new(root: clap::Command, opts: &ServeOpts) -> Result<Self> {
        let all = tools(root);
        for p in opts.tools.iter().chain(&opts.exclude) {
            if !all.iter().any(|t| matches(p, &t.name)) {
                return Err(Error::Other(format!(
                    "--tools/--exclude: no tool matches \"{p}\""
                )));
            }
        }
        let remote = opts.http.is_some();
        let tools = all
            .into_iter()
            .filter(|t| {
                if opts.tools.is_empty() {
                    !(remote && http_excluded(&t.name))
                } else {
                    opts.tools.iter().any(|p| matches(p, &t.name))
                }
            })
            .filter(|t| !opts.exclude.iter().any(|p| matches(p, &t.name)))
            .collect();
        Ok(Server {
            tools,
            default_host: opts.default_host.clone(),
            remote,
        })
    }

    /// One JSON-RPC message in, its reply out (`None` for notifications and responses).
    fn handle(&self, msg: &Value) -> Option<Value> {
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let id = msg.get("id")?;
        if method.is_empty() {
            return None;
        }
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        Some(match method {
            "initialize" => reply(id, Ok(initialize(&params))),
            "ping" => reply(id, Ok(json!({}))),
            "tools/list" => {
                let list: Vec<Value> = self.tools.iter().map(Tool::describe).collect();
                reply(id, Ok(json!({ "tools": list })))
            }
            "tools/call" => reply(id, self.call(&params)),
            _ => reply(id, Err((-32601, format!("method not found: {method}")))),
        })
    }

    fn call(&self, params: &Value) -> std::result::Result<Value, (i64, String)> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let Some(tool) = self.tools.iter().find(|t| t.name == name) else {
            return Err((-32602, format!("unknown tool: {name}")));
        };
        let mut input = match params.get("arguments") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => return Err((-32602, "arguments must be an object".into())),
        };
        if self.remote && tool.args.iter().any(|a| a.id == "no_wait") {
            input.entry("no_wait").or_insert(Value::Bool(true));
        }
        let started = Instant::now();
        let (result, argv) = match tool.argv(&input, self.default_host.as_deref()) {
            Ok(argv) => (
                run(&argv, input.get("stdin").and_then(Value::as_str)),
                argv.join(" "),
            ),
            Err(e) => (text_result(&e.to_string(), true), String::new()),
        };
        let failed = result["isError"] == Value::Bool(true);
        let mut shown: String = argv.chars().take(200).collect();
        if shown.len() < argv.len() {
            shown.push('…');
        }
        eprintln!(
            "{} fleet mcp: {name} {} {}ms  fleet {shown}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            if failed { "error" } else { "ok" },
            started.elapsed().as_millis(),
        );
        Ok(result)
    }
}

/// `fleet mcp`: serve on stdio until stdin closes, or over HTTP (`--http`) until killed.
pub fn serve(root: clap::Command, opts: ServeOpts) -> Result<()> {
    let server = Arc::new(Server::new(root, &opts)?);
    match &opts.http {
        Some(http) => serve_http(server, http),
        None => serve_stdio(server),
    }
}

fn serve_stdio(server: Arc<Server>) -> Result<()> {
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let write = |out: &Mutex<std::io::Stdout>, msg: &Value| {
        let mut o = out.lock().unwrap_or_else(|p| p.into_inner());
        let _ = writeln!(o, "{msg}");
        let _ = o.flush();
    };
    let mut calls = Vec::new();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                write(
                    &out,
                    &reply(&Value::Null, Err((-32700, format!("parse error: {e}")))),
                );
                continue;
            }
        };
        // Tool calls answer from their own thread, so a slow one doesn't hold up the rest.
        if msg.get("method").and_then(Value::as_str) == Some("tools/call") {
            let (server, out) = (Arc::clone(&server), Arc::clone(&out));
            calls.push(std::thread::spawn(move || {
                if let Some(r) = server.handle(&msg) {
                    write(&out, &r);
                }
            }));
            calls.retain(|h| !h.is_finished());
        } else if let Some(r) = server.handle(&msg) {
            write(&out, &r);
        }
    }
    for c in calls {
        let _ = c.join();
    }
    Ok(())
}

/// Streamable HTTP (MCP 2025-03-26+), stateless: `POST /mcp` with one message (or a batch)
/// answers `application/json`; notifications get 202; no server-sent stream (GET is 405).
/// One request per connection, every request needs `Authorization: Bearer <token>`.
fn serve_http(server: Arc<Server>, opts: &HttpOpts) -> Result<()> {
    if opts.token.trim().len() < 16 {
        return Err(Error::Other(
            "fleet mcp --http: the token must be at least 16 characters".into(),
        ));
    }
    let listener = std::net::TcpListener::bind((opts.bind.as_str(), opts.port))?;
    eprintln!(
        "fleet mcp: {} tools on http://{}:{}/mcp",
        server.tools.len(),
        opts.bind,
        opts.port
    );
    let token = Arc::new(opts.token.trim().to_string());
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (server, token) = (Arc::clone(&server), Arc::clone(&token));
        std::thread::spawn(move || {
            let _ = http_connection(stream, &server, &token);
        });
    }
    Ok(())
}

const MAX_BODY: usize = 4 << 20;

struct Response {
    status: u16,
    body: Option<Value>,
    extra: &'static str,
}

impl Response {
    fn json(status: u16, body: Value) -> Self {
        Response {
            status,
            body: Some(body),
            extra: "",
        }
    }
    fn empty(status: u16, extra: &'static str) -> Self {
        Response {
            status,
            body: None,
            extra,
        }
    }
}

fn http_connection(
    stream: std::net::TcpStream,
    server: &Server,
    token: &str,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = std::io::BufReader::new(stream.try_clone()?);
    let response = match read_request(&mut reader) {
        Ok(req) => route(&req, server, token),
        Err(status) => Response::empty(status, ""),
    };
    write_response(stream, response)
}

struct Request {
    method: String,
    path: String,
    authorization: Option<String>,
    body: Vec<u8>,
}

fn read_request(r: &mut impl BufRead) -> std::result::Result<Request, u16> {
    let mut line = String::new();
    r.read_line(&mut line).map_err(|_| 400u16)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or(400u16)?.to_string();
    let target = parts.next().ok_or(400u16)?;
    let path = target.split('?').next().unwrap_or("").to_string();
    let mut length = 0usize;
    let mut authorization = None;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).map_err(|_| 400u16)? == 0 {
            return Err(400);
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let Some((k, v)) = h.split_once(':') else {
            return Err(400);
        };
        let v = v.trim();
        match k.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = v.parse().map_err(|_| 400u16)?,
            "authorization" => authorization = Some(v.to_string()),
            "transfer-encoding" => return Err(411),
            _ => {}
        }
    }
    if length > MAX_BODY {
        return Err(413);
    }
    let mut body = vec![0; length];
    r.read_exact(&mut body).map_err(|_| 400u16)?;
    Ok(Request {
        method,
        path,
        authorization,
        body,
    })
}

/// Compare without leaking how much of the token matched.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn route(req: &Request, server: &Server, token: &str) -> Response {
    if req.path != "/mcp" {
        return Response::empty(404, "");
    }
    let authorized = req
        .authorization
        .as_deref()
        .and_then(|a| a.strip_prefix("Bearer "))
        .is_some_and(|t| same(t.trim(), token));
    if !authorized {
        return Response::empty(401, "WWW-Authenticate: Bearer\r\n");
    }
    if req.method != "POST" {
        return Response::empty(405, "Allow: POST\r\n");
    }
    let msg: Value = match serde_json::from_slice(&req.body) {
        Ok(m) => m,
        Err(e) => {
            return Response::json(
                400,
                reply(&Value::Null, Err((-32700, format!("parse error: {e}")))),
            );
        }
    };
    let replies: Vec<Value> = match &msg {
        Value::Array(batch) => batch.iter().filter_map(|m| server.handle(m)).collect(),
        m => server.handle(m).into_iter().collect(),
    };
    match (msg.is_array(), replies.len()) {
        (_, 0) => Response::empty(202, ""),
        (false, _) => Response::json(200, replies.into_iter().next().unwrap_or_default()),
        (true, _) => Response::json(200, Value::Array(replies)),
    }
}

fn write_response(mut stream: std::net::TcpStream, r: Response) -> std::io::Result<()> {
    let reason = match r.status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        _ => "Payload Too Large",
    };
    let body = r.body.map(|b| b.to_string()).unwrap_or_default();
    let content_type = if body.is_empty() {
        ""
    } else {
        "Content-Type: application/json\r\n"
    };
    write!(
        stream,
        "HTTP/1.1 {} {reason}\r\n{content_type}{}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        r.status,
        r.extra,
        body.len()
    )?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{Arg, Command};

    fn root() -> Command {
        Command::new("fleet")
            .arg(Arg::new("host").short('H').long("host").global(true))
            .subcommand(
                Command::new("send")
                    .about("Type text into a session")
                    .arg(Arg::new("target").required(true))
                    .arg(Arg::new("text").required(true)),
            )
            .subcommand(
                Command::new("tmux")
                    .subcommand(
                        Command::new("list")
                            .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)),
                    )
                    .subcommand(Command::new("enter").arg(Arg::new("query")))
                    .subcommand(
                        Command::new("new")
                            .arg(Arg::new("name"))
                            .arg(
                                Arg::new("detach")
                                    .short('d')
                                    .long("detach")
                                    .action(ArgAction::SetTrue),
                            )
                            .arg(
                                Arg::new("cmd")
                                    .last(true)
                                    .num_args(1..)
                                    .action(ArgAction::Append),
                            ),
                    ),
            )
            .subcommand(Command::new("watch"))
            .subcommand(
                Command::new("peek")
                    .arg(Arg::new("target").required(true))
                    .arg(
                        Arg::new("lines")
                            .long("lines")
                            .value_parser(clap::value_parser!(usize))
                            .default_value("40"),
                    ),
            )
    }

    fn find(name: &str) -> Tool {
        tools(root()).into_iter().find(|t| t.name == name).unwrap()
    }

    fn args(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn one_tool_per_leaf_minus_interactive() {
        let names: Vec<String> = tools(root()).into_iter().map(|t| t.name).collect();
        assert_eq!(names, ["send", "tmux_list", "tmux_new", "peek"]);
    }

    #[test]
    fn schema_types_and_required() {
        let s = find("peek").input_schema();
        assert_eq!(s["properties"]["lines"]["type"], "integer");
        assert!(
            s["properties"]["lines"]["description"]
                .as_str()
                .unwrap()
                .contains("default: 40")
        );
        assert_eq!(s["required"], json!(["target"]));
        assert!(s["properties"]["host"].is_object());
        assert!(
            find("send").input_schema()["properties"]
                .get("stdin")
                .is_none()
        );
    }

    #[test]
    fn argv_puts_positionals_after_double_dash() {
        let argv = find("send")
            .argv(
                &args(json!({ "target": "app", "text": "-rf now", "host": "workstation" })),
                None,
            )
            .unwrap();
        assert_eq!(argv, ["-H", "workstation", "send", "--", "app", "-rf now"]);
    }

    #[test]
    fn argv_flags_values_and_default_host() {
        let argv = find("peek")
            .argv(
                &args(json!({ "target": "app", "lines": 5, "dry_run": true })),
                Some("ws"),
            )
            .unwrap();
        assert_eq!(argv, ["-H", "ws", "-n", "peek", "--lines=5", "--", "app"]);
    }

    #[test]
    fn tmux_new_always_detaches_and_takes_a_command() {
        let t = find("tmux_new");
        assert!(t.input_schema()["properties"].get("detach").is_none());
        let argv = t
            .argv(
                &args(json!({ "name": "job", "cmd": ["htop", "-d", "5"] })),
                None,
            )
            .unwrap();
        assert_eq!(
            argv,
            ["tmux", "new", "--detach", "job", "--", "htop", "-d", "5"]
        );
    }

    #[test]
    fn argv_rejects_bad_input() {
        let t = find("send");
        assert!(t.argv(&args(json!({ "target": "x" })), None).is_err());
        assert!(
            t.argv(
                &args(json!({ "target": "x", "text": "y", "nope": 1 })),
                None
            )
            .is_err()
        );
        assert!(
            find("tmux_list")
                .argv(&args(json!({ "json": "yes" })), None)
                .is_err()
        );
    }

    fn opts(tools: &[&str], exclude: &[&str], http: bool) -> ServeOpts {
        ServeOpts {
            default_host: None,
            tools: tools.iter().map(|s| s.to_string()).collect(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
            http: http.then(|| HttpOpts {
                bind: "127.0.0.1".into(),
                port: 0,
                token: "t".repeat(32),
            }),
        }
    }

    fn names(s: &Server) -> Vec<&str> {
        s.tools.iter().map(|t| t.name.as_str()).collect()
    }

    #[test]
    fn tool_filters() {
        let s = Server::new(root(), &opts(&["tmux_*", "peek"], &["tmux_new"], false)).unwrap();
        assert_eq!(names(&s), ["tmux_list", "peek"]);
        assert!(Server::new(root(), &opts(&["nope"], &[], false)).is_err());
    }

    fn post(body: &str, auth: Option<&str>) -> Request {
        Request {
            method: "POST".into(),
            path: "/mcp".into(),
            authorization: auth.map(|t| format!("Bearer {t}")),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn http_route() {
        let token = "t".repeat(32);
        let s = Server::new(root(), &opts(&[], &[], true)).unwrap();
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;

        assert_eq!(route(&post(init, None), &s, &token).status, 401);
        assert_eq!(route(&post(init, Some("wrong")), &s, &token).status, 401);
        let ok = route(&post(init, Some(&token)), &s, &token);
        assert_eq!(ok.status, 200);
        assert_eq!(ok.body.unwrap()["result"]["serverInfo"]["name"], "fleet");

        let note = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert_eq!(route(&post(note, Some(&token)), &s, &token).status, 202);

        let batch = r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","method":"x"}]"#;
        let r = route(&post(batch, Some(&token)), &s, &token);
        assert_eq!(r.body.unwrap().as_array().unwrap().len(), 1);

        let mut get = post("", Some(&token));
        get.method = "GET".into();
        assert_eq!(route(&get, &s, &token).status, 405);
        get.path = "/other".into();
        assert_eq!(route(&get, &s, &token).status, 404);
        assert_eq!(route(&post("{", Some(&token)), &s, &token).status, 400);
    }

    #[test]
    fn read_request_parses_headers_and_body() {
        let raw = "POST /mcp?x=1 HTTP/1.1\r\nHost: a\r\nAuthorization: Bearer abc\r\nContent-Length: 2\r\n\r\n{}";
        let req = read_request(&mut std::io::Cursor::new(raw)).unwrap();
        assert_eq!((req.method.as_str(), req.path.as_str()), ("POST", "/mcp"));
        assert_eq!(req.authorization.as_deref(), Some("Bearer abc"));
        assert_eq!(req.body, b"{}");
        let chunked = "POST /mcp HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert_eq!(
            read_request(&mut std::io::Cursor::new(chunked)).err(),
            Some(411)
        );
    }
}
