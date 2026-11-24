//! On stdio, stdout is the JSON-RPC wire: the real binary, started with an
//! empty config, answers `initialize` and `tools/list` with JSON-RPC and
//! writes nothing else there. A positive control makes the process print a
//! stray line after startup, the way a dependency might, and it must not
//! reach the wire either.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_makinax-mcp")
}

/// How long the server may stay silent before a test fails as hung.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Read `child`'s stdout lines until one satisfies `done` (or 40 lines),
/// failing the test if no line arrives within [`TIMEOUT`].
fn read_until(child: &mut Child, done: impl Fn(&str) -> bool) -> Vec<String> {
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + TIMEOUT;
    let mut lines = Vec::new();
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(line) => {
                if line.trim().is_empty() {
                    continue;
                }
                let finished = done(&line);
                lines.push(line);
                if finished || lines.len() > 40 {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the server did not answer within {TIMEOUT:?}; read so far: {lines:?}");
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    lines
}

/// Drain a pipe on its own thread, so a full pipe can never block the
/// child.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    })
}

/// Run the binary with `args` to completion, failing the test if it takes
/// longer than [`TIMEOUT`].
fn run(args: &[&str]) -> Output {
    let mut child = Command::new(binary())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the binary");
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`{}` did not exit within {TIMEOUT:?}", args.join(" "));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Output {
        status,
        stdout: out.join().expect("stdout reader"),
        stderr: err.join().expect("stderr reader"),
    }
}

/// `MAKINAX_CONFIG` names the file, and a `MAKINAX__` env section overrides
/// one key: the module's chain gets its RPC from the environment.
#[test]
fn the_config_env_var_and_env_sections_are_read() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("elsewhere.toml");
    std::fs::write(
        &cfg,
        "[modules.main]\nchain = \"base\"\n\
         safe = \"0x1111111111111111111111111111111111111111\"\n\
         module = \"0x2222222222222222222222222222222222222222\"\n",
    )
    .unwrap();
    let mut child = Command::new(binary())
        .env_clear()
        .env("HOME", dir.path())
        .env("PATH", "/usr/bin:/bin")
        .env("MAKINAX_CONFIG", &cfg)
        .env("MAKINAX__RPC_URLS__BASE", "http://127.0.0.1:1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the server");
    {
        let stdin = child.stdin.as_mut().unwrap();
        for msg in [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_modules","arguments":{}}}"#,
        ] {
            stdin.write_all(msg.as_bytes()).unwrap();
            stdin.write_all(b"\n").unwrap();
        }
        stdin.flush().unwrap();
    }
    let lines = read_until(&mut child, |l| l.contains("\"id\":2"));
    let _ = child.kill();
    let _ = child.wait();
    let answer = lines
        .into_iter()
        .find(|l| l.contains("\"id\":2"))
        .expect("list_modules was answered");
    let v: serde_json::Value = serde_json::from_str(&answer).unwrap();
    let text = v["result"]["content"][0]["text"].as_str().expect("text");
    let modules: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(modules[0]["name"], "main", "{text}");
    assert_eq!(modules[0]["rpc_configured"], true, "{text}");
}

/// Run the binary over stdio and return every non-empty stdout line, and
/// everything it wrote to stderr.
fn handshake(stray: bool) -> (Vec<String>, String) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "").unwrap();

    let mut cmd = Command::new(binary());
    cmd.arg("--config")
        .arg(&cfg)
        .env_clear()
        .env("HOME", dir.path())
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stray {
        cmd.env("MAKINAX_MCP_TEST_STRAY_STDOUT", "1");
    }
    let mut child = cmd.spawn().expect("spawn the server");
    let stderr = drain(child.stderr.take());
    {
        let stdin = child.stdin.as_mut().unwrap();
        stdin
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"t\",\"version\":\"1\"}}}\n",
            )
            .unwrap();
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .unwrap();
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n")
            .unwrap();
        stdin.flush().unwrap();
    }
    let lines = read_until(&mut child, |l| l.contains("\"id\":2"));
    let _ = child.kill();
    let _ = child.wait();
    let stderr = String::from_utf8_lossy(&stderr.join().expect("stderr reader")).into_owned();
    (lines, stderr)
}

fn assert_only_json_rpc(lines: &[String]) {
    assert!(
        lines.iter().any(|l| l.contains("\"id\":1")),
        "initialize was never answered: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("\"id\":2")),
        "tools/list was never answered: {lines:?}"
    );
    for line in lines {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("a non-JSON line reached the wire: {e}\n  {line}"));
        assert_eq!(v["jsonrpc"], "2.0", "not JSON-RPC: {line}");
    }
}

#[test]
fn stdout_carries_json_rpc_and_nothing_else() {
    let (lines, _) = handshake(false);
    assert_only_json_rpc(&lines);
    let list = lines.iter().find(|l| l.contains("\"id\":2")).unwrap();
    let v: serde_json::Value = serde_json::from_str(list).unwrap();
    let tools = v["result"]["tools"].as_array().expect("a tool list");
    let want = if cfg!(feature = "write") { 19 } else { 18 };
    assert_eq!(tools.len(), want, "{list}");
}

#[test]
fn a_stray_print_never_reaches_the_wire() {
    let (lines, stderr) = handshake(true);
    assert_only_json_rpc(&lines);
    for line in &lines {
        assert!(
            !line.contains("STRAY-NOT-JSON"),
            "stray print on the wire: {line}"
        );
    }
    // The control fired: the print happened, and landed on stderr. The
    // hook exists in debug builds only.
    if cfg!(debug_assertions) {
        assert!(
            stderr.contains("STRAY-NOT-JSON"),
            "the stray print never happened, so this proves nothing: {stderr}"
        );
    }
}

#[test]
fn version_names_the_tool_and_the_build() {
    let out = run(&["--version"]);
    assert!(out.status.success());
    let s = String::from_utf8(out.stdout).unwrap();
    let variant = if cfg!(feature = "write") {
        "[read-write]"
    } else {
        "[read-only]"
    };
    // Exactly `makinax-mcp <version> (<12 lowercase hex>) [<variant>]`, one
    // line. A working tree with uncommitted changes adds `-dirty` to the
    // commit; a release is built from a clean checkout and never has it.
    let line = s.strip_suffix('\n').unwrap_or(&s);
    let rest = line
        .strip_prefix(&format!("makinax-mcp {} (", env!("CARGO_PKG_VERSION")))
        .unwrap_or_else(|| panic!("wrong name or version: {s:?}"));
    let (commit, tail) = rest
        .split_once(") ")
        .unwrap_or_else(|| panic!("no commit: {s:?}"));
    assert_eq!(tail, variant, "{s:?}");
    let sha = commit.strip_suffix("-dirty").unwrap_or(commit);
    assert!(
        sha.len() == 12
            && sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "the commit must be 12 lowercase hex digits: {s:?}"
    );
}

#[test]
fn tools_prints_the_tool_list_as_json() {
    let out = run(&["tools"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let n = v.as_array().expect("an array").len();
    assert_eq!(n, if cfg!(feature = "write") { 19 } else { 18 });
}

#[test]
fn an_unknown_argument_is_refused_before_anything_starts() {
    let out = run(&["--nope"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--nope"));
}
