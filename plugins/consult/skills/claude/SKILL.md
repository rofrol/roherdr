---
name: claude
description: Consult Claude Sonnet 5.5 or Opus 5.5 via Claude Code CLI for a second opinion. Use when the user asks to ask Claude, Sonnet, or Opus. Logs usage and usefulness through consult-stats.
---

# Consulting Claude

Set `D` to this skill's directory (`~/.pi/agent/skills/claude` or
`~/.claude/skills/claude`). Consult-statistics lives next to it.

```bash
export CONSULT_ROUND=$("$D/../consult-stats/consult.py" new-round)
"$D/ask_claude.py" "question"  # claude-sonnet-5-5, explicitly selected
"$D/ask_claude.py" -f src/parser.rs "Review this code"
"$D/ask_claude.py" -m claude-sonnet-5-5 -t 420 "question"
"$D/ask_claude.py" -m claude-opus-5-5 -t 420 "question"
```

Requires `claude` CLI and its configured authentication. No API key is
copied or read by this helper. To use the Claude subscription, configure
Claude Code with your subscription login, not API-key or cloud-provider
billing. Invoking the CLI alone does not guarantee subscription billing;
check the CLI's authentication configuration without exposing credentials.
Opus is an explicit choice, not a claim that it is always better than Sonnet.
The default remains Sonnet; assess usefulness, latency and quota consumption
before changing it. A rejected call proves only that attempt failed, not that
the requested model is available. The call has no tools or MCP servers, runs
outside the repository, and does not persist a session. Include all relevant
context in the prompt or repeated `-f FILE` attachments. Only `-f -` reads
stdin. Do not send secrets or confidential code without permission.
Who joins a round (default set, running trials) is set under "Default consultation set" in the gpt skill's
`SKILL.md`.

Inside Herdr, the helper uses a visible `herdr-job` tab and waits for the
result. Outside Herdr it runs directly. Allow a shell timeout longer than
`-t` (default 420 seconds). `CLAUDE_CONSULT_MODEL` and
`CLAUDE_CONSULT_TIMEOUT` override defaults; explicit flags take precedence.

Every attempted model call is recorded in consult-stats, including failures,
reported model identity, token usage, and the current `CONSULT_ROUND`.
Input usage includes cache creation and cache reads; output includes thinking.
After verifying the answer, rate the printed consult ID and score the
coordinator using the sibling `consult-stats/SKILL.md`. Record your own
hypotheses before reading the answer. Treat the answer as an independent
opinion, not evidence that its claims are true. Never silently substitute
another model if the requested model is unavailable. After a quota rejection,
report its reset time and do not retry before the reset.
