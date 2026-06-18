#!/usr/bin/env python3
"""session-wait-state — record whether a Claude Code session is waiting on the human.

Driven by three settings.json hooks, each passing its verb as argv[1]:

  Stop              -> "wait"   : turn finished, Claude is idle waiting for input
  Notification      -> "block"  : Claude is blocked on a permission prompt / idle nudge
  UserPromptSubmit  -> "clear"  : the human responded; the session is busy again

State lives beside the statusline's cache-timer files so `cachewatch` (which keys
rows by session UUID) can surface a WAIT column without extra plumbing:

  ${TMPDIR:-/tmp}/claude-cache-timer-<uid>/<session_id>.waiting   ->  "<state> <epoch>"

On a wait/block transition it also fires a Telegram push (same bot config the
statusline cache-cold alert uses, ~/.config/cache-notify/env with TG_TOKEN +
TG_CHAT) so a phone-away human learns a session needs them. The push is
fire-and-forget — curl is detached so the hook returns instantly. Set
CACHE_WAIT_NOTIFY=0 to silence the per-turn "waiting" pings (the "blocked"
permission pings still fire). Everything is inert until the config file exists.

The session_id arrives on stdin (the standard hook payload). Hooks must never
fail the turn, so every error path exits 0 silently.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

VERBS = {"wait": "wait", "block": "block", "clear": None}
NOTIFY_CFG = Path.home() / ".config" / "cache-notify" / "env"


def state_dir() -> Path:
    base = os.environ.get("TMPDIR", "/tmp").rstrip("/")
    return Path(base) / f"claude-cache-timer-{os.getuid()}"


def resolve_cwd(payload: dict) -> str:
    """The session's working dir. Prefer the hook payload's cwd; fall back to the
    last cwd recorded in the transcript (the Notification hook may omit cwd, and
    decoding it from the project-dir name is ambiguous when the path has hyphens)."""
    cwd = str(payload.get("cwd") or "").strip()
    if cwd:
        return cwd
    tp = str(payload.get("transcript_path") or "").strip()
    if not tp:
        return ""
    try:
        with open(tp, "rb") as f:
            f.seek(0, 2)
            size = f.tell()
            f.seek(max(0, size - 65536))
            tail = f.read().decode("utf-8", "replace")
    except OSError:
        return ""
    last = ""
    for line in tail.splitlines():
        i = line.find('"cwd":"')
        if i == -1:
            continue
        j = line.find('"', i + 7)
        if j != -1:
            last = line[i + 7:j]
    return last


def tab_label(cwd: str, sid: str) -> str:
    """The /tab sidecar label for this session, if one was written."""
    for base in (cwd, "."):
        if not base:
            continue
        try:
            line = (Path(base) / ".claude" / "sessions" / sid).read_text().splitlines()
            if line and line[0].strip():
                return line[0].strip()
        except OSError:
            continue
    return ""


def telegram_creds() -> tuple[str, str] | None:
    """Parse TG_TOKEN / TG_CHAT from the shell-style cache-notify env file."""
    try:
        text = NOTIFY_CFG.read_text()
    except OSError:
        return None
    vals: dict[str, str] = {}
    for raw in text.splitlines():
        line = raw.strip()
        if line.startswith("export "):
            line = line[len("export "):]
        if "=" not in line:
            continue
        k, v = line.split("=", 1)
        vals[k.strip()] = v.strip().strip("'\"")
    token, chat = vals.get("TG_TOKEN"), vals.get("TG_CHAT")
    return (token, chat) if token and chat else None


def notify(state: str, sid: str, payload: dict) -> None:
    """Fire a detached Telegram push; silently no-op on any problem."""
    if state == "wait" and os.environ.get("CACHE_WAIT_NOTIFY", "1") == "0":
        return
    creds = telegram_creds()
    if creds is None:
        return
    token, chat = creds
    tab = tab_label(resolve_cwd(payload), sid)
    prefix = f"[{tab}] " if tab else ""
    if state == "block":
        detail = str(payload.get("message") or "").strip() or "blocked on a prompt"
        msg = f"{prefix}Claude needs you — {detail}"
    else:
        msg = f"{prefix}Claude finished — waiting for your input"
    try:
        subprocess.Popen(
            ["curl", "-s", "--max-time", "5",
             f"https://api.telegram.org/bot{token}/sendMessage",
             "--data-urlencode", f"chat_id={chat}",
             "--data-urlencode", f"text={msg}"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
    except OSError:
        pass


def main() -> int:
    if len(sys.argv) < 2 or sys.argv[1] not in VERBS:
        return 0
    verb = sys.argv[1]
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, ValueError):
        payload = {}
    sid = str(payload.get("session_id") or "").strip()
    if not sid or "/" in sid or sid in (".", ".."):
        return 0

    d = state_dir()
    try:
        d.mkdir(parents=True, exist_ok=True)
    except OSError:
        return 0
    sf = d / f"{sid}.waiting"

    state = VERBS[verb]
    if state is None:  # clear
        try:
            sf.unlink()
        except FileNotFoundError:
            pass
        except OSError:
            pass
        return 0

    # write-then-rename so a concurrent cachewatch read never sees a partial file
    tmp = d / f"{sid}.waiting.{os.getpid()}"
    try:
        tmp.write_text(f"{state} {int(time.time())}\n")
        os.replace(tmp, sf)
        # prune sentinels older than a day, mirroring the statusline's cleanup
        cutoff = time.time() - 86400
        for f in d.glob("*.waiting"):
            try:
                if f.stat().st_mtime < cutoff:
                    f.unlink()
            except OSError:
                pass
    except OSError:
        try:
            tmp.unlink()
        except OSError:
            pass
        return 0

    notify(state, sid, payload)
    return 0


if __name__ == "__main__":
    sys.exit(main())
