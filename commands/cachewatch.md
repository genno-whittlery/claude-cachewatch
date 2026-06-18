Run this command and report the output verbatim:

```bash
python3 ~/.claude/scripts/cachewatch.py $ARGUMENTS
```

Lists every Claude Code session active in the last 24h with its prompt-cache state: **warm** (last API activity < 5 min ago, seconds remaining shown) or **cold**, plus idle time, project, session ID prefix, and last prompt. Sessions sort warmest-first. The **WAIT** column flags sessions parked on the human — `waiting` (turn done) or `blocked` (on a permission prompt) — when the optional wait hooks are installed; otherwise it stays blank.

Flags: `--hours N` (narrower window), `--all` (no age filter), `--watch` (live 1-second refresh — suggest the user run that one themselves in a tmux/zellij pane via `! python3 ~/.claude/scripts/cachewatch.py --watch`, since it never exits).

Same mtime proxy as the statusline cache timer: "cold" is always real; "warm" can be slightly optimistic because harness records also touch the transcript.

Source: `~/.claude/scripts/cachewatch.py`. Don't reimplement inline — edit the script if behavior needs to change.
