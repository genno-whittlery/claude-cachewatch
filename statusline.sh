#!/usr/bin/env bash
# Claude Code status line — model + context-window usage with a colored bar.
# Receives a JSON blob on stdin (schema: model.*, context_window.*, etc.).
# Docs: https://code.claude.com/docs/en/statusline
#
# Renders e.g.:   Opus 4.8 (1M context)  ███████░░░░░░░ 27%  ·  275k/1M
#
# Null-safety note: context_window.used_percentage is null BEFORE the first
# API call and immediately AFTER /compact, then repopulates. The `// 0`
# defaults below keep the line from blanking/erroring in that window.

input="$(cat)"

# jq is required; degrade gracefully if it's missing.
if ! command -v jq >/dev/null 2>&1; then
  printf 'Claude  (install jq for context usage)'
  exit 0
fi

# --- extract fields, null-safe ---------------------------------------------
IFS=$'\t' read -r model pct size used transcript sid apims < <(
  jq -r '
    [ (.model.display_name // "Claude"),
      (.context_window.used_percentage // 0),
      (.context_window.context_window_size // 0),
      (.context_window.total_input_tokens // 0),
      (.transcript_path // ""),
      (.session_id // ""),
      (.cost.total_api_duration_ms // 0)
    ] | @tsv' <<<"$input"
)

# integer percent (strip any decimal)
pct_i=${pct%.*}; pct_i=${pct_i:-0}
(( pct_i < 0 )) && pct_i=0
(( pct_i > 100 )) && pct_i=100

# --- humanize a token count: 1234567 -> 1.2M, 27450 -> 27k -------------------
hum() {
  local n=$1
  if   (( n >= 1000000 )); then printf '%d.%dM' $(( n / 1000000 )) $(( (n % 1000000) / 100000 ))
  elif (( n >= 1000 ));    then printf '%dk' $(( n / 1000 ))
  else printf '%d' "$n"
  fi
}

# --- progress bar (14 cells) ------------------------------------------------
cells=14
filled=$(( pct_i * cells / 100 ))
(( filled > cells )) && filled=cells
empty=$(( cells - filled ))
bar=""
for ((i=0; i<filled; i++)); do bar+="█"; done
for ((i=0; i<empty;  i++)); do bar+="░"; done

# --- color thresholds (green < 70 <= yellow < 90 <= red) --------------------
if   (( pct_i >= 90 )); then col=$'\033[31m'   # red
elif (( pct_i >= 70 )); then col=$'\033[33m'   # yellow
else                         col=$'\033[32m'   # green
fi
bold=$'\033[1m'; dim=$'\033[2m'; rst=$'\033[0m'

# context-size + used readout (e.g. 275k/1M); only when size is known
readout=""
if (( size > 0 )); then
  readout="${dim}  ·  $(hum "$used")/$(hum "$size")${rst}"
fi

# --- prompt-cache TTL --------------------------------------------------------
# Anthropic's prompt cache expires 5 min after the last API call that touched
# it (reads refresh the TTL too). Two proxies for "last API activity", we take
# the freshest:
#   1. Activity fingerprint: total_input_tokens + total_api_duration_ms change
#      on every API call and arrive in the statusline JSON immediately, so the
#      timer resets live while Claude is working. Tracked in a per-session
#      state file ("<fingerprint> <epoch>").
#   2. Transcript mtime: catches activity from before the state file existed.
#      (Transcript writes are buffered, so mtime alone lags during turns.)
cache=""
now=$(date +%s)
last=0
if [[ -n "$sid" ]]; then
  state_dir="${TMPDIR:-/tmp}/claude-cache-timer"
  mkdir -p "$state_dir" 2>/dev/null
  sf="$state_dir/$sid"
  fp="${used}:${apims}"
  prev_fp=""; prev_ts=0
  [[ -f "$sf" ]] && read -r prev_fp prev_ts < "$sf"
  if [[ "$fp" != "$prev_fp" ]]; then
    printf '%s %s\n' "$fp" "$now" > "$sf"
    last=$now
  else
    last=${prev_ts:-0}
  fi
fi
if [[ -n "$transcript" && -f "$transcript" ]]; then
  mtime=$(stat -f %m "$transcript" 2>/dev/null || stat -c %Y "$transcript" 2>/dev/null || echo 0)
  (( mtime > last )) && last=$mtime
fi
if (( last > 0 )); then
  left=$(( last + 300 - now ))
  if (( left > 0 )); then
    ccol=$'\033[32m'; (( left < 60 )) && ccol=$'\033[33m'
    cache="${dim}  ·  ${rst}${ccol}⏱ ${left}s${rst}"
  else
    cache="${dim}  ·  ⏱ cache cold${rst}"
  fi
fi

printf '%s%s%s  %s%s %d%%%s%s%s' \
  "$bold" "$model" "$rst" \
  "$col" "$bar" "$pct_i" "$rst" \
  "$readout" "$cache"
