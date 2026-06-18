---
description: "Live one-line-per-session dashboard of all Claude Code agents (state/cache/subagents/tab/doing)"
---
Run this and report the output **inside a fenced code block** (```), verbatim, NO markdown table (a table reflows to one-line-per-column on narrow widths):

```
python3 ~/.claude/scripts/agents.py --width 60
```

`--width 60` keeps each line inside a narrow chat view. Live pane the user runs themselves (auto-detects real width): `python3 ~/.claude/scripts/agents.py --watch`
