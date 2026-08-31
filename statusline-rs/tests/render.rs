//! End-to-end render checks: run the compiled binary, feed it a statusline
//! JSON payload on stdin, assert the printed line. Time is pinned via
//! CLAUDE_STATUSLINE_NOW and state dirs are pointed at a throwaway TMPDIR so
//! no real cache/quota files leak in.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run the binary with `payload` on stdin and a clean env, return stdout.
fn run(payload: &str, now: i64) -> String {
    let tmp = std::env::temp_dir().join(format!(
        "slrs-it-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_statusline-rs"))
        .env("TMPDIR", &tmp) // isolate cache/quota state
        .env("CLAUDE_STATUSLINE_NOW", now.to_string()) // pin the clock
        .env("CLAUDE_CONFIG_DIR", ".claude-it") // a profile with no usage file
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn statusline-rs");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success(), "binary exited non-zero");
    String::from_utf8(out.stdout).expect("utf8 stdout")
}

#[test]
fn fresh_context_renders_marker_model_and_fresh() {
    let payload = r#"{"model":{"display_name":"Opus"},
        "context_window":{"used_percentage":0,"total_input_tokens":0},
        "session_id":"","transcript_path":""}"#;
    let out = run(payload, 1_000_000_000);
    assert!(out.contains("rs.v2"), "build marker present: {out:?}");
    assert!(out.contains("Opus"), "model name present: {out:?}");
    assert!(out.contains("0%"), "context percent present: {out:?}");
    assert!(out.contains("fresh"), "used==0 shows 'fresh': {out:?}");
    assert!(out.ends_with(' '), "trailing space mirrors bash printf: {out:?}");
}

#[test]
fn half_full_bar_and_no_quota_without_file() {
    let payload = r#"{"model":{"display_name":"Sonnet"},
        "context_window":{"used_percentage":50,"total_input_tokens":12345},
        "session_id":"","transcript_path":""}"#;
    let out = run(payload, 1_000_000_000);
    assert!(out.contains("50%"), "percent present: {out:?}");
    // 50% of an 8-cell bar = 4 filled.
    assert!(out.contains("████░░░░"), "bar half filled: {out:?}");
    // No usage file for .claude-it -> quota suppressed, so no countdown digits.
    assert!(!out.contains('/'), "no quota readout without a usage file: {out:?}");
}

#[test]
fn garbage_stdin_degrades_to_claude() {
    let out = run("this is not json", 1_000_000_000);
    // serde parse fails -> Value::Null -> model defaults to "Claude".
    assert!(out.contains("Claude"), "degrades to default model: {out:?}");
}
