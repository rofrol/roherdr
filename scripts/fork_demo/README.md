# Fork demo video

`assets/fork-demo.mp4` is the demo linked from the top of the root README. It
is a screen recording of a real herdr client in a real Ghostty window, driven
by real mouse and key events:

1. clicking the sidebar usage footer opens the usage popup;
2. middle-clicking a tab closes it;
3. middle-clicking a space closes it;
4. an agent finishes in a background tab, and clicking its notification jumps
   to that tab.
5. a long job runs in a child tab of the agent's tab (the second tab row), and
   the space's row counts running and failed jobs (`⧖ 1 !1`) while the agent
   is idle.
6. the herdr menu's **oracle stats** item opens the oracle plugin's stats
   popup over a dimmed background, and a click outside closes it.

## Re-record

Requirements: macOS, Ghostty in `/Applications`, `cargo` (the repo builds with
Zig 0.16.0), a C compiler (`cc`), `python3`, `uv`, and `ffmpeg`. The terminal
you run it from needs the Screen Recording and Accessibility permissions
(macOS asks on the first run). For real numbers in the usage footer, be logged
in to Claude Code, to Codex with ChatGPT, and to the Antigravity CLI (`agy`)
for Gemini, and log in to DeepSeek and OpenRouter in pi
(`~/.pi/agent/auth.json`); a missing DeepSeek key shows `!`, a missing
OpenRouter key hides its row.

```sh
scripts/fork_demo/record.sh              # writes assets/fork-demo.mp4
scripts/fork_demo/record.sh /tmp/demo.mp4
```

It takes over the mouse and keyboard for about a minute: don't touch them
until it prints `wrote ...`. `HERDR_DEMO_KEEP=1` keeps the work directory
(logs, raw capture) for debugging.

The script:

- builds the debug binary and starts it as a throwaway server with its own
  socket, config, and state under `/tmp/herdr-demo.*`, so your running herdr
  sessions are untouched and nothing is restored from earlier runs;
- copies your usage cache into it and sets a day-long refresh interval, so
  the footer shows your cached numbers instead of refetching every provider
  (Claude's usage endpoint rate-limits that). Opening the usage popup still
  refreshes; if your own session is rate-limited, wait until its footer loses
  the `!` before recording;
- uses the `one-light` theme and a plain zsh prompt, and turns notifications
  off (see below);
- creates the `herdr` space (tabs `code`, `agent`, `logs`), `website`, and
  `notes`, and marks the `agent` pane as a working `claude` agent through
  `herdr pane report-agent`; the agent's output is placeholder text;
- links the [oracle plugin](../../plugins/oracle/README.md), whose stats
  popup reads the real oracle log in `~/.local/state/oracle/`;
- runs `record.py`, which opens a Ghostty window (104x30, `Atom One Light`,
  its own config, no title bar) in the top-right corner of the screen. The
  window runs `proxy.py`: the herdr client in a pseudo-terminal, with a copy
  of its output parsed by libghostty-vt (the VT parser herdr vendors, loaded
  through `ghostty_vt.py`), so `record.py` can find what to click by the text
  on screen;
- moves and clicks the real mouse with Quartz events (`cliclick` has no
  middle click) and presses keys the same way, and captures the window's
  part of the screen with `ffmpeg` (avfoundation), showing the cursor and its
  clicks;
- nests the job tabs itself with `herdr tab parent` and sets their
  `herdr tab status`, as `herdr-job` does; no command runs in them;
- adds a caption bar under the capture and encodes an H.264 MP4. Captions
  are timed against the capture's wall-clock timestamps.

macOS hides notification banners while the screen is recorded, so the
notification card is drawn onto the video. The pointer moves to it, and
instead of a click (which would land on herdr's tab bar under the card) the
script runs what a click on the real banner runs: `herdr agent focus <pane>`
against the session socket.

To change the scenes, captions, or timing, edit `scenes()` in `record.py`.

## Update the README video

GitHub plays inline only videos uploaded as attachments, not MP4 files from
the repository:

1. Open <https://github.com/rofrol/herdr/edit/master/README.md>.
2. Drag the new MP4 into the editor (any GitHub comment box works too) and
   wait for the upload; GitHub inserts a
   `https://github.com/user-attachments/assets/...` link.
3. Copy that link and cancel the edit.
4. Replace the `user-attachments` line near the top of `README.md` with it,
   and commit the new `assets/fork-demo.mp4` together with the README change.

### Uploading as an agent

With the Claude in Chrome extension (Chrome logged in to GitHub), pass the MP4
to the edit page's `#blob-dragged-file-input` with the file upload tool, copy
the link, and close the tab. If the browser tools are missing, restart with
`claude --continue --chrome`: `/chrome` may show "Status: Disabled" and
"Extension: Not detected" even with the extension installed (on macOS the
terminal can't read Chrome's profile, TCC). "Select browser…" appears only
once the status is Enabled.
