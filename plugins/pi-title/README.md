# pi-title (Pi extensions for Herdr)

Two small Pi extensions, installed together with `plugins/pi-title/install`.

## pi-title

A Pi extension that names a session after its first prompt. Pi puts the session
name in the terminal title (`π - <name> - <cwd>`), and Herdr's sidebar labels
the pane from that title, so the tab shows the task, as it does for Claude CLI.
Without a name, Pi keeps emitting `π - <cwd>` for the whole session.

- Names once, from the first interactive prompt: one line, control and bidi
  characters removed, at most 48 graphemes. No LLM call.
- Skips slash commands, blank input and prompts that come from extensions or RPC.
- Keeps a name that is already there (`/name`, a resumed session) and never
  overwrites a later `/name`.

Install with `plugins/pi-title/install`, then `/reload` in Pi (or start a new
session). Tests run in `just maintenance-test` (`bun test plugins/pi-title/*.test.ts`).

## pi-awaiting-reply

Tells Pi to run `herdr agent awaiting-reply` as the last command of a turn
that ends needing your answer or decision, as the Claude integration does, so
Herdr marks the pane with `?` until you type. It adds a named system-prompt
section (or appends to the system prompt on older Pi), only inside Herdr's
TUI mode. Set `HERDR_AWAITING_REPLY_INSTRUCTIONS=0` to leave it out. It lives
beside the managed `herdr-agent-state.ts`, which Herdr overwrites on reinstall.
