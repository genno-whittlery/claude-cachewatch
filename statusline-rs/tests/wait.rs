//! End-to-end checks for the `wait` / `block` / `clear` subcommands. Each run
//! gets a throwaway TMPDIR (state dir) and a config-less HOME so `telegram_creds`
//! returns None — the curl send path is never taken, keeping the test hermetic
//! (no network) while still exercising sentinel writes + the hook exit contract.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

// Collision-proof suffix for the throwaway dirs: the parallel test threads all
// read the clock inside the same microsecond often enough that (pid, nanos)
// alone repeats — two tests then share a dir and the first remove_dir_all
// deletes it under the other.
static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

fn seq() -> u64 {
    DIR_SEQ.fetch_add(1, Ordering::Relaxed)
}

struct Run {
    tmp: PathBuf,
    code: i32,
}

fn run(verb: &str, payload: &str, now: i64, extra_env: &[(&str, &str)]) -> Run {
    let tmp = std::env::temp_dir().join(format!(
        "slrs-wait-{}-{}-{}",
        std::process::id(),
        seq(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp).unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_statusline-rs"));
    cmd.arg(verb)
        .env("TMPDIR", &tmp) // state dir lands under here
        .env("HOME", &tmp) // no ~/.config/cache-notify/env -> no Telegram
        .env("CLAUDE_STATUSLINE_NOW", now.to_string())
        .env_remove("ZELLIJ_SESSION_NAME")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn statusline-rs");
    child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    let code = child.wait().expect("wait").code().unwrap_or(-1);
    Run { tmp, code }
}

/// Find a `<sid>.waiting` sentinel anywhere under the throwaway TMPDIR.
fn sentinel_body(tmp: &PathBuf, sid: &str) -> Option<String> {
    for entry in std::fs::read_dir(tmp).ok()?.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("claude-cache-timer-") {
            let p = entry.path().join(format!("{sid}.waiting"));
            if let Ok(s) = std::fs::read_to_string(&p) {
                return Some(s);
            }
        }
    }
    None
}

fn payload(sid: &str) -> String {
    format!(r#"{{"session_id":"{sid}","cwd":"/tmp/nope","message":"run rm -rf"}}"#)
}

#[test]
fn block_writes_sentinel_and_exits_zero() {
    let sid = "sess-block";
    let r = run("block", &payload(sid), 1_000_000_000, &[]);
    assert_eq!(r.code, 0, "hook must exit 0");
    assert_eq!(
        sentinel_body(&r.tmp, sid).as_deref(),
        Some("block 1000000000\n"),
        "block writes the sentinel"
    );
    let _ = std::fs::remove_dir_all(&r.tmp);
}

#[test]
fn clear_removes_sentinel() {
    // First block to create the sentinel, then clear within the SAME state dir.
    // We reuse one tmp by pointing both runs at it via TMPDIR.
    let tmp = std::env::temp_dir().join(format!(
        "slrs-wait-clear-{}-{}-{}",
        std::process::id(),
        seq(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let sid = "sess-clear";

    let spawn = |verb: &str| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_statusline-rs"));
        cmd.arg(verb)
            .env("TMPDIR", &tmp)
            .env("HOME", &tmp)
            .env("CLAUDE_STATUSLINE_NOW", "1000000000")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().unwrap();
        child.stdin.take().unwrap().write_all(payload(sid).as_bytes()).unwrap();
        child.wait().unwrap().code().unwrap_or(-1)
    };

    assert_eq!(spawn("block"), 0);
    assert!(sentinel_body(&tmp, sid).is_some(), "sentinel created");
    assert_eq!(spawn("clear"), 0);
    assert!(sentinel_body(&tmp, sid).is_none(), "sentinel removed by clear");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn wait_with_notify_off_still_writes_sentinel() {
    let sid = "sess-wait";
    let r = run("wait", &payload(sid), 1_000_000_000, &[("CACHE_WAIT_NOTIFY", "0")]);
    assert_eq!(r.code, 0);
    assert_eq!(
        sentinel_body(&r.tmp, sid).as_deref(),
        Some("wait 1000000000\n"),
        "wait writes the sentinel even with notifications off"
    );
    let _ = std::fs::remove_dir_all(&r.tmp);
}

#[test]
fn empty_session_id_is_noop() {
    let r = run("block", r#"{"session_id":"","cwd":"/tmp/nope"}"#, 1_000_000_000, &[]);
    assert_eq!(r.code, 0, "still exits 0");
    // nothing written: no claude-cache-timer dir with a sentinel
    let any = std::fs::read_dir(&r.tmp)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("claude-cache-timer-"));
    assert!(!any || sentinel_body(&r.tmp, "").is_none(), "no sentinel for empty sid");
    let _ = std::fs::remove_dir_all(&r.tmp);
}
