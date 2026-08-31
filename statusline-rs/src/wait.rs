//! Session-wait notifications — the Rust port of `session-wait-state.py`.
//!
//! Driven by three settings.json hooks, each passing its verb as argv[1] and the
//! hook payload (session_id, cwd, transcript_path, message) as JSON on stdin:
//!
//!   Stop              -> "wait"   : turn finished, Claude is idle waiting for input
//!   Notification      -> "block"  : Claude is blocked on a permission prompt / nudge
//!   UserPromptSubmit  -> "clear"  : the human responded; the session is busy again
//!
//! On a wait/block transition it writes a `<session_id>.waiting` sentinel beside
//! the statusline's cache-timer files (so `cachewatch` can show a WAIT column) and
//! fires a detached Telegram push using the SAME bot config the statusline
//! cache-cold alert used (~/.config/cache-notify/env, TG_TOKEN + TG_CHAT).
//!
//! This is deliberately SEPARATE from the cache-expiry (180s countdown) alert,
//! which is NOT ported: the no-arg statusline render path stays side-effect-free.
//!
//! `CACHE_WAIT_NOTIFY=0` silences the per-turn "waiting" pings (the "blocked"
//! permission pings still fire). Inert until the config file exists. Hooks must
//! never fail a turn, so every path returns exit 0.

use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Bytes of transcript tail scanned for the last assistant line (the "finished"
/// snippet). The last line is always complete (we read to EOF); this only bounds
/// how far back a single oversized line could be recovered.
const SNIPPET_TAIL_BYTES: u64 = 131_072;

/// Entry point for the `wait` / `block` / `clear` subcommands. Always returns 0
/// (a hook must never fail the turn); errors degrade to silent no-ops.
pub fn run(verb: &str) -> i32 {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let payload: Value = serde_json::from_str(&input).unwrap_or(Value::Null);

    let sid = payload["session_id"].as_str().unwrap_or("").trim().to_string();
    if sid.is_empty() || sid.contains('/') || sid == "." || sid == ".." {
        return 0;
    }

    let dir = crate::state_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return 0;
    }
    let now = crate::now_epoch();

    match verb {
        "clear" => clear_state(&dir, &sid),
        "wait" | "block" => {
            write_state(&dir, &sid, verb, now);
            prune_old(&dir, now);
            notify(verb, &sid, &payload);
        }
        _ => {} // unknown verb: no-op (main only dispatches the three above)
    }
    0
}

/// `<sid>.waiting` path inside the cache-timer dir.
fn sentinel(dir: &Path, sid: &str) -> std::path::PathBuf {
    dir.join(format!("{sid}.waiting"))
}

/// Record "<state> <epoch>" atomically (write-then-rename, via the shared helper
/// which appends the trailing newline) so a concurrent cachewatch read never sees
/// a partial file.
fn write_state(dir: &Path, sid: &str, state: &str, now: i64) {
    crate::atomic_write(&sentinel(dir, sid), &format!("{state} {now}"));
}

/// Remove the sentinel; missing file is fine.
fn clear_state(dir: &Path, sid: &str) {
    let _ = std::fs::remove_file(sentinel(dir, sid));
}

/// Prune `*.waiting` sentinels older than a day, mirroring the statusline cleanup.
fn prune_old(dir: &Path, now: i64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let cutoff = now - 86_400;
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("waiting") {
            continue;
        }
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(now);
        if mtime < cutoff {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Fire a detached Telegram push; silently no-op on any problem.
fn notify(state: &str, sid: &str, payload: &Value) {
    let cwn = std::env::var("CACHE_WAIT_NOTIFY").ok();
    if wait_suppressed(state, cwn.as_deref()) {
        return;
    }
    let Some((token, chat)) = telegram_creds() else { return };
    let cwd = resolve_cwd(payload);
    let tab = tab_label(&cwd, sid);
    let proj = proj_name(&cwd);
    // For the "finished" ping, lead with what Claude last said so the turn can be
    // triaged from the phone; "block" already carries the permission detail.
    let summary = if state == "wait" {
        last_assistant_snippet(payload)
    } else {
        None
    };
    let msg = compose_msg(state, &tab, payload["message"].as_str(), &proj, summary.as_deref());
    spawn_curl(&token, &chat, &msg);
}

/// The per-turn "waiting" ping is opt-out via CACHE_WAIT_NOTIFY=0; "block"
/// permission pings always fire.
fn wait_suppressed(state: &str, cache_wait_notify: Option<&str>) -> bool {
    state == "wait" && cache_wait_notify == Some("0")
}

/// The phone-push message.
/// - prefix: the `[tab]` session label (omitted when empty)
/// - "block": carries the permission detail from the hook `message`
/// - "wait": leads with `summary` (the last-assistant snippet, quoted) when
///   present, else the generic "waiting for your input"
/// - suffix: ` · {proj}` for at-a-glance repo context, dropped when `proj` is
///   empty or equals the tab label (no redundant `[infra] … · infra`)
fn compose_msg(
    state: &str,
    tab: &str,
    message: Option<&str>,
    proj: &str,
    summary: Option<&str>,
) -> String {
    let prefix = if tab.is_empty() {
        String::new()
    } else {
        format!("[{tab}] ")
    };
    let p = proj.trim();
    let suffix = if p.is_empty() || p.eq_ignore_ascii_case(tab.trim()) {
        String::new()
    } else {
        format!(" · {p}")
    };
    if state == "block" {
        let detail = message
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or("blocked on a prompt");
        format!("{prefix}Claude needs you — {detail}{suffix}")
    } else {
        let tail = summary
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("\"{s}\""))
            .unwrap_or_else(|| "waiting for your input".to_string());
        format!("{prefix}Claude finished — {tail}{suffix}")
    }
}

/// Basename of the session cwd ("riddlery"); "" when empty/root. Pure.
fn proj_name(cwd: &str) -> String {
    Path::new(cwd.trim())
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

/// Parse TG_TOKEN / TG_CHAT from the shell-style cache-notify env file. Both must
/// be present and non-empty.
fn parse_creds(text: &str) -> Option<(String, String)> {
    let mut token: Option<String> = None;
    let mut chat: Option<String> = None;
    for raw in text.lines() {
        let mut line = raw.trim();
        if let Some(stripped) = line.strip_prefix("export ") {
            line = stripped.trim_start();
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim().trim_matches(|c| c == '\'' || c == '"').to_string();
        match k.trim() {
            "TG_TOKEN" => token = Some(v),
            "TG_CHAT" => chat = Some(v),
            _ => {}
        }
    }
    match (token, chat) {
        (Some(t), Some(c)) if !t.is_empty() && !c.is_empty() => Some((t, c)),
        _ => None,
    }
}

/// Read + parse ~/.config/cache-notify/env. None if absent/unreadable/incomplete.
fn telegram_creds() -> Option<(String, String)> {
    let home = std::env::var("HOME").ok()?;
    let cfg = Path::new(&home).join(".config").join("cache-notify").join("env");
    let text = std::fs::read_to_string(cfg).ok()?;
    parse_creds(&text)
}

/// The session's working dir: prefer the payload's cwd, else the last cwd recorded
/// in the transcript tail (the Notification hook may omit cwd).
fn resolve_cwd(payload: &Value) -> String {
    let cwd = payload["cwd"].as_str().unwrap_or("").trim();
    if !cwd.is_empty() {
        return cwd.to_string();
    }
    let tp = payload["transcript_path"].as_str().unwrap_or("").trim();
    if tp.is_empty() {
        return String::new();
    }
    let Ok(mut f) = std::fs::File::open(tp) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(65_536);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&buf);
    last_cwd_in(&text)
}

/// Last `"cwd":"…"` value in a transcript chunk (compact JSON form).
fn last_cwd_in(text: &str) -> String {
    let mut last = String::new();
    for line in text.lines() {
        if let Some(i) = line.find("\"cwd\":\"") {
            let rest = &line[i + 7..];
            if let Some(j) = rest.find('"') {
                last = rest[..j].to_string();
            }
        }
    }
    last
}

/// One-line snippet of the LAST assistant message's text from the transcript
/// tail — what leads the "finished" ping so a turn can be triaged from the phone.
/// None when there's no transcript or no assistant text yet.
fn last_assistant_snippet(payload: &Value) -> Option<String> {
    let tp = payload["transcript_path"].as_str().unwrap_or("").trim();
    if tp.is_empty() {
        return None;
    }
    let mut f = std::fs::File::open(tp).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(SNIPPET_TAIL_BYTES);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    extract_assistant_snippet(&String::from_utf8_lossy(&buf))
}

/// Pure core of `last_assistant_snippet`: scan JSONL lines, keep the text of the
/// LAST `"type":"assistant"` entry that has any text block, return its first
/// non-empty line truncated. A partial leading line (tail cut mid-JSON) fails to
/// parse and is skipped.
fn extract_assistant_snippet(text: &str) -> Option<String> {
    let mut snippet: Option<String> = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["type"].as_str() != Some("assistant") {
            continue;
        }
        let Some(content) = v["message"]["content"].as_array() else {
            continue;
        };
        let mut buf = String::new();
        for block in content {
            if block["type"].as_str() == Some("text") {
                if let Some(t) = block["text"].as_str() {
                    buf.push_str(t);
                }
            }
        }
        if let Some(first) = buf.lines().map(str::trim).find(|l| !l.is_empty()) {
            snippet = Some(truncate_snippet(first)); // last assistant with text wins
        }
    }
    snippet
}

/// Trim and cap to 80 chars on a char boundary, appending "…" when cut.
fn truncate_snippet(s: &str) -> String {
    const MAX: usize = 80;
    let s = s.trim();
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX).collect();
    format!("{}…", head.trim_end())
}

/// The /tab sidecar label for this session, else the zellij session name — the
/// same fallback chain statusline.sh uses.
fn tab_label(cwd: &str, sid: &str) -> String {
    for base in [cwd, "."] {
        if base.is_empty() {
            continue;
        }
        let p = Path::new(base).join(".claude").join("sessions").join(sid);
        if let Ok(s) = std::fs::read_to_string(&p) {
            if let Some(first) = s.lines().next() {
                let t = first.trim();
                if !t.is_empty() {
                    return t.to_string();
                }
            }
        }
    }
    std::env::var("ZELLIJ_SESSION_NAME")
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Detached `curl` to the Telegram sendMessage endpoint: setsid()'d into its own
/// session and stdio-nulled so it can never hold the hook's pipe; we never wait,
/// so the hook returns instantly (mirrors the Python start_new_session=True).
fn spawn_curl(token: &str, chat: &str, msg: &str) {
    use std::os::unix::process::CommandExt;
    let url = format!("https://api.telegram.org/bot{token}/sendMessage");
    let mut cmd = std::process::Command::new("curl");
    cmd.args([
        "-s",
        "--max-time",
        "5",
        &url,
        "--data-urlencode",
        &format!("chat_id={chat}"),
        "--data-urlencode",
        &format!("text={msg}"),
    ])
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null());
    // SAFETY: setsid() is async-signal-safe and the only call in the child
    // between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let _ = cmd.spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parse_creds_basic_export_and_quotes() {
        assert_eq!(
            parse_creds("TG_TOKEN=abc\nTG_CHAT=123"),
            Some(("abc".into(), "123".into()))
        );
        assert_eq!(
            parse_creds("export TG_TOKEN='abc'\nexport TG_CHAT=\"123\""),
            Some(("abc".into(), "123".into()))
        );
        // surrounding whitespace + an unrelated line are tolerated
        assert_eq!(
            parse_creds("# notify\n  TG_TOKEN = abc \nOTHER=x\nTG_CHAT= 123 "),
            Some(("abc".into(), "123".into()))
        );
    }

    #[test]
    fn parse_creds_incomplete_is_none() {
        assert_eq!(parse_creds("TG_TOKEN=abc"), None); // no chat
        assert_eq!(parse_creds("TG_CHAT=123"), None); // no token
        assert_eq!(parse_creds("TG_TOKEN=\nTG_CHAT=123"), None); // empty token
        assert_eq!(parse_creds(""), None);
    }

    #[test]
    fn compose_msg_block_and_wait() {
        assert_eq!(
            compose_msg("block", "snake", Some("permission to run rm"), "", None),
            "[snake] Claude needs you — permission to run rm"
        );
        // empty / whitespace / missing message -> default detail
        assert_eq!(
            compose_msg("block", "snake", Some("   "), "", None),
            "[snake] Claude needs you — blocked on a prompt"
        );
        assert_eq!(
            compose_msg("block", "snake", None, "", None),
            "[snake] Claude needs you — blocked on a prompt"
        );
        assert_eq!(
            compose_msg("wait", "snake", None, "", None),
            "[snake] Claude finished — waiting for your input"
        );
        // no tab -> no prefix
        assert_eq!(
            compose_msg("wait", "", None, "", None),
            "Claude finished — waiting for your input"
        );
    }

    #[test]
    fn compose_msg_proj_suffix_and_summary() {
        // wait + summary -> the snippet (quoted) replaces the generic tail
        assert_eq!(
            compose_msg("wait", "Server", None, "riddlery", Some("committed & FF-merged to main")),
            "[Server] Claude finished — \"committed & FF-merged to main\" · riddlery"
        );
        // proj == tab (case-insensitive) -> no redundant suffix
        assert_eq!(
            compose_msg("wait", "infra", None, "Infra", None),
            "[infra] Claude finished — waiting for your input"
        );
        // no tab, proj present -> suffix still gives context
        assert_eq!(
            compose_msg("wait", "", None, "riddlery", None),
            "Claude finished — waiting for your input · riddlery"
        );
        // block keeps its detail and also gains the proj suffix
        assert_eq!(
            compose_msg("block", "Server", Some("run rm -rf"), "riddlery", None),
            "[Server] Claude needs you — run rm -rf · riddlery"
        );
        // empty/whitespace summary falls back to the generic tail
        assert_eq!(
            compose_msg("wait", "x", None, "", Some("   ")),
            "[x] Claude finished — waiting for your input"
        );
    }

    #[test]
    fn proj_name_basename() {
        assert_eq!(proj_name("/Users/alice/proj/riddlery"), "riddlery");
        assert_eq!(proj_name("/Users/alice/proj/riddlery/"), "riddlery"); // trailing slash
        assert_eq!(proj_name("  /a/b  "), "b"); // trims surrounding ws
        assert_eq!(proj_name(""), "");
        assert_eq!(proj_name("/"), "");
    }

    #[test]
    fn truncate_snippet_caps_and_ellipsizes() {
        assert_eq!(truncate_snippet("short"), "short");
        assert_eq!(truncate_snippet("  padded  "), "padded"); // trims
        let long = "a".repeat(100);
        let out = truncate_snippet(&long);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 81); // 80 + ellipsis
        // multibyte chars: char-boundary safe, no panic
        let multi = "é".repeat(100);
        let o = truncate_snippet(&multi);
        assert!(o.ends_with('…'));
        assert_eq!(o.chars().count(), 81);
    }

    #[test]
    fn extract_assistant_snippet_last_text_first_line() {
        let chunk = r#"{"type":"user","message":{"content":[{"type":"text","text":"hi"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"first answer"}]}}
{"type":"user","message":{"content":[{"type":"text","text":"more"}]}}
{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"Done — committed to main\nsecond line"}]}}"#;
        assert_eq!(
            extract_assistant_snippet(chunk).as_deref(),
            Some("Done — committed to main")
        );
    }

    #[test]
    fn extract_assistant_snippet_skips_partial_and_tool_only() {
        // leading line is a tail-cut partial (unparseable) -> skipped;
        // the only complete assistant entry is tool-only -> no text -> None
        let chunk = r#"pe":"assistant","message":{"content":[{"type":"text","text":"junk"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#;
        assert_eq!(extract_assistant_snippet(chunk), None);
    }

    #[test]
    fn extract_assistant_snippet_none_without_assistant() {
        let chunk = r#"{"type":"user","message":{"content":[{"type":"text","text":"hi"}]}}"#;
        assert_eq!(extract_assistant_snippet(chunk), None);
    }

    #[test]
    fn wait_suppressed_only_for_wait_when_zero() {
        assert!(wait_suppressed("wait", Some("0")));
        assert!(!wait_suppressed("wait", Some("1")));
        assert!(!wait_suppressed("wait", None));
        assert!(!wait_suppressed("block", Some("0"))); // block always fires
    }

    #[test]
    fn last_cwd_in_takes_last_match() {
        let t = r#"{"cwd":"/a","type":"user"}
{"type":"assistant"}
{"cwd":"/b/c","type":"user"}"#;
        assert_eq!(last_cwd_in(t), "/b/c");
        assert_eq!(last_cwd_in("{\"type\":\"user\"}"), "");
    }

    #[test]
    fn write_then_clear_round_trip() {
        let dir = unique_dir("wait-state");
        let sid = "sess-abc";
        write_state(&dir, sid, "block", 1_000_000_000);
        let body = std::fs::read_to_string(sentinel(&dir, sid)).unwrap();
        assert_eq!(body, "block 1000000000\n");

        clear_state(&dir, sid);
        assert!(!sentinel(&dir, sid).exists());
        // clearing a missing sentinel is a silent no-op
        clear_state(&dir, sid);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_removes_only_stale_waiting_files() {
        let dir = unique_dir("wait-prune");
        // prune keys on file MTIME, so `now` must track the wall clock the files
        // were written under (a fixed far-future `now` would prune everything).
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        // fresh + stale sentinels, plus a non-.waiting file that must survive
        write_state(&dir, "fresh", "wait", now);
        write_state(&dir, "stale", "wait", now);
        std::fs::write(dir.join("keep.txt"), "x").unwrap();
        // backdate the stale one well past the 1-day cutoff
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(200_000);
        set_mtime(&sentinel(&dir, "stale"), old);

        prune_old(&dir, now);

        assert!(sentinel(&dir, "fresh").exists(), "fresh sentinel kept");
        assert!(!sentinel(&dir, "stale").exists(), "stale sentinel pruned");
        assert!(dir.join("keep.txt").exists(), "non-.waiting file untouched");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- helpers ----------------------------------------------------------
    fn unique_dir(tag: &str) -> std::path::PathBuf {
        let ns = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        // Fixed /tmp base, NOT temp_dir(): temp_dir() re-reads $TMPDIR, which
        // main.rs's quota_segment_paths mutates process-wide — this module's
        // tests run in the same process and raced it (prune test's dir was
        // created inside the quota dir and deleted by its cleanup).
        let d = std::path::PathBuf::from("/tmp").join(format!("slrs-{tag}-{}-{ns}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn set_mtime(p: &Path, t: SystemTime) {
        // touch via filetime-free approach: reopen + set times through libc utimes
        let secs = t.duration_since(UNIX_EPOCH).unwrap().as_secs() as libc::time_t;
        let tv = libc::timeval { tv_sec: secs, tv_usec: 0 };
        let times = [tv, tv];
        let cpath = std::ffi::CString::new(p.as_os_str().to_str().unwrap()).unwrap();
        unsafe {
            libc::utimes(cpath.as_ptr(), times.as_ptr());
        }
    }
}
