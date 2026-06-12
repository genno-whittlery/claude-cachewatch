#!/usr/bin/env python3
"""cachewatch — which Claude Code sessions still have a warm prompt cache?

Scans ~/.claude/projects/*/<uuid>.jsonl (top-level sessions only; agent-*.jsonl
subagent transcripts are excluded), treats each file's mtime as the session's
last API activity, and reports warm (< 5 min) vs cold against Anthropic's
sliding 5-minute prompt-cache TTL.

Same proxy as the statusline timer, same caveat: harness records (mode toggles,
last-prompt) also touch mtime, so "warm" can be slightly optimistic; "cold" is
always real.

Usage:
  cachewatch.py            sessions active in the last 24h, one shot
  cachewatch.py --hours 6  narrower window
  cachewatch.py --all      every session ever
  cachewatch.py --watch    live view, refreshes every second (run in a pane)
"""

import argparse
import json
import sys
import time
from pathlib import Path

TTL = 300  # Anthropic prompt-cache TTL, seconds
PROJECTS = Path.home() / ".claude" / "projects"
TAIL_BYTES = 65536
HOME_ENC = str(Path.home()).replace("/", "-")  # project dirs encode cwd with "/" -> "-"

GREEN, YELLOW, DIM, BOLD, RST = "\033[32m", "\033[33m", "\033[2m", "\033[1m", "\033[0m"


def humanize(secs: float) -> str:
    secs = int(secs)
    if secs < 60:
        return f"{secs}s"
    if secs < 3600:
        return f"{secs // 60}m{secs % 60:02d}s"
    if secs < 86400:
        return f"{secs // 3600}h{(secs % 3600) // 60:02d}m"
    return f"{secs // 86400}d{(secs % 86400) // 3600}h"


def session_label(path: Path) -> str:
    """Best-effort label from the transcript tail: last user prompt, else slug."""
    try:
        with path.open("rb") as f:
            f.seek(max(0, path.stat().st_size - TAIL_BYTES))
            tail = f.read()
    except OSError:
        return ""
    prompt, slug = "", ""
    for line in tail.split(b"\n")[1:]:
        line = line.strip()
        if not line:
            continue
        try:
            d = json.loads(line)
        except (json.JSONDecodeError, UnicodeDecodeError):
            continue
        if d.get("type") == "last-prompt" and d.get("lastPrompt"):
            prompt = str(d["lastPrompt"])
        if d.get("slug"):
            slug = str(d["slug"])
    label = prompt or slug
    return " ".join(label.split())


def scan(max_age_s: float | None):
    now = time.time()
    rows = []
    seen = set()  # same session can appear under two project-dir encodings (/Users vs /home symlink)
    for jsonl in sorted(PROJECTS.glob("*/*.jsonl")):
        if jsonl.name.startswith("agent-") or jsonl.stem in seen:
            continue
        seen.add(jsonl.stem)
        try:
            mtime = jsonl.stat().st_mtime
        except OSError:
            continue
        idle = now - mtime
        if max_age_s is not None and idle > max_age_s:
            continue
        project = jsonl.parent.name.removeprefix(HOME_ENC).lstrip("-")
        rows.append((idle, project, jsonl.stem[:8], jsonl))
    rows.sort(key=lambda r: r[0])
    return rows


def render(rows, labels: dict) -> str:
    out = [f"{BOLD}{'CACHE':<12} {'IDLE':>8}  {'PROJECT':<28} {'SESSION':<9} LAST PROMPT{RST}"]
    warm = 0
    for idle, project, sid, path in rows:
        left = TTL - idle
        if left > 0:
            warm += 1
            col = YELLOW if left < 60 else GREEN
            state = f"{col}⏱ {int(left):>3}s warm{RST}"
        else:
            state = f"{DIM}cold{RST}        "
        label = labels.setdefault(path, session_label(path))[:60]
        out.append(f"{state} {humanize(idle):>8}  {project:<28.28} {sid:<9} {DIM}{label}{RST}")
    out.append(f"{DIM}{warm} warm / {len(rows)} shown · TTL {TTL}s · mtime proxy (warm may be optimistic){RST}")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--hours", type=float, default=24, help="only sessions active in the last N hours (default 24)")
    ap.add_argument("--all", action="store_true", help="no age filter")
    ap.add_argument("--watch", action="store_true", help="refresh every second")
    args = ap.parse_args()
    max_age = None if args.all else args.hours * 3600

    labels: dict = {}
    if not args.watch:
        print(render(scan(max_age), labels))
        return
    try:
        while True:
            frame = render(scan(max_age), labels)
            sys.stdout.write("\033[2J\033[H" + frame + "\n")
            sys.stdout.flush()
            time.sleep(1)
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
