# Fork demo video

`assets/fork-demo.mp4` is the demo linked from the top of the root README. It
shows the fork's features in a real herdr client:

1. clicking the sidebar usage footer opens the usage popup;
2. middle-clicking a tab closes it;
3. middle-clicking a space closes it;
4. an agent finishes in a background tab, and clicking its notification jumps
   to that tab.

## Re-record

Requirements: macOS, `cargo` (the repo builds with Zig 0.16.0), `uv`, and
`ffmpeg`. For real numbers in the usage footer, be logged in to Claude Code
and to Codex with ChatGPT, and log in to DeepSeek and OpenRouter in pi
(`~/.pi/agent/auth.json`); a missing DeepSeek key shows `!`, a missing
OpenRouter key hides its row.

```sh
scripts/fork_demo/record.sh              # writes assets/fork-demo.mp4
scripts/fork_demo/record.sh /tmp/demo.mp4
```

The script:

- builds the debug binary and starts it as a throwaway server with its own
  socket, config, and state under `/tmp/herdr-demo.*`, so your running herdr
  sessions are untouched and nothing is restored from earlier runs;
- uses the `one-light` theme, a plain zsh prompt, and turns notifications off
  so no real banner pops up while recording;
- creates the `herdr` space (tabs `code`, `agent`, `logs`), `website`, and
  `notes`, and marks the `agent` pane as a working `claude` agent through
  `herdr pane report-agent`; the agent's output is placeholder text;
- runs `record.py`, which attaches a real client in a pseudo-terminal, sends
  real left and middle mouse clicks, and renders each screen with `pyte` and
  Pillow into PNG frames with a caption bar;
- encodes the frames into an H.264 MP4 with `ffmpeg`.

macOS draws notifications outside the terminal, so the notification card in
the video is drawn onto the frame. Clicking it runs the same command a real
notification click runs: `herdr agent focus <pane>` against the session
socket.

Frames use JetBrains Mono from `~/Library/Fonts` when installed, otherwise
Menlo. Set `HERDR_DEMO_FONT` to another `.ttf` to override. To change the
scenes, captions, or timing, edit `main()` in `record.py`; each `add` or
`shot` call is one frame with its duration in milliseconds.

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
