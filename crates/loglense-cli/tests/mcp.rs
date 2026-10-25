//! MCP dogfood suite (T13): an rmcp client drives the REAL `loglense mcp`
//! binary over stdio. The server is launched from `$HOME` on purpose — the
//! classic cwd trap — and every run must still land in the project named
//! by `cwd`.

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::process::Stdio;

use rmcp::model::{CallToolRequestParams, JsonObject};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_loglense");

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(project.join(".git")).unwrap();
        Self {
            _tmp: tmp,
            home,
            project,
        }
    }

    fn cwd(&self) -> String {
        self.project.to_string_lossy().into_owned()
    }

    /// The MCP session plus the server process (killed on drop).
    async fn client(&self) -> Session {
        let mut child = tokio::process::Command::new(BIN)
            .arg("mcp")
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .env_remove("LOGLENSE_STATE_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("loglense mcp starts");
        let stdout = child.stdout.take().expect("stdout");
        let stdin = child.stdin.take().expect("stdin");
        let service = ().serve((stdout, stdin)).await.expect("MCP handshake");
        Session {
            service,
            _server: child,
        }
    }
}

struct Session {
    service: RunningService<RoleClient, ()>,
    _server: tokio::process::Child,
}

impl std::ops::Deref for Session {
    type Target = RunningService<RoleClient, ()>;

    fn deref(&self) -> &Self::Target {
        &self.service
    }
}

/// Calls `tool`; returns (structured content, is_error).
async fn call(client: &RunningService<RoleClient, ()>, tool: &str, args: Value) -> (Value, bool) {
    let args: JsonObject = match args {
        Value::Object(map) => map,
        _ => JsonObject::default(),
    };
    let result = client
        .call_tool(CallToolRequestParams::new(tool.to_string()).with_arguments(args))
        .await
        .unwrap_or_else(|err| panic!("{tool}: transport/protocol error {err}"));
    let value = result
        .structured_content
        .clone()
        .unwrap_or_else(|| panic!("{tool}: structured content missing"));
    (value, result.is_error == Some(true))
}

async fn ok(client: &RunningService<RoleClient, ()>, tool: &str, args: Value) -> Value {
    let (value, is_error) = call(client, tool, args.clone()).await;
    assert!(!is_error, "{tool}({args}) failed: {value}");
    value
}

async fn err(client: &RunningService<RoleClient, ()>, tool: &str, args: Value) -> String {
    let (value, is_error) = call(client, tool, args.clone()).await;
    assert!(is_error, "{tool}({args}) should fail, got {value}");
    value["error"].as_str().unwrap_or_default().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn exactly_the_eight_documented_tools_are_listed() {
    let fixture = Fixture::new();
    let client = fixture.client().await;
    let mut names: Vec<String> = client
        .list_all_tools()
        .await
        .expect("tools/list")
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "get_events",
            "get_log",
            "get_run",
            "kill_run",
            "leave_feedback",
            "list_runs",
            "start_run",
            "wait_run"
        ]
    );
    let _ = client.service.cancel().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn full_agent_loop_over_the_real_binary_launched_from_home() {
    let fixture = Fixture::new();
    let client = fixture.client().await;
    let script = "echo compiling; echo 'ERROR boom' >&2; echo 'warning: careful'; exit 2";
    let started = ok(
        &client,
        "start_run",
        json!({ "cmd": ["sh", "-c", script], "cwd": fixture.cwd() }),
    )
    .await;
    let run_id = started["run_id"].as_str().expect("run_id").to_string();
    assert!(started["pid"].as_u64().is_some());
    assert!(
        fixture
            .project
            .join(".loglense/runs")
            .join(&run_id)
            .is_dir(),
        "run lands in the project named by cwd"
    );
    assert!(
        !fixture.home.join(".loglense").exists(),
        "the server's own cwd ($HOME) is never used"
    );

    // Run-scoped calls without cwd: the server remembers the store.
    let waited = ok(&client, "wait_run", json!({ "run_id": run_id })).await;
    assert_eq!(waited["status"], "exited");
    assert_eq!(waited["exit_code"], 2);
    assert_eq!(waited["timed_out"], false);

    let run = ok(&client, "get_run", json!({ "run_id": run_id })).await;
    assert_eq!(run["state"], "exited");
    assert_eq!(run["meta"]["exit_code"], 2);
    assert_eq!(run["summary"]["severities"]["error"], 1);
    assert_eq!(run["templates"][0]["severity"], "error");
    assert_eq!(run["templates"][0]["example"], "ERROR boom");

    let events = ok(
        &client,
        "get_events",
        json!({ "run_id": run_id, "severity": "error" }),
    )
    .await;
    assert_eq!(events["total"], 1);
    assert_eq!(events["events"][0]["message"], "ERROR boom");
    let paged = ok(
        &client,
        "get_events",
        json!({ "run_id": run_id, "limit": 1, "offset": 1 }),
    )
    .await;
    assert_eq!(paged["total"], 3);
    assert_eq!(paged["shown"], 1);

    let both = ok(
        &client,
        "get_log",
        json!({ "run_id": run_id, "stream": "both" }),
    )
    .await;
    let texts: Vec<&str> = both["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["text"].as_str().unwrap())
        .collect();
    assert!(
        texts.contains(&"ERROR boom") && texts.contains(&"compiling"),
        "{texts:?}"
    );
    let window = ok(
        &client,
        "get_log",
        json!({ "run_id": run_id, "stream": "out", "from_line": 2, "tail_n": 1 }),
    )
    .await;
    assert_eq!(window["lines"][0]["text"], "warning: careful");
    assert_eq!(window["truncated"], true);

    let listed = ok(&client, "list_runs", json!({ "cwd": fixture.cwd() })).await;
    assert_eq!(listed["total"], 1);
    assert_eq!(listed["runs"][0]["run_id"], run_id.as_str());
    assert_eq!(listed["runs"][0]["counts"]["error"], 1);

    let feedback = ok(
        &client,
        "leave_feedback",
        json!({
            "run_id": run_id,
            "kind": "parse-miss",
            "detail": "the warning should be an error",
            "line": 2,
            "expected_severity": "error",
        }),
    )
    .await;
    let fid = feedback["feedback_id"].as_str().expect("feedback id");
    assert!(fid.starts_with("f-"), "{fid}");
    let record = fixture
        .project
        .join(".loglense/feedback")
        .join(format!("{fid}.json"));
    let record: Value = serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    assert_eq!(record["kind"], "parse-miss");
    assert_eq!(record["expected"], "error");
    assert_eq!(record["parser"], "generic");
    assert!(fixture
        .home
        .join(".local/state/loglense/feedback-index.jsonl")
        .is_file());
    let _ = client.service.cancel().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failures_are_tool_errors_and_the_server_keeps_serving() {
    let fixture = Fixture::new();
    let client = fixture.client().await;
    let unknown = err(&client, "get_run", json!({ "run_id": "20991231-ZZZZZZ" })).await;
    assert!(
        unknown.contains("cwd"),
        "error tells the agent what to pass: {unknown}"
    );
    let malformed = err(
        &client,
        "get_run",
        json!({ "run_id": "../../etc", "cwd": fixture.cwd() }),
    )
    .await;
    assert!(malformed.contains("invalid run id"), "{malformed}");
    let relative = err(
        &client,
        "start_run",
        json!({ "cmd": ["true"], "cwd": "relative/dir" }),
    )
    .await;
    assert!(relative.contains("absolute"), "{relative}");
    err(
        &client,
        "start_run",
        json!({ "cmd": [], "cwd": fixture.cwd() }),
    )
    .await;
    err(
        &client,
        "start_run",
        json!({ "cmd": ["true"], "cwd": fixture.cwd(), "parser": "no-such-parser" }),
    )
    .await;
    let no_store = err(&client, "list_runs", json!({ "cwd": fixture.home })).await;
    assert!(
        no_store.contains("loglense init") || no_store.contains("no `.loglense/`"),
        "{no_store}"
    );

    // Timeout path, kill, and kill-again — on the same live connection.
    let started = ok(
        &client,
        "start_run",
        json!({ "cmd": ["sleep", "30"], "cwd": fixture.cwd() }),
    )
    .await;
    let run_id = started["run_id"].as_str().unwrap().to_string();
    let waited = ok(
        &client,
        "wait_run",
        json!({ "run_id": run_id, "timeout_ms": 300 }),
    )
    .await;
    assert_eq!(waited["status"], "running");
    assert_eq!(waited["timed_out"], true);
    assert!(waited.get("exit_code").is_none());
    let both_from = err(
        &client,
        "get_log",
        json!({ "run_id": run_id, "stream": "both", "from_line": 1 }),
    )
    .await;
    assert!(both_from.contains("from_line"), "{both_from}");
    let killed = ok(&client, "kill_run", json!({ "run_id": run_id })).await;
    assert_eq!(killed["action"], "signaled");
    let done = ok(
        &client,
        "wait_run",
        json!({ "run_id": run_id, "timeout_ms": 10000 }),
    )
    .await;
    assert_eq!(done["status"], "signaled");
    let again = ok(&client, "kill_run", json!({ "run_id": run_id })).await;
    assert_eq!(again["action"], "already-terminal");
    let _ = client.service.cancel().await;
}

#[test]
fn stdout_carries_only_json_rpc_frames_while_diagnostics_go_to_stderr() {
    let fixture = Fixture::new();
    // A broken project pack makes the registry print a warning while the
    // tool runs — it must reach stderr, never the protocol stream.
    let packs = fixture.project.join(".loglense/formats.d");
    std::fs::create_dir_all(&packs).unwrap();
    std::fs::write(packs.join("broken.toml"), "schema = 1\nname = \n").unwrap();

    let mut child = std::process::Command::new(BIN)
        .arg("mcp")
        .current_dir(&fixture.home)
        .env("HOME", &fixture.home)
        .env_remove("LOGLENSE_STATE_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("loglense mcp starts");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut frames = Vec::new();
    let mut read_frame = |frames: &mut Vec<String>| {
        let mut line = String::new();
        stdout.read_line(&mut line).expect("read frame");
        frames.push(line.clone());
        line
    };
    let send = |stdin: &mut std::process::ChildStdin, value: Value| {
        writeln!(stdin, "{value}").expect("write frame");
        stdin.flush().expect("flush");
    };
    send(
        &mut stdin,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "purity-test", "version": "0"}
        }}),
    );
    read_frame(&mut frames);
    send(
        &mut stdin,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    send(
        &mut stdin,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "start_run",
            "arguments": {"cmd": ["true"], "cwd": fixture.cwd(), "parser": "generic"}
        }}),
    );
    let response = loop {
        let line = read_frame(&mut frames);
        assert!(!line.is_empty(), "server closed stdout early");
        let value: Value = serde_json::from_str(&line).expect("frame is JSON");
        if value["id"] == 2 {
            break value;
        }
    };
    assert_eq!(response["result"]["isError"], false, "{response}");
    drop(stdin);
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut stdout, &mut rest).unwrap();
    frames.extend(rest.lines().map(str::to_string));
    let status = child.wait().unwrap();
    assert!(status.success(), "clean exit on stdin EOF: {status}");
    for frame in frames.iter().filter(|f| !f.trim().is_empty()) {
        let value: Value = serde_json::from_str(frame)
            .unwrap_or_else(|e| panic!("non-protocol bytes on stdout ({e}): {frame:?}"));
        assert_eq!(value["jsonrpc"], "2.0", "{frame}");
    }
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    assert!(
        stderr.contains("broken.toml"),
        "warning went to stderr: {stderr:?}"
    );
}

#[test]
fn mcp_with_closed_stdin_exits_cleanly() {
    let fixture = Fixture::new();
    let out = std::process::Command::new(BIN)
        .arg("mcp")
        .current_dir(&fixture.home)
        .env("HOME", &fixture.home)
        .stdin(Stdio::null())
        .output()
        .expect("runs");
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn start_run_on_complete_round_trips_through_get_run() {
    let fixture = Fixture::new();
    let client = fixture.client().await;
    let marker = fixture.project.join("callback-fired");
    let started = ok(
        &client,
        "start_run",
        json!({
            "cmd": ["sh", "-c", "exit 0"],
            "cwd": fixture.cwd(),
            "on_complete": format!("echo \"$LOGLENSE_STATUS\" > '{}'; echo pinged", marker.display()),
            "on_complete_timeout_ms": 5000,
        }),
    )
    .await;
    let run_id = started["run_id"].as_str().unwrap().to_string();
    ok(&client, "wait_run", json!({ "run_id": run_id })).await;
    let mut run = Value::Null;
    for _ in 0..200 {
        run = ok(&client, "get_run", json!({ "run_id": run_id })).await;
        if run["summary"].get("callback").is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(run["summary"]["callback"], "ok", "{run}");
    assert_eq!(run["meta"]["on_complete"]["timeout_ms"], 5000);
    assert_eq!(run["callback_log"], "pinged\n");
    assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "exited");
    let _ = client.service.cancel().await;
}
