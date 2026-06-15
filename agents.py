#!/usr/bin/env python3
"""agents — live one-line-per-session dashboard of every Claude Code agent.

Scans ~/.claude/projects/*/<uuid>.jsonl (top-level sessions; agent-*.jsonl
subagent transcripts are counted, not listed). For each session it shows:

  STATE   derived from the transcript tail + mtime:
            ● active   — something is writing the transcript right now (<8s)
            ◧ running  — last record dispatched a tool, awaiting its result
            ◌ waiting  — turn ended, awaiting your input
            · idle     — quiet for a while
  IDLE    age of the last transcript write (mtime proxy)
  CACHE   warm (<5min, Anthropic prompt-cache TTL) / cold
  SUB     live subagents (subagents/agent-*.jsonl written in the last 30s)
  PROJECT cwd-encoded project dir
  TAB     /tab label from <cwd>/.claude/sessions/<uuid>, else session slug
  DOING   last tool call when running, else last prompt / assistant snippet

Usage:
  agents.py            sessions active in the last 24h, one shot
  agents.py --hours 6  narrower window
  agents.py --all      every session ever
  agents.py --watch    live view, refreshes every second (run in a pane)
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path


def term_width(default: int = 80) -> int:
    """Real terminal width from the tty, ignoring a possibly-stale COLUMNS env
    (which shutil.get_terminal_size would prefer and get wrong under zellij)."""
    for stream in (sys.stdout, sys.stderr, sys.stdin):
        try:
            return os.get_terminal_size(stream.fileno()).columns
        except (OSError, ValueError, AttributeError):
            continue
    try:
        return int(os.environ.get("COLUMNS", "")) or default
    except ValueError:
        return default

TTL = 300          # Anthropic prompt-cache TTL, seconds
ACTIVE_S = 8       # transcript written this recently => actively working
SUB_LIVE_S = 30    # subagent transcript written this recently => still running
PROJECTS = Path.home() / ".claude" / "projects"
TAIL_BYTES = 65536
HOME_ENC = str(Path.home()).replace("/", "-")
# statusline.sh writes "<fingerprint> <epoch>" here when a session's last API
# call (fingerprint) changes; <epoch> is the true cache-warm start and the same
# source the statusline ⏱ counts down from. Transcript mtime is a worse proxy —
# harness writes (mode toggles, last-prompt) keep bumping it, so a CD derived
# from mtime never reliably counts down.
CACHE_DIR = Path(os.environ.get("TMPDIR") or "/tmp") / f"claude-cache-timer-{os.getuid()}"

GREEN, YELLOW, CYAN, DIM, BOLD, RST = (
    "\033[32m", "\033[33m", "\033[36m", "\033[2m", "\033[1m", "\033[0m")


def humanize(secs: float) -> str:
    secs = int(secs)
    if secs < 60:
        return f"{secs}s"
    if secs < 3600:
        return f"{secs // 60}m{secs % 60:02d}s"
    if secs < 86400:
        return f"{secs // 3600}h{(secs % 3600) // 60:02d}m"
    return f"{secs // 86400}d{(secs % 86400) // 3600}h"


def clean(s: str, n: int) -> str:
    s = " ".join(str(s).split())
    s = "".join(c for c in s if c.isprintable())  # untrusted text -> terminal
    return s[:n]


def parse_tail(path: Path) -> dict:
    """Pull last-prompt, slug, cwd, last-record type, and last tool/text from
    the transcript tail. Best-effort; partial data on any parse hiccup."""
    info = {"prompt": "", "slug": "", "cwd": "", "last_type": "",
            "last_tool": "", "last_text": "", "has_pending_tool": False}
    try:
        size = path.stat().st_size
        with path.open("rb") as f:
            f.seek(max(0, size - TAIL_BYTES))
            tail = f.read()
    except OSError:
        return info
    lines = tail.split(b"\n")
    if size > TAIL_BYTES:
        lines = lines[1:]  # first line is a mid-record fragment after seeking
    for raw in lines:
        raw = raw.strip()
        if not raw:
            continue
        try:
            d = json.loads(raw)
        except (json.JSONDecodeError, UnicodeDecodeError):
            continue
        t = d.get("type")
        if t == "last-prompt" and d.get("lastPrompt"):
            info["prompt"] = str(d["lastPrompt"])
        if d.get("slug"):
            info["slug"] = str(d["slug"])
        if d.get("cwd"):
            info["cwd"] = str(d["cwd"])
        if t in ("user", "assistant"):
            info["last_type"] = t
            msg = d.get("message") or {}
            content = msg.get("content")
            pending = False
            if isinstance(content, list):
                for blk in content:
                    if not isinstance(blk, dict):
                        continue
                    bt = blk.get("type")
                    if bt == "tool_use":
                        info["last_tool"] = str(blk.get("name") or "")
                        inp = blk.get("input") or {}
                        arg = (inp.get("command") or inp.get("file_path")
                               or inp.get("pattern") or inp.get("description")
                               or inp.get("prompt") or "")
                        if arg:
                            info["last_tool"] += ": " + str(arg)
                        pending = True
                    elif bt == "text" and blk.get("text"):
                        info["last_text"] = str(blk["text"])
                    elif bt == "tool_result":
                        pending = False  # a result arrived; tool no longer pending
            elif isinstance(content, str) and t == "assistant":
                info["last_text"] = content
            info["has_pending_tool"] = pending
    return info


def count_live_subagents(stem: str, now: float) -> int:
    n = 0
    for sub in PROJECTS.glob(f"*/{stem}/subagents/agent-*.jsonl"):
        try:
            if now - sub.stat().st_mtime <= SUB_LIVE_S:
                n += 1
        except OSError:
            continue
    return n


def cache_epoch(stem: str) -> float:
    """Epoch of the session's last API call, per statusline.sh's state file.
    0 if unknown (then render() falls back to the mtime-based idle)."""
    try:
        _fp, ts = (CACHE_DIR / stem).read_text().split()
        return float(ts)
    except (OSError, ValueError):
        return 0.0


def tab_label(cwd: str, stem: str) -> str:
    if not cwd:
        return ""
    sc = Path(cwd) / ".claude" / "sessions" / stem
    try:
        return sc.read_text(encoding="utf-8").strip()
    except OSError:
        return ""


def state_of(idle: float, info: dict) -> tuple[str, str]:
    """(glyph, color) from recency + last record shape. ASCII width-1 glyphs
    (avoid ambiguous-width unicode that doubles in CJK-configured terminals).
    * active   > running a tool / about to reply   o waiting on you   . idle"""
    if idle < ACTIVE_S:
        return "*", GREEN
    if info["has_pending_tool"] or info["last_type"] == "user":
        return ">", CYAN
    if info["last_type"] == "assistant":
        return "o", YELLOW
    return ".", DIM


def scan(max_age_s: float | None) -> list:
    now = time.time()
    best = {}  # one session can appear under /Users and /home encodings; keep freshest
    for jsonl in PROJECTS.glob("*/*.jsonl"):
        if jsonl.name.startswith("agent-"):
            continue
        try:
            mtime = jsonl.stat().st_mtime
        except OSError:
            continue
        prev = best.get(jsonl.stem)
        if prev is None or mtime > prev[0]:
            best[jsonl.stem] = (mtime, jsonl)
    rows = []
    for stem, (mtime, jsonl) in best.items():
        idle = now - mtime
        if max_age_s is not None and idle > max_age_s:
            continue
        info = parse_tail(jsonl)
        project = jsonl.parent.name.removeprefix(HOME_ENC).lstrip("-")
        rows.append({
            "idle": idle, "mtime": mtime, "stem": stem, "project": project,
            "info": info, "subs": count_live_subagents(stem, now),
            "tab": tab_label(info["cwd"], stem),
            "cache_epoch": cache_epoch(stem),
        })
    rows.sort(key=lambda r: r["idle"])
    return rows


def render(rows: list, width: int | None = None) -> str:
    width = width or term_width()
    now = time.time()
    doing_w = max(8, width - 26)  # 25 cols of fixed prefix (incl " | ") + 1 spare
    hdr = f"{'S':<1} {'CD':>3} {'#':>1} {'NAME':<14} | DOING"[:width - 1]
    out = [f"{BOLD}{hdr}{RST}"]
    active = 0
    for r in rows:
        idle, info = r["idle"], r["info"]
        sym, col = state_of(idle, info)
        if col in (GREEN, CYAN):
            active += 1
        # prefer the statusline's true warm-start epoch; fall back to mtime
        left = int(r["cache_epoch"] + TTL - now if r["cache_epoch"]
                   else TTL - idle)
        if left <= 0:
            cd = f"{DIM}  c{RST}"
        else:
            ccol = GREEN if left > 60 else YELLOW
            cd = f"{ccol}{left:>3}{RST}"
        subs = f"{CYAN}{r['subs']}{RST}" if r["subs"] else f"{DIM}.{RST}"
        name = r["tab"] or info["slug"] or r["stem"][:8]
        doing = (info["last_tool"] if info["has_pending_tool"]
                 else (info["prompt"] or info["last_text"]))
        out.append(
            f"{col}{sym}{RST} {cd} {subs} "
            f"{clean(name, 14):<14} {DIM}|{RST} "
            f"{DIM}{clean(doing, doing_w)}{RST}")
    foot = (f"{active} active - {len(rows)} shown - "
            f"*active >running owaiting .idle - CD=cache secs/c=cold")[:width - 1]
    out.append(f"{DIM}{foot}{RST}")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--hours", type=float, default=24,
                    help="only sessions active in the last N hours (default 24)")
    ap.add_argument("--all", action="store_true", help="no age filter")
    ap.add_argument("--watch", action="store_true", help="refresh every second")
    ap.add_argument("--width", type=int, default=None,
                    help="force output width (use when piped/fenced, no real tty)")
    args = ap.parse_args()
    max_age = None if args.all else args.hours * 3600

    if not args.watch:
        print(render(scan(max_age), args.width))
        return
    try:
        while True:
            frame = render(scan(max_age), args.width)
            sys.stdout.write("\033[2J\033[H" + frame + "\n")
            sys.stdout.flush()
            time.sleep(1)
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
