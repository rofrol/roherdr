# local.restart

Restart agent CLIs in place after they update (Claude Code says a new
version is available, pi updated) and resume their sessions, so the running
instances pick up the new binary.

- Menu actions: "Restart agent in this pane" and "Restart idle agents in
  this workspace". From a shell: `python3 restart_agent.py PANE...`,
  `--workspace [W]`, `--dry-run` to see what would run.
- Only an idle or finished agent is restarted. A working or blocked one is
  skipped, and so is Claude with unsent text in its input box (its dim
  placeholder hint does not count; if the box cannot be found, it counts as a
  draft).
- The new command is the agent's own command line, read from its process,
  so flags such as `--model` or `--dangerously-skip-permissions` stay. Old
  resume arguments (`--resume`, `--continue`, `--session-id`, pi's
  `--session`) and prompts are dropped, since a prompt would be sent again,
  and the resume arguments for the session herdr knows are added:
  `claude --resume <id>`, `pi --session <path>`.
- The agent gets SIGTERM, and once the pane's shell is back in the
  foreground (at most 15 s) the new command is run in it. An agent that was
  not started from a shell (the pane's own command) is skipped: nothing could
  start it again.
- Agents restart one at a time, so they do not compete for the same login.

Limits: Claude and pi only; no queue for busy agents yet (run it again when
they are idle); pi's input box is not checked for a draft; the environment
of the new process is the shell's, not the old process's.

Tests: `python3 -m unittest test_restart_agent` in this directory.

Setup: `herdr plugin link ~/personal_projects/herdr/plugins/restart`.
