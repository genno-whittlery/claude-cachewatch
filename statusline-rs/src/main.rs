//! statusline-rs — a render-only Rust prototype of `statusline.sh`.
//!
//! Purpose
//! -------
//! A native counter: it reproduces the *visible* status line (model + 14-cell
//! bar + percentage + ⏱ prompt-cache countdown) byte-for-byte against the bash
//! script, including the per-session fingerprint state file and the
//! mtime-memoized transcript scan. Current visuals: an 8-cell context bar, the
//! cache TTL shown bare as "{N}s" / "cold" / "fresh" (the VS16 stopwatch glyph
//! was REMOVED — its ambiguous width destabilized the layout), no "·" dot
//! between the context % and the cache value, and a two-space gap before the
//! 5h/7d quota readout.
//!
//! The RENDER path (no-arg invocation) is still side-effect-free: NO DB call, NO
//! subprocess spawn, and NO `jq` — it cannot stall a 1Hz render. The cache-expiry
//! (180s countdown) notification, the macOS banner, and the agents.py SQLite write
//! from statusline.sh remain DELIBERATELY OMITTED here.
//!
//! The binary ALSO serves the session-wait hooks as subcommands —
//! `statusline-rs wait|block|clear` (see `wait.rs`) — which fire the
//! "Claude finished / Claude needs you" Telegram pushes that statusline.sh's
//! sibling `session-wait-state.py` used to. Those run only on the explicit verb
//! and never on the render path.
//!
//! Reads the statusline JSON on stdin, prints one line on stdout. Matches the
//! bash logic in statusline.sh as of the mtime-memoization fix.

use std::io::{Read, Seek, SeekFrom};
use std::time::{SystemTime, UNIX_EPOCH};

mod wait;

const C_RST: &str = "\x1b[0m";
const C_BOLD: &str = "\x1b[1m";
const C_DIM: &str = "\x1b[2m";
const C_GREEN: &str = "\x1b[32m";
const C_YELLOW: &str = "\x1b[33m";
const C_RED: &str = "\x1b[31m";

const CELLS: usize = 8;
const TTL: i64 = 300; // prompt-cache lifetime (s)
// Build marker so it's visible at a glance which statusline binary is live
// (vs the bash statusline.sh). Bump on meaningful render changes.
const VER: &str = "rs.v2";
const TAIL_BYTES: u64 = 131_072; // last 128KB of the transcript, like `tail -c`

fn main() {
    // Hook subcommand mode: `statusline-rs wait|block|clear` ports the
    // session-wait-state Telegram alerts (Stop/Notification/UserPromptSubmit
    // hooks) into this binary. With no recognized verb we fall through to the
    // normal statusline render below — that path stays free of the cache-expiry
    // (180s countdown) notification by design.
    if let Some(verb) = std::env::args().nth(1) {
        if matches!(verb.as_str(), "wait" | "block" | "clear") {
            std::process::exit(wait::run(&verb));
        }
    }

    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        // mirror bash: degrade rather than error
        print!("Claude");
        return;
    }
    let v: serde_json::Value = serde_json::from_str(&input).unwrap_or(serde_json::Value::Null);

    // --- extract fields, null-safe (mirrors the jq block) -------------------
    let model = v["model"]["display_name"].as_str().unwrap_or("Claude").to_string();
    let cw = &v["context_window"];
    let pct_f = json_num(&cw["used_percentage"]); // may be fractional
    let used = json_num(&cw["total_input_tokens"]) as i64;
    let transcript = v["transcript_path"].as_str().unwrap_or("").to_string();
    let sid = v["session_id"].as_str().unwrap_or("").to_string();

    let mut pct_i = pct_f.trunc() as i64;
    if pct_i < 0 {
        pct_i = 0;
    }
    if pct_i > 100 {
        pct_i = 100;
    }

    // --- progress bar -------------------------------------------------------
    let filled = ((pct_i as usize) * CELLS / 100).min(CELLS);
    let mut bar = String::with_capacity(CELLS * 3);
    for _ in 0..filled {
        bar.push('█');
    }
    for _ in 0..(CELLS - filled) {
        bar.push('░');
    }

    // --- color threshold (green <70<= yellow <90<= red) ---------------------
    let col = if pct_i >= 90 {
        C_RED
    } else if pct_i >= 70 {
        C_YELLOW
    } else {
        C_GREEN
    };

    // --- prompt-cache TTL ---------------------------------------------------
    let now = now_epoch();
    let cache = cache_segment(&sid, &transcript, used, now);

    // --- subscriber rate-limit quota (5h / 7d) ------------------------------
    // Source is the out-of-band poller (claude-usage-poll.py via launchd), NOT
    // the payload's per-session rate_limits snapshot — the poller hits the same
    // authoritative /api/oauth/usage endpoint as the in-app `/usage` command, so
    // the number is live and account-wide rather than frozen per session.
    let quota = quota_segment(now);

    // Trailing space mirrors the bash printf's final literal space.
    print!(
        "{C_DIM}{VER}{C_RST} {C_BOLD}{model}{C_RST}  {col}{bar} {pct_i}%{C_RST}{cache}{quota} "
    );
}

/// Build the "  42%/18%  8:34/134:34" quota trailer from the poller's per-profile
/// usage file: 5h/7d used-% then H:MM until each resets.
///
/// The file is written by claude-usage-poll.py at
/// $TMPDIR/claude-usage-<uid>/<profile>.json, keyed by basename($CLAUDE_CONFIG_DIR)
/// (".claude-sol", …), shape: {"5h":{"pct":N,"reset":epoch}, "7d":{...}, "at":epoch}.
///
/// Suppressed (returns "") when: no file (profile not polled — e.g. keychain-only
/// token), the data is stale (poller hasn't refreshed in >10 min — dead daemon),
/// or both windows are missing/already reset. A single window past its reset
/// renders as "—".
fn quota_segment(now: i64) -> String {
    let path = usage_dir().join(format!("{}.json", config_profile()));
    let txt = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    let v: serde_json::Value = serde_json::from_str(&txt).unwrap_or(serde_json::Value::Null);
    // dead-daemon guard: if the poll is older than 10 min, don't show stale %.
    let at = v["at"].as_i64().unwrap_or(0);
    if at != 0 && now - at > 600 {
        return String::new();
    }
    // A window is "live" only if it has a percentage AND a future reset.
    let live = |w: &serde_json::Value| -> Option<(i64, i64)> {
        let p = w["pct"].as_i64()?;
        let r = w["reset"].as_i64()?;
        if r <= now {
            return None;
        }
        Some((p, r))
    };
    let w5 = live(&v["5h"]);
    let w7 = live(&v["7d"]);
    if w5.is_none() && w7.is_none() {
        return String::new();
    }
    let pct = |w: Option<(i64, i64)>| w.map(|(p, _)| format!("{p}%")).unwrap_or_else(|| "—".into());
    let cd = |w: Option<(i64, i64)>| w.map(|(_, r)| hm(r - now)).unwrap_or_else(|| "—".into());
    let c5 = qcol(w5.map(|(p, _)| p));
    let c7 = qcol(w7.map(|(p, _)| p));
    format!(
        "  {c5}{}{C_RST}{C_DIM}/{C_RST}{c7}{}{C_RST}  {C_DIM}{}/{}{C_RST}",
        pct(w5), pct(w7), cd(w5), cd(w7)
    )
}

/// Per-user usage dir written by the poller: $TMPDIR/claude-usage-<uid>/.
fn usage_dir() -> std::path::PathBuf {
    let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
    let uid = unsafe { libc::getuid() };
    std::path::Path::new(&base).join(format!("claude-usage-{uid}"))
}

/// The active profile key = basename of $CLAUDE_CONFIG_DIR (".claude-sol", …),
/// falling back to ".claude" when the var is unset.
fn config_profile() -> String {
    let dir = std::env::var("CLAUDE_CONFIG_DIR").unwrap_or_else(|_| ".claude".into());
    std::path::Path::new(&dir)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(".claude")
        .to_string()
}

/// Seconds → "H:MM" (hours unbounded, minutes zero-padded). Clamps negatives to
/// 0:00 so a stale/just-reset window never shows a negative countdown.
fn hm(secs: i64) -> String {
    let s = secs.max(0);
    format!("{}:{:02}", s / 3600, (s % 3600) / 60)
}

/// Threshold color for a used-% (green <70, yellow <90, red ≥90); green when
/// absent (it won't be rendered anyway).
fn qcol(p: Option<i64>) -> &'static str {
    match p {
        Some(n) if n >= 90 => C_RED,
        Some(n) if n >= 70 => C_YELLOW,
        _ => C_GREEN,
    }
}

/// Accept ints, floats, or numeric strings; default 0. (The API has sent all
/// three over time; the jq `// 0` + `${x%%.*}` chain tolerated them.)
fn json_num(v: &serde_json::Value) -> f64 {
    if let Some(f) = v.as_f64() {
        f
    } else if let Some(s) = v.as_str() {
        s.trim().parse::<f64>().unwrap_or(0.0)
    } else {
        0.0
    }
}

pub(crate) fn now_epoch() -> i64 {
    if let Ok(s) = std::env::var("CLAUDE_STATUSLINE_NOW") {
        if let Ok(n) = s.trim().parse::<i64>() {
            return n;
        }
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn state_dir() -> std::path::PathBuf {
    let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
    let uid = unsafe { libc::getuid() };
    std::path::Path::new(&base).join(format!("claude-cache-timer-{uid}"))
}

/// Build the "  ·  ⏱ …" trailer. Returns "" when used>0 but no cache signal
/// exists yet, matching the bash `elif last>0` (no else) shape.
fn cache_segment(sid: &str, transcript: &str, used: i64, now: i64) -> String {
    let dir = state_dir();
    let _ = std::fs::create_dir_all(&dir);

    let mut last: i64 = 0;
    let sf = if sid.is_empty() {
        None
    } else {
        Some(dir.join(sid))
    };

    // 1. activity fingerprint = total_input_tokens, tracked in "<fp> <epoch>"
    if let Some(sf) = &sf {
        let fp = used.to_string();
        let (prev_fp, prev_ts) = read_pair(sf);
        if fp != prev_fp {
            atomic_write(sf, &format!("{fp} {now}"));
            last = now;
        } else {
            last = prev_ts;
        }
    }

    // 2. send-time reset from the transcript's last user/assistant entry,
    //    memoized by the transcript's mtime so the 128KB scan is skipped on
    //    idle renders (the smoothness fix).
    if used > 0 && !transcript.is_empty() {
        if let Ok(meta) = std::fs::metadata(transcript) {
            let tmtime = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let tcache = sf.as_ref().map(|p| with_suffix(p, ".tcache"));
            let cached = tcache.as_ref().map(|p| read_pair(p));
            let hit = matches!(&cached, Some((m, _)) if !m.is_empty() && m == &tmtime.to_string())
                && tmtime != 0;
            let msg_ts: i64 = if hit {
                cached.as_ref().unwrap().1 // reuse cached entry epoch
            } else {
                let ts = scan_transcript_epoch(transcript).unwrap_or(0);
                if let Some(tc) = &tcache {
                    atomic_write(tc, &format!("{tmtime} {ts}"));
                }
                ts
            };
            if msg_ts > last {
                last = msg_ts;
            }
        }
    }

    // 3. last-ditch: nothing else, fall back to transcript mtime as a bound.
    if last == 0 && !transcript.is_empty() {
        if let Ok(meta) = std::fs::metadata(transcript) {
            if let Ok(d) = meta.modified().and_then(|m| Ok(m.duration_since(UNIX_EPOCH).unwrap())) {
                last = d.as_secs() as i64;
            }
        }
    }

    // --- render the trailer (matches statusline.sh byte-for-byte) -----------
    if used == 0 {
        return format!("{C_DIM}  fresh{C_RST}");
    }
    if last > 0 {
        let left = last + TTL - now;
        if left > 0 {
            let ccol = if left < 60 { C_YELLOW } else { C_GREEN };
            return format!("  {ccol}{left}s{C_RST}");
        }
        return format!("{C_DIM}  cold{C_RST}");
    }
    String::new()
}

/// Read "first second" from a one-line file. Missing/short -> ("", 0).
fn read_pair(p: &std::path::Path) -> (String, i64) {
    let s = std::fs::read_to_string(p).unwrap_or_default();
    let mut it = s.split_whitespace();
    let a = it.next().unwrap_or("").to_string();
    let b = it.next().and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
    (a, b)
}

/// write-then-rename so a concurrent render never reads a half-written file.
pub(crate) fn atomic_write(p: &std::path::Path, body: &str) {
    let tmp = with_suffix(p, &format!(".{}", std::process::id()));
    if std::fs::write(&tmp, format!("{body}\n")).is_ok() {
        let _ = std::fs::rename(&tmp, p);
    }
}

pub(crate) fn with_suffix(p: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    std::path::PathBuf::from(s)
}

/// Read the last TAIL_BYTES of the transcript, find the last user/assistant
/// entry, parse its timestamp to an epoch. Mirrors the bash tail|grep pipeline.
fn scan_transcript_epoch(path: &str) -> Option<i64> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);

    let mut best: Option<i64> = None;
    for line in text.lines() {
        if !is_user_or_assistant(line) {
            continue;
        }
        if let Some(ts) = extract_timestamp(line).and_then(parse_iso_epoch) {
            best = Some(ts); // last matching line wins (== `tail -1`)
        }
    }
    best
}

/// Matches lines whose "type" is "user" or "assistant", tolerating both compact
/// and spaced JSON (`"type":"user"` and `"type": "user"`).
fn is_user_or_assistant(line: &str) -> bool {
    let squished: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    squished.contains("\"type\":\"user\"") || squished.contains("\"type\":\"assistant\"")
}

/// Pull the first `YYYY-MM-DDTHH:MM:SS` after a `"timestamp"` key.
fn extract_timestamp(line: &str) -> Option<&str> {
    let i = line.find("\"timestamp\"")?;
    let rest = &line[i..];
    // find the value's opening quote after the colon
    let colon = rest.find(':')?;
    let after = &rest[colon..];
    let q1 = after.find('"')?;
    let val = &after[q1 + 1..];
    if val.len() >= 19 && looks_like_iso(&val[..19]) {
        Some(&val[..19])
    } else {
        None
    }
}

fn looks_like_iso(s: &str) -> bool {
    let b = s.as_bytes();
    s.len() == 19
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b.iter().enumerate().all(|(i, &c)| {
            matches!(i, 4 | 7 | 10 | 13 | 16) || c.is_ascii_digit()
        })
}

/// "YYYY-MM-DDTHH:MM:SS" (UTC) -> epoch seconds. No chrono dependency:
/// Howard Hinnant's days_from_civil.
fn parse_iso_epoch(s: &str) -> Option<i64> {
    let y: i64 = s[0..4].parse().ok()?;
    let m: i64 = s[5..7].parse().ok()?;
    let d: i64 = s[8..10].parse().ok()?;
    let hh: i64 = s[11..13].parse().ok()?;
    let mm: i64 = s[14..16].parse().ok()?;
    let ss: i64 = s[17..19].parse().ok()?;
    let yy = y - (m <= 2) as i64;
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = yy - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    // --- hm: seconds -> "H:MM", negatives clamped --------------------------
    #[test]
    fn hm_formats_and_clamps() {
        assert_eq!(hm(0), "0:00");
        assert_eq!(hm(-5), "0:00"); // negative clamps, never shows "-0:00"
        assert_eq!(hm(59), "0:00"); // sub-minute floors
        assert_eq!(hm(60), "0:01");
        assert_eq!(hm(3600), "1:00");
        assert_eq!(hm(3661), "1:01");
        assert_eq!(hm(8 * 3600 + 34 * 60), "8:34");
        assert_eq!(hm(134 * 3600 + 34 * 60), "134:34"); // hours unbounded, mins padded
    }

    // --- qcol: threshold colors at the 70/90 boundaries --------------------
    #[test]
    fn qcol_thresholds() {
        assert_eq!(qcol(None), C_GREEN);
        assert_eq!(qcol(Some(0)), C_GREEN);
        assert_eq!(qcol(Some(69)), C_GREEN);
        assert_eq!(qcol(Some(70)), C_YELLOW); // 70 is yellow, not green
        assert_eq!(qcol(Some(89)), C_YELLOW);
        assert_eq!(qcol(Some(90)), C_RED); // 90 is red, not yellow
        assert_eq!(qcol(Some(100)), C_RED);
    }

    // --- json_num: tolerate int / float / numeric-string / junk -----------
    #[test]
    fn json_num_coerces() {
        use serde_json::json;
        assert_eq!(json_num(&json!(42)), 42.0);
        assert_eq!(json_num(&json!(42.7)), 42.7);
        assert_eq!(json_num(&json!("42")), 42.0);
        assert_eq!(json_num(&json!("  42.5  ")), 42.5); // trims whitespace
        assert_eq!(json_num(&json!(null)), 0.0); // missing -> 0
        assert_eq!(json_num(&json!("nope")), 0.0); // unparseable -> 0
        assert_eq!(json_num(&json!(true)), 0.0); // wrong type -> 0
    }

    // --- looks_like_iso: shape gate for the 19-char prefix ----------------
    #[test]
    fn looks_like_iso_shape() {
        assert!(looks_like_iso("2021-06-21T12:00:00"));
        assert!(!looks_like_iso("2021-06-21T12:00:0")); // too short
        assert!(!looks_like_iso("2021-06-21 12:00:00")); // space instead of 'T'
        assert!(!looks_like_iso("2021/06/21T12:00:00")); // wrong separators
        assert!(!looks_like_iso("20X1-06-21T12:00:00")); // non-digit in field
    }

    // --- parse_iso_epoch: known UTC anchors -------------------------------
    #[test]
    fn parse_iso_epoch_anchors() {
        assert_eq!(parse_iso_epoch("1970-01-01T00:00:00"), Some(0));
        assert_eq!(parse_iso_epoch("2001-09-09T01:46:40"), Some(1_000_000_000));
        assert_eq!(parse_iso_epoch("2009-02-13T23:31:30"), Some(1_234_567_890));
        assert_eq!(parse_iso_epoch("2000-01-01T00:00:00"), Some(946_684_800));
        assert_eq!(parse_iso_epoch("not-a-date--------"), None);
    }

    // --- extract_timestamp: compact + spaced JSON, value truncated to 19 --
    #[test]
    fn extract_timestamp_variants() {
        assert_eq!(
            extract_timestamp(r#"{"timestamp":"2021-06-21T12:00:00.123Z","type":"user"}"#),
            Some("2021-06-21T12:00:00")
        );
        assert_eq!(
            extract_timestamp(r#"{ "timestamp": "2021-06-21T12:00:00Z" }"#),
            Some("2021-06-21T12:00:00")
        );
        assert_eq!(extract_timestamp(r#"{"type":"user"}"#), None); // no timestamp key
        assert_eq!(extract_timestamp(r#"{"timestamp":"short"}"#), None); // value too short
    }

    // --- is_user_or_assistant: type filter, whitespace-insensitive --------
    #[test]
    fn is_user_or_assistant_filter() {
        assert!(is_user_or_assistant(r#"{"type":"user"}"#));
        assert!(is_user_or_assistant(r#"{"type":"assistant"}"#));
        assert!(is_user_or_assistant(r#"{ "type" : "user" }"#)); // spaced
        assert!(!is_user_or_assistant(r#"{"type":"system"}"#));
        assert!(!is_user_or_assistant(r#"{"role":"user"}"#)); // wrong key
    }

    // --- read_pair: "first second" parsing from a one-line file -----------
    #[test]
    fn read_pair_parses() {
        let f = unique_tmp("read_pair");
        std::fs::write(&f, "abc 123\n").unwrap();
        assert_eq!(read_pair(&f), ("abc".to_string(), 123));

        std::fs::write(&f, "").unwrap();
        assert_eq!(read_pair(&f), (String::new(), 0)); // empty file

        std::fs::write(&f, "onlyone\n").unwrap();
        assert_eq!(read_pair(&f), ("onlyone".to_string(), 0)); // missing second field

        let _ = std::fs::remove_file(&f);
        assert_eq!(read_pair(&f), (String::new(), 0)); // missing file
    }

    // --- with_suffix: byte-append to the path ------------------------------
    #[test]
    fn with_suffix_appends() {
        assert_eq!(with_suffix(Path::new("/tmp/x"), ".y"), PathBuf::from("/tmp/x.y"));
        assert_eq!(with_suffix(Path::new("/a/b.json"), ".123"), PathBuf::from("/a/b.json.123"));
    }

    // --- quota_segment: env + file backed; all suppression paths ----------
    // Mutates process env (TMPDIR / CLAUDE_CONFIG_DIR), so every assertion
    // lives in ONE test to avoid racing the parallel test harness.
    #[test]
    fn quota_segment_paths() {
        let tmp = unique_dir("quota");
        let orig_tmpdir = std::env::var_os("TMPDIR");
        let orig_ccd = std::env::var_os("CLAUDE_CONFIG_DIR");
        std::env::set_var("TMPDIR", &tmp);
        std::env::set_var("CLAUDE_CONFIG_DIR", ".claude-qtest");
        let dir = usage_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(".claude-qtest.json");
        let now = 1_000_000_000_i64;

        // no file -> suppressed
        let _ = std::fs::remove_file(&file);
        assert_eq!(quota_segment(now), "");

        // both windows live -> shows both pcts + both countdowns
        let live = format!(
            r#"{{"5h":{{"pct":42,"reset":{}}},"7d":{{"pct":18,"reset":{}}},"at":{}}}"#,
            now + 8 * 3600 + 34 * 60,   // 5h resets in 8:34
            now + 134 * 3600 + 34 * 60, // 7d resets in 134:34
            now
        );
        std::fs::write(&file, &live).unwrap();
        let out = quota_segment(now);
        assert!(out.starts_with("  "), "leads with two spaces: {out:?}");
        assert!(out.contains("42%"), "5h pct: {out:?}");
        assert!(out.contains("18%"), "7d pct: {out:?}");
        assert!(out.contains("8:34"), "5h countdown: {out:?}");
        assert!(out.contains("134:34"), "7d countdown: {out:?}");

        // dead-daemon guard: poll older than 600s -> suppressed
        let stale = format!(
            r#"{{"5h":{{"pct":42,"reset":{}}},"7d":{{"pct":18,"reset":{}}},"at":{}}}"#,
            now + 3600,
            now + 3600,
            now - 700
        );
        std::fs::write(&file, &stale).unwrap();
        assert_eq!(quota_segment(now), "");

        // both windows already reset -> suppressed
        let expired = format!(
            r#"{{"5h":{{"pct":42,"reset":{}}},"7d":{{"pct":18,"reset":{}}},"at":{}}}"#,
            now - 10,
            now - 10,
            now
        );
        std::fs::write(&file, &expired).unwrap();
        assert_eq!(quota_segment(now), "");

        // one window past reset -> that side renders as the em dash
        let one = format!(
            r#"{{"5h":{{"pct":42,"reset":{}}},"7d":{{"pct":18,"reset":{}}},"at":{}}}"#,
            now + 3600, // 5h live
            now - 10,   // 7d already reset
            now
        );
        std::fs::write(&file, &one).unwrap();
        let out = quota_segment(now);
        assert!(out.contains("42%"), "live side shown: {out:?}");
        assert!(out.contains("—"), "reset side is em dash: {out:?}");

        // Restore the mutated process env so no later temp_dir()/profile read
        // in this process sees the about-to-be-deleted dir.
        match orig_tmpdir {
            Some(v) => std::env::set_var("TMPDIR", v),
            None => std::env::remove_var("TMPDIR"),
        }
        match orig_ccd {
            Some(v) => std::env::set_var("CLAUDE_CONFIG_DIR", v),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // --- test helpers -----------------------------------------------------
    fn unique_dir(tag: &str) -> PathBuf {
        let ns = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        // Fixed /tmp base, NOT temp_dir(): temp_dir() re-reads $TMPDIR, which
        // quota_segment_paths mutates process-wide — a parallel test that read
        // it mid-window would build its scratch dir inside the quota dir and
        // lose it to that test's remove_dir_all.
        let d = PathBuf::from("/tmp").join(format!("slrs-{tag}-{}-{ns}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn unique_tmp(tag: &str) -> PathBuf {
        unique_dir(tag).join("f")
    }
}
