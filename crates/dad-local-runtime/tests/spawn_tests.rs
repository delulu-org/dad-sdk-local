//! End-to-end spawn tests: build the example addons, spawn them EXACTLY the
//! way the host will (one JSON request line on stdin, one response line on
//! stdout), and assert the full protocol behavior.

use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};

fn request_line(id: u64, method: &str, params: Value) -> String {
    let mut line = serde_json::to_string(&json!({
        "jsonrpc": "2.0", "id": id, "protocol_version": "2.0",
        "method": method, "params": params
    }))
    .unwrap();
    line.push('\n');
    line
}

fn raw_line(body: &str) -> String {
    format!("{body}\n")
}

/// Spawns the addon binary with one request on stdin and collects the
/// response — the one-shot contract, exercised for real.
fn run_addon(exe: &str, stdin_text: &str) -> (Value, i32, String) {
    let mut child = Command::new(exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn example addon");

    {
        let mut stdin = child.stdin.take().expect("stdin piped");
        stdin.write_all(stdin_text.as_bytes()).expect("write request");
        // dropping `stdin` closes the pipe — the one-shot contract
    }

    let output = child.wait_with_output().expect("wait for addon exit");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);

    let first_line = stdout.lines().next().unwrap_or("");
    let parsed: Value = serde_json::from_str(first_line)
        .unwrap_or_else(|e| panic!("stdout was not one JSON response line ({e}): {stdout:?}"));
    (parsed, code, stderr)
}

#[test]
fn streams_roundtrip_direct_and_torrent() {
    let exe = std::env::var("CARGO_BIN_EXE_demo_open").expect("example binary built");
    let (response, code, _stderr) = run_addon(
        &exe,
        &request_line(1, "getStreams", json!({"tmdb_id": 1, "media_type": "movie"})),
    );
    assert_eq!(code, 0, "graceful answers still exit 0");
    assert_eq!(response["id"], json!(1));
    let result = response["result"].as_array().expect("result is a stream array");
    assert_eq!(result[0]["type"], json!("direct"));
    assert_eq!(result[0]["stream_url"], json!("https://cdn.example.com/demo.m3u8"));
    assert_eq!(result[0]["audio_languages"], json!([]));
    assert_eq!(result[1]["type"], json!("torrent"));
    assert_eq!(result[1]["file_idx"], json!(0));
    assert_eq!(result[1]["info_hash"], json!("a1b2c3d4e5f60718293a4b5c6d7e8f9012345678"));
    assert!(response.get("error").is_none());
}

#[test]
fn meta_roundtrip() {
    let exe = std::env::var("CARGO_BIN_EXE_demo_open").expect("example binary built");
    let (response, code, _) = run_addon(
        &exe,
        &request_line(2, "getMeta", json!({"tmdb_id": 550, "media_type": "movie"})),
    );
    assert_eq!(code, 0);
    assert_eq!(response["result"]["imdb_id"], json!("tt0111113"));
    assert_eq!(response["result"]["imdb_rating"], json!(8.8));
    assert_eq!(response["result"]["trailers"][0], json!("https://cdn.example.com/trailer.mp4"));
}

#[test]
fn graceful_error_passes_through() {
    let exe = std::env::var("CARGO_BIN_EXE_demo_open").expect("example binary built");
    let (response, code, _) = run_addon(
        &exe,
        &request_line(3, "getStreams", json!({"tmdb_id": 7, "media_type": "movie"})),
    );
    assert_eq!(code, 0, "a well-formed DAD error is a valid answer, exit 0");
    assert_eq!(response["error"]["code"], json!("content_unavailable"));
    assert_eq!(response["id"], json!(3));
}

#[test]
fn handler_panic_becomes_internal_error() {
    let exe = std::env::var("CARGO_BIN_EXE_demo_open").expect("example binary built");
    let (response, code, stderr) = run_addon(
        &exe,
        &request_line(4, "getStreams", json!({"tmdb_id": 13, "media_type": "movie"})),
    );
    assert_eq!(code, 0, "panic is contained: the process answers, not dies");
    assert_eq!(response["error"]["code"], json!("internal_error"));
    let message = response["error"]["error_message"].as_str().unwrap();
    assert!(!message.contains("demo panic"), "panic payload must not leak into stdout");
    assert!(stderr.contains("demo panic"), "panic details belong on stderr");
}

#[test]
fn invalid_handler_output_never_leaves_the_process() {
    let exe = std::env::var("CARGO_BIN_EXE_demo_open").expect("example binary built");
    let (response, code, _) = run_addon(
        &exe,
        &request_line(5, "getStreams", json!({"tmdb_id": 42, "media_type": "movie"})),
    );
    assert_eq!(code, 0);
    assert_eq!(response["error"]["code"], json!("invalid_response"));
    assert!(response["error"]["error_message"]
        .as_str()
        .unwrap()
        .contains("HTTPS"));
    assert!(response.get("result").is_none());
}

#[test]
fn unknown_method_is_not_found() {
    let exe = std::env::var("CARGO_BIN_EXE_demo_open").expect("example binary built");
    let (response, code, _) = run_addon(&exe, &request_line(6, "resolveStream", json!({})));
    assert_eq!(code, 0);
    assert_eq!(response["error"]["code"], json!("not_found"));
    assert!(response["error"]["error_message"]
        .as_str()
        .unwrap()
        .contains("declares capabilities"));
}

#[test]
fn movie_with_season_is_bad_request() {
    let exe = std::env::var("CARGO_BIN_EXE_demo_open").expect("example binary built");
    let (response, code, _) = run_addon(
        &exe,
        &request_line(7, "getStreams", json!({"tmdb_id": 550, "media_type": "movie", "s": 1})),
    );
    assert_eq!(code, 0);
    assert_eq!(response["error"]["code"], json!("bad_request"));
    assert!(response["error"]["error_message"].as_str().unwrap().contains("TV-only"));
}

#[test]
fn manifest_method_self_reports() {
    let exe = std::env::var("CARGO_BIN_EXE_demo_open").expect("example binary built");
    let (response, code, _) = run_addon(&exe, &request_line(8, "manifest", json!(null)));
    assert_eq!(code, 0);
    assert_eq!(response["result"]["id"], json!("org.example.demo-open"));
    assert_eq!(response["result"]["protocol_version"], json!("2.0"));
    assert_eq!(response["result"]["capabilities"], json!(["direct_stream", "torrent", "meta"]));
}

#[test]
fn health_check_answers_ok() {
    let exe = std::env::var("CARGO_BIN_EXE_demo_open").expect("example binary built");
    let (response, code, _) = run_addon(&exe, &request_line(9, "healthCheck", json!(null)));
    assert_eq!(code, 0);
    assert_eq!(response["result"]["ok"], json!(true));
    assert_eq!(response["result"]["addon_id"], json!("org.example.demo-open"));
}

#[test]
fn protocol_failures_answer_bad_request_with_null_id() {
    let exe = std::env::var("CARGO_BIN_EXE_demo_open").expect("example binary built");

    // Not JSON at all.
    let (response, _, _) = run_addon(&exe, &raw_line("this is not json"));
    assert_eq!(response["id"], json!(null));
    assert_eq!(response["error"]["code"], json!("bad_request"));

    // Wrong protocol version.
    let (response, _, _) = run_addon(
        &exe,
        &raw_line(r#"{"jsonrpc":"2.0","id":9,"protocol_version":"1.0","method":"getStreams"}"#),
    );
    assert_eq!(response["error"]["code"], json!("bad_request"));
    assert!(response["error"]["error_message"].as_str().unwrap().contains("protocol_version"));
}

#[test]
fn api_key_gate_blocks_before_the_handler() {
    let exe = std::env::var("CARGO_BIN_EXE_demo_gated").expect("example binary built");

    // No auth -> unauthorized, handler never invoked.
    let (response, code, _) = run_addon(
        &exe,
        &request_line(10, "getSubtitles", json!({"tmdb_id": 550, "media_type": "movie"})),
    );
    assert_eq!(code, 0);
    assert_eq!(response["error"]["code"], json!("unauthorized"));
    assert!(response["error"]["error_message"]
        .as_str()
        .unwrap()
        .contains("https://example.com/get-a-demo-key"));

    // A present-but-wrong key reaches the handler, which rejects it itself.
    let (response, _, _) = run_addon(
        &exe,
        &request_line(
            11,
            "getSubtitles",
            json!({"tmdb_id": 550, "media_type": "movie", "auth": "wrong-key"}),
        ),
    );
    assert_eq!(response["error"]["code"], json!("unauthorized"));
    assert_eq!(response["error"]["error_message"], json!("Missing or invalid API key"));

    // The right key unlocks real content.
    let (response, _, _) = run_addon(
        &exe,
        &request_line(
            12,
            "getSubtitles",
            json!({"tmdb_id": 550, "media_type": "movie", "auth": "good-key"}),
        ),
    );
    assert_eq!(response["result"][0]["format"], json!("vtt"));
    assert_eq!(response["result"][0]["lang_code"], json!("en"));
}

#[test]
fn empty_stdin_exits_nonzero_with_stderr_explanation() {
    let exe = std::env::var("CARGO_BIN_EXE_demo_open").expect("example binary built");
    let mut child = Command::new(exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    drop(child.stdin.take()); // close immediately: no request at all
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "no stdout noise on empty input");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("one-shot"), "stderr explains the model: {stderr}");
}
