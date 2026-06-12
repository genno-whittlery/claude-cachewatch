# claude-cachewatch

Prompt-cache visibility for Claude Code: a ticking cache-expiry timer in your status line, and a fleet view of which sessions are still warm.

Anthropic's prompt cache has a **5-minute sliding TTL**. Every API call that reuses your conversation prefix refreshes it; let it lapse and the next turn re-reads your entire conversation at full input price instead of the ~10× cheaper cached rate. On a 300k-token conversation that's a real difference, and Claude Code gives you no indication of where you stand. These two tools make the window visible.

## Pieces

- **`statusline.sh`** — a Claude Code status line: model, context-usage bar, and a live `⏱ 287s` countdown to cache expiry that ticks every second and resets whenever the session makes an API call. Shows `cache cold` once the window lapses.

  `Fable 5  ████░░░░░░░░░░ 31%  ·  310k/1.0M  ·  ⏱ 287s`

- **`cachewatch.py`** — scans every session on the machine and prints a warm/cold table, warmest first, so you know which conversation is cheap to reply to right now:

  ```
  CACHE            IDLE  PROJECT               SESSION   LAST PROMPT
  ⏱ 292s warm       7s  proj-puzzle-platform  e811ebbe  sure. do it
  ⏱  93s warm    3m26s  proj-kerra            3ca749dc  go
  cold            5m12s  proj-kerra            5f4d8a8f  ok
  ```

## Install

**Status line** (requires `jq`):

```bash
cp statusline.sh ~/.claude/statusline.sh && chmod +x ~/.claude/statusline.sh
```

Then in `~/.claude/settings.json`:

```json
{
  "statusLine": {
    "type": "command",
    "command": "~/.claude/statusline.sh",
    "refreshInterval": 1
  }
}
```

`refreshInterval` is the undersung half of this: by default the status line only re-renders on conversation events, so a countdown would freeze between messages. With it, the harness re-runs the script every second and the timer ticks while you think.

**Fleet view** (Python ≥ 3.10, no dependencies):

```bash
cp cachewatch.py ~/.claude/scripts/cachewatch.py
python3 ~/.claude/scripts/cachewatch.py            # active in last 24h, one shot
python3 ~/.claude/scripts/cachewatch.py --watch    # live dashboard, 1 s refresh — run it in a spare tmux/zellij pane
python3 ~/.claude/scripts/cachewatch.py --hours 6  # narrower window
python3 ~/.claude/scripts/cachewatch.py --all      # everything ever
```

Optionally wrap it as a slash command at `~/.claude/commands/cachewatch.md` so `/cachewatch` works inside a session.

## Token-saving tips

What to actually do with the timer:

1. **Reply while warm.** Cached input is roughly 10× cheaper than fresh input. A follow-up inside the window re-reads your whole conversation at the cached rate; a cold one pays full price *plus* a fresh cache write (1.25×).
2. **Batch your follow-ups.** Three quick questions inside one warm window cost a fraction of the same three questions spread across hours.
3. **It's a cliff, not a slope.** 4 minutes idle costs nothing extra; 6 minutes costs a full re-read — and 6 minutes costs exactly the same as 6 hours. If you've already missed the window, stop hurrying.
4. **Long single commands kill the cache silently.** One 20-minute build inside one Bash call means zero API calls for 20 minutes — the session looks busy, but the next turn is cold. Backgrounding long jobs keeps the agent making calls (and the cache warm) while the work runs.
5. **Subagents don't keep the parent warm.** Each subagent is its own conversation with its own cache. While the parent waits on a >5-minute agent run, the parent's own timer counts down honestly — expect one cold read when it resumes. Usually still worth it: the subagent did its heavy reading in its *own* context, which is the bigger saving.
6. **Pick a side of the cliff for polling loops.** Poll under ~270 s to stay warm, or commit to long sleeps (20–30 min) and pay one cold read per wake. Polling at ~300 s is the worst of both worlds.
7. **`/compact` starts a new prefix.** The next turn after compaction is a full cache write no matter how warm you were. Compact because you need the room, not reflexively.
8. **When juggling sessions, answer the warmest first.** That's why `cachewatch` sorts the way it does.

## How it works

There's no API for "when does my cache expire," so both tools reconstruct *last API activity* locally — and the obvious signal is wrong in an interesting way.

The obvious signal is the session transcript (`~/.claude/projects/<dir>/<uuid>.jsonl`), which the harness appends to as the conversation progresses. But those writes are **buffered**: mid-turn, the file's mtime can lag ~40 s behind actual API traffic, so an mtime-only timer reads stale *while the model is actively working*. Meanwhile the status line's JSON input carries `context_window.total_input_tokens` and `cost.total_api_duration_ms`, which the harness updates immediately on every API call.

So `statusline.sh` uses both, taking whichever is fresher:

- **Activity fingerprint** — `total_input_tokens:total_api_duration_ms` is compared between renders via a tiny state file (`$TMPDIR/claude-cache-timer/<session_id>`). Changed fingerprint = API call happening right now = timer resets to 300 live.
- **Transcript mtime** — the floor that covers fresh sessions before a state file exists.

`cachewatch.py` is read-only across all sessions, so it uses pure mtime, deduplicates sessions that appear under two project-dir encodings (e.g. a `/home → /Users` symlink), excludes `agent-*.jsonl` subagent transcripts, and pulls each session's label from a 64 KB tail-read of the JSONL — cheap even on multi-hundred-MB transcripts.

### Accuracy caveats

- The errors are asymmetric in a useful direction: harness bookkeeping records (mode toggles, prompt drafts) touch the transcript without an API call, so **"warm" can be slightly optimistic** — but a false warm costs you nothing you weren't already paying. **"cold" is always real.**
- In `cachewatch`, a session that's mid-turn can read more stale than it is (buffered writes). It also isn't waiting for your reply, so the misread is harmless.
- The subagent behavior from tip 5 is visible in the status line: the parent's timer keeps counting down during an agent run. That's not a bug — the parent's cache really is cooling.
- The 5-minute TTL is the standard Anthropic tier, hardcoded as `TTL = 300`. (The API also has a 1-hour tier; Claude Code doesn't use it.)

## Requirements

macOS or Linux. `jq` for the status line; Python ≥ 3.10 for `cachewatch.py`. Tested against Claude Code's statusline JSON as of June 2026 — field names may drift in future versions.

## License

MIT
