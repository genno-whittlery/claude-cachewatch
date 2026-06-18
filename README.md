# claude-cachewatch

Prompt-cache visibility for Claude Code: a ticking cache-expiry timer in your status line, and a fleet view of which sessions are still warm.

Anthropic's prompt cache has a **5-minute sliding TTL**. Every API call that reuses your conversation prefix refreshes it; let it lapse and the next turn re-reads your entire conversation at full input price instead of the ~10× cheaper cached rate. On a 300k-token conversation that's a real difference, and Claude Code gives you no indication of where you stand. These two tools make the window visible.

## Pieces

- **`statusline.sh`** — a Claude Code status line: model, context-usage bar, and a live `⏱ 287s` countdown to cache expiry that ticks every second and resets whenever the session makes an API call. Shows `⏱ cache cold` once the window lapses, and `⏱ fresh` when no cached prefix exists yet (new session, or right after `/compact`) — a moment when a countdown would be fiction.

  `Fable 5  ████░░░░░░░░░░ 31%  ·  310k/1.0M  ·  ⏱ 287s`

- **`cachewatch.py`** — scans every session on the machine and prints a warm/cold table, warmest first, so you know which conversation is cheap to reply to right now. The **WAIT** column flags sessions parked on you — `waiting` (the turn finished, it's your move) or `blocked` (stuck on a permission prompt) — so you can spot what needs attention without opening each one:

  ```
  CACHE            IDLE  WAIT     PROJECT               SESSION   LAST PROMPT
  ⏱ 292s warm       7s  waiting  proj-puzzle-platform  e811ebbe  sure. do it
  ⏱  93s warm    3m26s  blocked  proj-kerra            3ca749dc  go
  cold            5m12s           proj-kerra            5f4d8a8f  ok
  2 warm / 2 awaiting you / 3 shown · TTL 300s · mtime proxy (warm may be optimistic)
  ```

  The WAIT column is opt-in: it needs the three hooks below. Without them the column simply stays blank.

- **`session-wait-state.py`** — a tiny hook target that powers the WAIT column. Three Claude Code hooks call it (`Stop` → waiting, `Notification` → blocked, `UserPromptSubmit` → clear); it drops a per-session sentinel next to the cache-timer state. Optionally it also pushes a **Telegram** message when a session starts waiting on you — handy when you're away from the desk.

- **`agents.py`** — a denser one-line-per-session dashboard that builds on the same scan. Beyond warm/cold it shows a per-session **state glyph** (`*` active / `>` running a tool / `o` waiting on you / `.` idle), a live **CD** countdown read from the same state file the status line uses, a count of that session's **live subagents**, and the session's name (from a `/tab` label if you have one, else the slug). Width-aware so it fits a narrow pane:

  ```
  S  CD # NAME           | DOING
  * 298 . warm-meander   | Bash: python3 .claude/scripts/agents.py
  > 288 1 modular-sing   | Agent: draft the implementation plan
  o   c . signy          | how to exit
  2 active - 15 shown - *active >running owaiting .idle - CD=cache secs/c=cold
  ```

## Install

The easiest installer is Claude Code itself — paste this into a session:

> Install claude-cachewatch from https://github.com/genno-whittlery/claude-cachewatch — follow the README's install steps and agent notes. If I already have a custom status line, ask before replacing it.

### Notes for the installing agent

- `statusline.sh` → `~/.claude/statusline.sh`, `chmod +x`. If the user already has a `statusLine` command configured in `~/.claude/settings.json`, **ask before replacing it** — they may want the cache segment grafted into their existing script instead — that's the block between the `prompt-cache TTL` comment and the final `printf`, plus its inputs: `sid`, `used`, `apims`, `transcript`, and `cwd` from the statusline JSON, and the `dim`/`rst` color variables. (`cwd` is only used by the optional expiry-notification sub-block to find a `/tab` session label.)
- The settings change is a **merge, not an overwrite**: add/update only the `statusLine` key (including `"refreshInterval": 1`) and leave every other key in `settings.json` untouched.
- `cachewatch.py` → `~/.claude/scripts/cachewatch.py`; `commands/cachewatch.md` → `~/.claude/commands/cachewatch.md`. Optionally `agents.py` → `~/.claude/scripts/agents.py` and `commands/agents.md` → `~/.claude/commands/agents.md` for the denser dashboard.
- The WAIT column is optional. If the user wants it, also copy `session-wait-state.py` → `~/.claude/scripts/`, `chmod +x`, and merge the `Stop`/`Notification`/`UserPromptSubmit` `hooks` block (see Manual install) using **absolute** command paths. Tell them newly-added hooks load at next session start (or after opening `/hooks` once). Leave Telegram alone unless they ask — it's inert without `~/.config/cache-notify/env`.
- **Never commit the user's notify token.** The optional push features read `~/.config/cache-notify/env`; that file holds the Telegram bot token and chat id and must stay out of any repo (it lives under `~/.config`, not here). `statusline.sh` and `session-wait-state.py` only reference the variables, never the values.
- Verify before reporting success: pipe a fake payload through the statusline (`printf '{"session_id":"t","model":{"display_name":"X"},"context_window":{"used_percentage":10,"context_window_size":1000000,"total_input_tokens":100000},"cost":{"total_api_duration_ms":1}}' | bash ~/.claude/statusline.sh` should print a bar and `⏱ 300s` — on the first run; reruns with the same payload show a lower number because the state file persists) and run `python3 ~/.claude/scripts/cachewatch.py --hours 1` (should list the current session as warm). The live status line picks up `refreshInterval` at next session start if it doesn't tick immediately.

### Manual install

**Status line** (requires `jq`):

```bash
cp statusline.sh ~/.claude/statusline.sh && chmod +x ~/.claude/statusline.sh
```

Then merge this key into `~/.claude/settings.json` (keep your existing settings — don't replace the whole file):

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

If you run multiple `CLAUDE_CONFIG_DIR` profiles (separate logins), merge the `statusLine` block into **each profile's** `settings.json` — status-line config isn't shared between profiles. The script itself needs installing only once: the `command` path above is absolute, so every profile can point at the same file, and the timer's per-session state files don't care which profile a session belongs to.

**Fleet view** (Python ≥ 3.9, no dependencies):

```bash
mkdir -p ~/.claude/scripts && cp cachewatch.py ~/.claude/scripts/cachewatch.py
python3 ~/.claude/scripts/cachewatch.py            # active in last 24h, one shot
python3 ~/.claude/scripts/cachewatch.py --watch    # live dashboard, 1 s refresh — run it in a spare tmux/zellij pane
python3 ~/.claude/scripts/cachewatch.py --hours 6  # narrower window
python3 ~/.claude/scripts/cachewatch.py --all      # everything ever
```

For the denser dashboard, install `agents.py` the same way:

```bash
cp agents.py ~/.claude/scripts/agents.py
python3 ~/.claude/scripts/agents.py            # active in last 24h, one shot
python3 ~/.claude/scripts/agents.py --watch    # live, 1 s refresh, in a spare pane
python3 ~/.claude/scripts/agents.py --width 60 # force width when piped (no tty)
```

Optionally install the slash-command wrappers so `/cachewatch` and `/agents` work inside a session:

```bash
mkdir -p ~/.claude/commands
cp commands/cachewatch.md ~/.claude/commands/cachewatch.md
cp commands/agents.md ~/.claude/commands/agents.md
```

**WAIT column** (optional — flags sessions parked on you). Install the hook target, then wire three hooks:

```bash
cp session-wait-state.py ~/.claude/scripts/session-wait-state.py && chmod +x ~/.claude/scripts/session-wait-state.py
```

Merge this `hooks` block into `~/.claude/settings.json` (keep your existing settings — don't replace the whole file). Use the absolute path; `~` isn't expanded everywhere hooks run:

```json
{
  "hooks": {
    "Stop": [
      { "hooks": [ { "type": "command", "command": "python3 /ABSOLUTE/HOME/.claude/scripts/session-wait-state.py wait" } ] }
    ],
    "Notification": [
      { "hooks": [ { "type": "command", "command": "python3 /ABSOLUTE/HOME/.claude/scripts/session-wait-state.py block" } ] }
    ],
    "UserPromptSubmit": [
      { "hooks": [ { "type": "command", "command": "python3 /ABSOLUTE/HOME/.claude/scripts/session-wait-state.py clear" } ] }
    ]
  }
}
```

`Stop` fires when Claude finishes a turn (your move → `waiting`), `Notification` when it's stuck on a permission prompt or idle nudge (`blocked`), and `UserPromptSubmit` clears the flag when you reply. Newly-added hooks load at the next session start, or immediately if you open the `/hooks` menu once. If you set up the Telegram config below, `session-wait-state.py` also pushes a message when a session starts waiting — `[tab] Claude finished — waiting for your input`, or `Claude needs you — <prompt>` when blocked. Set `CACHE_WAIT_NOTIFY=0` to silence the per-turn "waiting" pings while keeping the "blocked" permission pings.

### Expiry notifications (optional)

`statusline.sh` can ping you when a session's cache first drops below a threshold, so you can keep a long-running session warm without watching the bar (and `session-wait-state.py` reuses the same config for the wait/blocked pings above). It's **inert until you create the config** — no file, no pings.

```bash
mkdir -p ~/.config/cache-notify && chmod 700 ~/.config/cache-notify
cat > ~/.config/cache-notify/env <<'EOF'
export TG_TOKEN="123456:your-telegram-bot-token"
export TG_CHAT="your-chat-id"
EOF
chmod 600 ~/.config/cache-notify/env
```

This file holds a secret — keep it under `~/.config`, never in a repo. Get a bot token from [@BotFather](https://t.me/BotFather), then message your bot once and read your chat id from `https://api.telegram.org/bot<TOKEN>/getUpdates`. The threshold defaults to 180 s; override per session with `CACHE_NOTIFY_BELOW=240` (or `0` to disable). On macOS a local banner fires too. Each cache cycle notifies once, re-armed when a new turn resets the timer.

## Token-saving tips

What to actually do with the timer:

1. **Reply while warm.** Cached input is 10× cheaper than fresh input (0.1× vs 1×). A follow-up inside the window re-reads your whole conversation at the cached rate; a cold one re-processes everything at the 1.25× cache-write rate.
2. **Batch your follow-ups.** Three quick questions inside one warm window cost a fraction of the same three questions spread across hours.
3. **It's a cliff, not a slope.** 4 minutes idle costs nothing extra; 6 minutes costs a full re-read — and 6 minutes costs exactly the same as 6 hours. If you've already missed the window, stop hurrying.
4. **Long single commands kill the cache silently.** One 20-minute build inside one Bash call means zero API calls for 20 minutes — the session looks busy, but the next turn is cold. Backgrounding long jobs keeps the agent making calls (and the cache warm) while the work runs.
5. **Subagents don't keep the parent warm.** Each subagent is its own conversation with its own cache. While the parent waits on a >5-minute agent run, the parent's own timer counts down honestly — expect one cold read when it resumes. Usually still worth it: the subagent did its heavy reading in its *own* context, which is the bigger saving.
6. **…unless the parent checks in every ~3 minutes.** Run long agents in the background and have the parent schedule a wakeup or status check at ~180 s intervals until they finish. Each check-in is a tiny *cached* API call that resets the parent's window, so the resume is warm instead of a full re-read. 180 s leaves comfortable margin inside the 5-minute TTL for scheduling jitter.
7. **Pick a side of the cliff for polling loops.** Poll under ~270 s to stay warm, or commit to long sleeps (20–30 min) and pay one cold read per wake. Polling at ~300 s is the worst of both worlds.
8. **`/compact` starts a new prefix.** The next turn after compaction is a full cache write no matter how warm you were — but compaction has its own cache economics; see below.
9. **When juggling sessions, answer the warmest first.** That's why `cachewatch` sorts the way it does.
10. **Simplify your memory files.** `CLAUDE.md` and auto-memory indexes load into the prompt prefix of *every* session — each line there is re-tokenized on every cache write and every cold read, in every conversation, forever. They're the most expensive prose you own. Keep index entries to one line with a pointer, move detail into topic files the agent reads on demand, and prune entries that stopped earning their keep.

### Compaction tips

`/compact` interacts with the cache more than you'd expect:

- **Compact while the timer is green.** The summarization pass is itself an API call carrying your whole conversation — fired inside the 5-minute window it reads your context at the cached rate; fired at a long-cold session it's a full-price read of everything. The end of a work arc, timer still ticking: that's the cheap moment.
- **…but finish your follow-ups first.** Once compacted, the model works from a summary instead of the verbatim history — anything you ask afterwards is answered from compressed memory. Cheap warm turns *before* the compact get the full-fidelity context; spend them, then compact.
- **Compact at task boundaries, and beat auto-compact to it.** A manual compact between tasks summarizes a *finished* arc, when nothing in flight is load-bearing. Auto-compact fires on a threshold, which by definition is mid-something.
- **Sometimes the right compact is a new session.** If the next piece of work is a different topic, a fresh session starts from a tiny prefix and the old session stays intact for reference — `cachewatch` keeps both on the board.
- **Compaction is an investment, not a discount.** You pay one summarize pass now so that every later turn re-reads a smaller prefix. It pays off if the session keeps going; it's wasted on a session you're about to abandon.

(Right after a compact, the harness reports a null context window — the status line shows this as `0%` and `⏱ fresh`; the bar and a real countdown rebuild on the next turn. **Resuming** carries the same rule: resume a normal session and the bar shows its real context immediately, but resume one whose latest state is a `/compact` and you'll see `0%` until your first new turn. Both are expected — the `0` is the post-compact null window, not a broken status line.)

## The numbers (verified against Anthropic's docs)

Fact-checked June 2026 against the official [prompt caching documentation](https://platform.claude.com/docs/en/build-with-claude/prompt-caching):

- **The TTL is a sliding window, and refreshing it is free.** Quoting the docs: *"The cache is refreshed for no additional cost each time the cached content is used."* Every API call that reads the cache pushes expiry another 5 minutes out — which is why an actively-working session never goes cold, and why this tool only measures *gaps*.
- **Reads cost 0.1× the base input-token price; writes cost 1.25×** (5-minute tier) or **2×** (1-hour tier). Reads are priced the same on both tiers.
- **Break-even is immediate.** On the 5-minute tier, caching pays for itself on the second request (1.25× + 0.1× = 1.35× vs 2× uncached). The 1-hour tier needs at least three requests (2× + 0.2× vs 3×).
- **A 1-hour tier exists** (`cache_control: {type: "ephemeral", ttl: "1h"}`) — built for exactly the long-gap pattern these tools make visible — but Claude Code uses the 5-minute tier, hence `TTL = 300` here.
- **Minimum cacheable prefix is model-dependent** (roughly 512–4096 tokens; below it, prompts silently don't cache). Irrelevant for Claude Code sessions, which exceed it within the first turn.
- **A cache entry becomes readable only after the first response begins.** N parallel requests with an identical prefix all pay full price — none can read what the others are still writing.

## How it works

There's no API for "when does my cache expire," so both tools reconstruct *last API activity* locally — and the obvious signal is wrong in an interesting way.

The obvious signal is the session transcript (`~/.claude/projects/<dir>/<uuid>.jsonl`), which the harness appends to as the conversation progresses. But those writes are **buffered**: mid-turn, the file's mtime can lag ~40 s behind actual API traffic, so an mtime-only timer reads stale *while the model is actively working*. Meanwhile the status line's JSON input carries `context_window.total_input_tokens` and `cost.total_api_duration_ms`, which the harness updates immediately on every API call.

So `statusline.sh` uses both, taking whichever is fresher:

- **Activity fingerprint** — `total_input_tokens:total_api_duration_ms` is compared between renders via a tiny per-user state file (`$TMPDIR/claude-cache-timer-<uid>/<session_id>`, pruned after a day). Changed fingerprint = API call happening right now = timer resets to 300 live.
- **Transcript mtime** — the floor that covers fresh sessions before a state file exists.

`cachewatch.py` is read-only across all sessions, so it uses pure mtime, deduplicates sessions that appear under two project-dir encodings (e.g. a `/home → /Users` symlink), excludes `agent-*.jsonl` subagent transcripts, and pulls each session's label from a 64 KB tail-read of the JSONL — cheap even on multi-hundred-MB transcripts.

The **WAIT column** is a different kind of signal — not "is the cache warm" but "is this session waiting on *me*." Cache warmth (mtime) can't answer that: a session is warm both while Claude is mid-thought and while it sits idle awaiting your reply. So the wait state comes from explicit edge events instead. The `Stop` hook fires exactly when a turn ends (→ `waiting`), `Notification` when Claude blocks on a permission prompt (→ `blocked`), and `UserPromptSubmit` when you reply (→ cleared); `session-wait-state.py` records the latest as a `<sid>.waiting` sentinel beside the cache-timer state, which `cachewatch` reads by session UUID. One guard handles the gap that permission-grants leave (they fire no clear hook): a sentinel is ignored once the transcript is written *after* it, so a `blocked` flag self-clears the moment work resumes.

### Accuracy caveats

- The errors are asymmetric in a useful direction: harness bookkeeping records (mode toggles, prompt drafts) touch the transcript without an API call, so **"warm" can be slightly optimistic** — but a false warm costs you nothing you weren't already paying. **"cold" is always real.**
- In `cachewatch`, a session that's mid-turn can read more stale than it is (buffered writes). It also isn't waiting for your reply, so the misread is harmless.
- The subagent behavior from tip 5 is visible in the status line: the parent's timer keeps counting down during an agent run. That's not a bug — the parent's cache really is cooling.
- **`/compact` blinds the timer while it runs, harmlessly.** During the summarize pass the countdown goes stale — the counters it fingerprints only update on call completion, and there is no observable signal at compact *start* — but the cached read the timer was there to confirm already happened the moment you fired the compact. Once the compact lands, the display switches to `⏱ fresh` (no cached prefix exists for the new, post-summary conversation) rather than a hollow green countdown, and a real countdown resumes on your first new-prefix turn.
- **Switching accounts mid-session is invisible to the timer.** Caches are scoped to the organization that made the request, so after a `/login` switch the green countdown shows real warmth in the *wrong wallet* — an entry the new account can't read. The next turn is a full-conversation cache write regardless. The statusline JSON carries no account identity, so this one can't be detected, only known: if you must switch accounts on a long session, compact first (warm, on the old account) so the new account's unavoidable write covers only the small summary prefix. Changing **model** mid-session misses the cache the same way (entries are model-specific).
- The 5-minute TTL is the standard Anthropic tier, hardcoded as `TTL = 300`. (The API also has a 1-hour tier; Claude Code doesn't use it.)

## Requirements

macOS or Linux. `jq` for the status line; Python ≥ 3.9 for `cachewatch.py` and `session-wait-state.py`; `curl` only for the optional Telegram push. Tested against Claude Code's statusline JSON as of June 2026 — field names may drift in future versions.

## License

MIT
