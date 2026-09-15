//! `shep lookout`'s terminal gate, and `shep whistle` speaking MCP down a
//! real pipe.

use super::*;

/// `assert_cmd` captures stdout through a pipe, so this is the not-a-tty
/// refusal a `shep lookout > dash.txt` meets.
#[test]
fn shep_lookout_refuses_when_stdout_is_not_a_terminal() {
    let home = TempDir::new().unwrap();
    let output = shep(home.path())
        .arg("lookout")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("needs a terminal"));
}

#[test]
fn shep_dash_is_the_same_verb() {
    let home = TempDir::new().unwrap();
    let output = shep(home.path())
        .arg("dash")
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("needs a terminal")
    );
}

/// The assertion is on `security boundary` alone: `wrap_help` re-wraps long
/// help at the detected terminal width, so a longer phrase can land across a
/// line break on one machine and not another.
#[test]
fn shep_lookout_help_names_the_gate() {
    let home = TempDir::new().unwrap();
    let output = shep(home.path())
        .args(["lookout", "--help"])
        .timeout(CMD_TIMEOUT)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("--read-only"));
    assert!(text.contains("security boundary"));
}

// ---------------------------------------------------------------------------
// whistle: the MCP interface, driven over real pipes.
// ---------------------------------------------------------------------------

/// Serializes `value` as compact JSON followed by `\n`, the newline-delimited
/// framing `transport-io`'s codec expects on both sides of the pipe.
fn push_mcp_line(buf: &mut Vec<u8>, value: &serde_json::Value) {
    buf.extend_from_slice(value.to_string().as_bytes());
    buf.push(b'\n');
}

/// Stdin for one MCP session: the `initialize` handshake (id `1`), the
/// `notifications/initialized`, then each of `requests`. `"2025-06-18"` is a
/// `ProtocolVersion::KNOWN_VERSIONS` entry rather than `LATEST`, so an rmcp
/// bump does not redden this suite.
fn mcp_session(requests: &[serde_json::Value]) -> Vec<u8> {
    let mut buf = Vec::new();
    push_mcp_line(
        &mut buf,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "cli_e2e", "version": "0.0.0"},
            },
        }),
    );
    push_mcp_line(
        &mut buf,
        &serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    for request in requests {
        push_mcp_line(&mut buf, request);
    }
    buf
}

/// A `tools/list` request with the given id.
fn tools_list_request(id: i64) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/list"})
}

/// A `tools/call` request. `arguments` is omitted rather than sent as `{}`
/// when a tool takes none, matching what a real client sends.
fn call_tool_request(
    id: i64,
    name: &str,
    arguments: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut params = serde_json::json!({"name": name});
    if let Some(args) = arguments {
        params
            .as_object_mut()
            .expect("params is always an object")
            .insert("arguments".to_string(), args);
    }
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params})
}

/// Parses every line of `stdout` as JSON-RPC, panicking with the offending
/// line otherwise. A search for the wanted reply alone would pass with a stray
/// `println!` or a tracing record on the same wire. `str::lines` yields no
/// trailing empty entry, so an empty line is one the verb wrote.
fn assert_every_stdout_line_is_jsonrpc(stdout: &[u8]) -> Vec<serde_json::Value> {
    let text = String::from_utf8(stdout.to_vec()).expect("whistle's stdout is valid UTF-8");
    text.lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|err| panic!("stdout line is not JSON: {err}\nline: {line}"));
            assert_eq!(
                value.get("jsonrpc").and_then(serde_json::Value::as_str),
                Some("2.0"),
                "stdout line is not JSON-RPC: {line}"
            );
            value
        })
        .collect()
}

/// The reply among `lines` whose `"id"` matches, told apart from a request or
/// notification of the same shape by carrying `"result"` or `"error"`.
fn find_reply(lines: &[serde_json::Value], id: i64) -> &serde_json::Value {
    lines
        .iter()
        .find(|line| {
            line.get("id") == Some(&serde_json::Value::from(id))
                && (line.get("result").is_some() || line.get("error").is_some())
        })
        .unwrap_or_else(|| panic!("no reply with id {id} in {lines:#?}"))
}

/// A `shep` invocation reaching `$SHEP_HOME` through the environment rather
/// than `--home`; `GlobalArgs::home` carries `env = "SHEP_HOME"`.
fn shep_via_env(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("shep").unwrap();
    cmd.env("SHEP_HOME", home).timeout(CMD_TIMEOUT);
    cmd
}

/// Drives `cmd` (already carrying `--home` or `SHEP_HOME`, not yet the
/// `whistle` argument) through an `initialize` handshake and a
/// `tools/list`, and returns the tool names the gate produced.
fn whistle_tool_names(mut cmd: Command) -> Vec<String> {
    let stdin = mcp_session(&[tools_list_request(2)]);
    let output = cmd.arg("whistle").write_stdin(stdin).output().unwrap();
    assert_success(&output);
    let lines = assert_every_stdout_line_is_jsonrpc(&output.stdout);
    find_reply(&lines, 2)["result"]["tools"]
        .as_array()
        .expect("tools/list result carries a tools array")
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("every tool has a name")
                .to_string()
        })
        .collect()
}

/// Drives the real binary: an `initialize` and a `tools/list` request,
/// newline-delimited on stdin, replies read back from stdout. Every stdout
/// line must parse as JSON-RPC.
#[test]
fn whistle_speaks_mcp_and_writes_nothing_else_to_stdout() {
    let home = TempDir::new().unwrap();
    let stdin = mcp_session(&[tools_list_request(2)]);
    let output = shep(home.path())
        .arg("whistle")
        .write_stdin(stdin)
        .output()
        .unwrap();
    assert_success(&output);

    let lines = assert_every_stdout_line_is_jsonrpc(&output.stdout);

    let init_reply = find_reply(&lines, 1);
    assert_eq!(init_reply["result"]["serverInfo"]["name"], "shep");
    assert!(init_reply["result"]["capabilities"]["tools"].is_object());

    let list_reply = find_reply(&lines, 2);
    assert!(list_reply["result"]["tools"].is_array());
}

/// Three runs against two `$SHEP_HOME`s: no `[whistle]` section (five tools),
/// `allow_control = true` (nine), and that same open directory again through
/// `--home`. The split is checked by name, not only by count: a count alone
/// would pass if the gate registered a read tool twice.
#[test]
fn the_shep_toml_gate_decides_the_tool_list_in_a_real_process() {
    let control_tools = ["start_sheep", "stop_sheep", "restart_sheep", "reload_sheep"];

    let closed_home = TempDir::new().unwrap();
    let names = whistle_tool_names(shep_via_env(closed_home.path()));
    assert_eq!(names.len(), 5, "read-only: {names:?}");
    for tool in control_tools {
        assert!(
            !names.contains(&tool.to_string()),
            "{tool} must be absent: {names:?}"
        );
    }

    let open_home = TempDir::new().unwrap();
    write_shep_toml(&open_home, "[whistle]\nallow_control = true\n");

    let names = whistle_tool_names(shep_via_env(open_home.path()));
    assert_eq!(names.len(), 9, "gate open via env: {names:?}");
    for tool in control_tools {
        assert!(
            names.contains(&tool.to_string()),
            "{tool} must be present: {names:?}"
        );
    }

    let names = whistle_tool_names(shep(open_home.path()));
    assert_eq!(names.len(), 9, "gate open via --home: {names:?}");
    for tool in control_tools {
        assert!(
            names.contains(&tool.to_string()),
            "{tool} must be present: {names:?}"
        );
    }
}

/// The malformed-config notice is the only thing whistle writes outside the
/// JSON-RPC wire, and it sits next to the stdout handle. A config that fails
/// to parse leaves the gate shut.
#[test]
fn a_malformed_shep_toml_stays_off_stdout_and_keeps_the_gate_shut() {
    let home = TempDir::new().unwrap();
    write_shep_toml(&home, "[whistle\n");

    let stdin = mcp_session(&[tools_list_request(2)]);
    let output = shep(home.path())
        .arg("whistle")
        .write_stdin(stdin)
        .output()
        .unwrap();
    assert_success(&output);

    let lines = assert_every_stdout_line_is_jsonrpc(&output.stdout);
    let list_reply = find_reply(&lines, 2);
    let names: Vec<String> = list_reply["result"]["tools"]
        .as_array()
        .expect("tools/list result carries a tools array")
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("every tool has a name")
                .to_string()
        })
        .collect();

    assert_eq!(
        names.len(),
        5,
        "a broken config must read as the gate SHUT, not open: {names:?}"
    );
    for tool in ["start_sheep", "stop_sheep", "restart_sheep", "reload_sheep"] {
        assert!(
            !names.contains(&tool.to_string()),
            "{tool} must be absent when shep.toml fails to parse: {names:?}"
        );
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid_config"),
        "the malformed-config notice must reach stderr: {stderr}"
    );
    assert!(
        stderr.contains("shep.toml"),
        "the notice must name the file: {stderr}"
    );
}

/// With the gate shut, `tools/call` for `stop_sheep` answers JSON-RPC error
/// `-32602`, rmcp's answer for a name its router does not hold. A tool that
/// existed and refused would answer a `result`.
#[test]
fn a_gated_off_control_tool_is_not_merely_refused_it_is_absent() {
    let home = TempDir::new().unwrap();
    let stdin = mcp_session(&[call_tool_request(
        2,
        "stop_sheep",
        Some(serde_json::json!({"name": "api"})),
    )]);
    let output = shep(home.path())
        .arg("whistle")
        .write_stdin(stdin)
        .output()
        .unwrap();
    assert_success(&output);

    let lines = assert_every_stdout_line_is_jsonrpc(&output.stdout);
    let reply = find_reply(&lines, 2);
    assert!(
        reply.get("result").is_none(),
        "a gated-off tool must be a protocol error, not a result: {reply:#?}"
    );
    let error = reply
        .get("error")
        .expect("a gated-off tool call must answer a JSON-RPC error");
    assert_eq!(error["code"], -32602);
    assert_eq!(error["message"], "tool not found");
}

/// Whistle's transport is the launcher's, not the shepherd's, so it answers
/// `initialize` against a home with no daemon and no socket, and reports the
/// missing shepherd per call.
#[test]
fn whistle_starts_with_no_shepherd_and_reports_it_per_call() {
    let home = TempDir::new().unwrap();
    let stdin = mcp_session(&[call_tool_request(2, "list_flock", None)]);
    let output = shep(home.path())
        .arg("whistle")
        .write_stdin(stdin)
        .output()
        .unwrap();
    assert_success(&output);

    let lines = assert_every_stdout_line_is_jsonrpc(&output.stdout);

    let init_reply = find_reply(&lines, 1);
    assert_eq!(init_reply["result"]["serverInfo"]["name"], "shep");
    assert!(init_reply["result"]["capabilities"]["tools"].is_object());

    let call_reply = find_reply(&lines, 2);
    assert_eq!(call_reply["result"]["isError"], true);
    let message = call_reply["result"]["structuredContent"]["message"]
        .as_str()
        .expect("a no-shepherd refusal carries a message");
    assert!(
        message.contains("no shepherd is running"),
        "message: {message}"
    );
}
