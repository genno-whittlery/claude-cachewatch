# claude-cachewatch

Prompt-cache visibility for Claude Code: a ticking cache-expiry timer in your status line, and a fleet view of which sessions are still warm.

Anthropic's prompt cache has a **5-minute sliding TTL**. Every API call that reuses your conversation prefix refreshes it; let it lapse and the next turn re-reads your entire conversation at full input price instead of the ~10× cheaper cached rate. On a 300k-token conversation that's a real difference, and Claude Code gives you no indication of where you stand. These two tools make the window visible.

## Pieces

- **`statusline.sh`** — a Claude Code status line: model, context-usage bar, and a live `⏱ 287s` countdown to cache expiry that ticks every second and resets whenever the session makes an API call. Shows `⏱ cache cold` once the window lapses, and `⏱ fresh` when no cached prefix exists yet (new session, or right after `/compact`) — a moment when a countdown would be fiction.

  `Fable 5  ████░░░░░░░░░░ 31%  ·  310k/1.0M  ·  ⏱ 287s`

- **`cachewatch.py`** — scans every session on the machine and prints a warm/cold table, warmest first, so you know which conversation is cheap to reply to right now:

  ```
  CACHE            IDLE  PROJECT               SESSION   LAST PROMPT
  ⏱ 292s warm       7s  proj-puzzle-platform  e811ebbe  sure. do it
  ⏱  93s warm    3m26s  proj-kerra            3ca749dc  go
  cold            5m12s  proj-kerra            5f4d8a8f  ok
  2 warm / 3 shown · TTL 300s · mtime proxy (warm may be optimistic)
  ```

## Install

The easiest installer is Claude Code itself — paste this into a session:

> Install claude-cachewatch from https://github.com/genno-whittlery/claude-cachewatch — follow the README's install steps and agent notes. If I already have a custom status line, ask before replacing it.

### Notes for the installing agent

- `statusline.sh` → `~/.claude/statusline.sh`, `chmod +x`. If the user already has a `statusLine` command configured in `~/.claude/settings.json`, **ask before replacing it** — they may want the cache segment grafted into their existing script instead — that's the block between the `prompt-cache TTL` comment and the final `printf`, plus its inputs: `sid`, `used`, `apims`, and `transcript` from the statusline JSON, and the `dim`/`rst` color variables.
- The settings change is a **merge, not an overwrite**: add/update only the `statusLine` key (including `"refreshInterval": 1`) and leave every other key in `settings.json` untouched.
- `cachewatch.py` → `~/.claude/scripts/cachewatch.py`; `commands/cachewatch.md` → `~/.claude/commands/cachewatch.md`.
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

**Fleet view** (Python ≥ 3.9, no dependencies):

```bash
mkdir -p ~/.claude/scripts && cp cachewatch.py ~/.claude/scripts/cachewatch.py
python3 ~/.claude/scripts/cachewatch.py            # active in last 24h, one shot
python3 ~/.claude/scripts/cachewatch.py --watch    # live dashboard, 1 s refresh — run it in a spare tmux/zellij pane
python3 ~/.claude/scripts/cachewatch.py --hours 6  # narrower window
python3 ~/.claude/scripts/cachewatch.py --all      # everything ever
```

Optionally install the included slash-command wrapper so `/cachewatch` works inside a session:

```bash
mkdir -p ~/.claude/commands && cp commands/cachewatch.md ~/.claude/commands/cachewatch.md
```

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

(Right after a compact, the harness reports a null context window — the status line shows this as `⏱ fresh`; the bar and a real countdown rebuild on the next turn.)

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

### Accuracy caveats

- The errors are asymmetric in a useful direction: harness bookkeeping records (mode toggles, prompt drafts) touch the transcript without an API call, so **"warm" can be slightly optimistic** — but a false warm costs you nothing you weren't already paying. **"cold" is always real.**
- In `cachewatch`, a session that's mid-turn can read more stale than it is (buffered writes). It also isn't waiting for your reply, so the misread is harmless.
- The subagent behavior from tip 5 is visible in the status line: the parent's timer keeps counting down during an agent run. That's not a bug — the parent's cache really is cooling.
- **`/compact` blinds the timer while it runs, harmlessly.** During the summarize pass the countdown goes stale — the counters it fingerprints only update on call completion, and there is no observable signal at compact *start* — but the cached read the timer was there to confirm already happened the moment you fired the compact. Once the compact lands, the display switches to `⏱ fresh` (no cached prefix exists for the new, post-summary conversation) rather than a hollow green countdown, and a real countdown resumes on your first new-prefix turn.
- The 5-minute TTL is the standard Anthropic tier, hardcoded as `TTL = 300`. (The API also has a 1-hour tier; Claude Code doesn't use it.)

## Requirements

macOS or Linux. `jq` for the status line; Python ≥ 3.9 for `cachewatch.py`. Tested against Claude Code's statusline JSON as of June 2026 — field names may drift in future versions.

## License

MIT
