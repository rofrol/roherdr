# Compact Pi activity

An opt-in Pi extension for Herdr (implemented against Pi 0.99.1 public APIs).
It replaces the **presentation** of the standard read, bash, edit, write and
codemode tools with a compact activity row. Tool schemas, final content,
structured results, mutation queues, permissions and codemode loadout/storage
remain with Pi's standard implementations. Do not combine it with an extension
that replaces those tools, such as a remote or sandbox tool backend.

## Try it

From this repository:

```sh
pi -e ./plugins/job/pi/index.ts
```

For persistent loading, link the **directory** (not just `index.ts`, which has
relative imports) after reviewing it:

```sh
ln -s /path/to/herdr/plugins/job/pi ~/.pi/agent/extensions/herdr-activity
```

Then use `/reload` in Pi. Do not overwrite an existing extension of that name.
Removing the link and reloading restores normal rendering. RPC and print
preserve ordinary rendering and do not emit compact UI updates.

The extension also provides `codemode` (it wraps the built-in one), so Pi warns
that the built-in `codemode` was not loaded. Silence it by switching the
built-in off in `~/.pi/agent/settings.json`:

```json
{ "extensions": ["-builtin:codemode"] }
```

Outside Herdr the extension then registers only the unchanged `codemode`, so
nothing is lost; read, bash, edit and write stay Pi's own and no row, command
or link is added.

## Interaction

- Each executing tool shows a half-circle animation and a short, generic summary.
  The summary deliberately excludes command arguments and file contents.
- A row that started or waited on a job ends in `ctrl+click opens job`, and the
  whole row is an OSC 8 link to `herdr-job://<id>`. **Ctrl+click** it and Herdr's job plugin
  (`[[link_handlers]]` in `plugins/job/herdr-plugin.toml`, `herdr-job open
  --from-click`) focuses that job's tab. This works in regular Pi, which never
  receives mouse clicks. The plugin must be installed or linked.
- Fullscreen Pi: click the row to open details. Regular Pi leaves mouse input
  to the terminal: use `/activity` to select a row instead.
- Ctrl+O still expands original input/output locally. Failures retain the
  original error renderer, and inline images remain available in Pi.
- `/activity off` expands tools and disables compact rendering; `/activity on`
  enables it again. These commands never change execution or model settings.
- `HERDR_ACTIVITY_REDUCED_MOTION=1` uses a static half-circle. Animation samples
  Pi's existing render ticks; the extension starts no animation timers.

A literal `herdr-job wait <id>` opens its existing job, including from a nested
codemode bash call. A simple `herdr-job run ...` returning just an ID is also
recognised. Shell compositions or wrapper scripts may not be recognised; they
use transcript details instead. Before focus, navigation checks private job
metadata, owner pane, current label and newer jobs reusing the same tab. Missing
or renamed tabs fall back to transcript details; endpoint errors expand locally.

If there is no existing job, a user action starts a kept **detail-viewer job**.
It reads only the selected call from the original saved Pi session. It is a
viewer, not the tool executor, and closing it does not cancel the original tool.
The job footer returns to the originating Pi tab. No saved session means local
expansion instead. The `/activity` list reconstructs the active session branch
on resume; abandoned branch entries are not included.

## Scope and privacy

The viewer shows saved tool input/output and, when provided in that assistant
message, **provider-exposed thinking text**. It does not reveal hidden internal
reasoning or encrypted thinking signatures. Pi still controls thinking display;
Ctrl+T can collapse it in the main transcript. This extension does not replace
Pi's assistant-message renderer or automatically change that preference.

Pi does not persist every live partial result. During an unfinished call the
viewer may wait for a saved record; it is not a second live provider stream.
Tool images are referenced, not decoded into the text viewer.

Original details may contain sensitive data. Open them only on a trusted local
terminal. The viewer strips terminal control sequences and writes to the job's
TTY, **not its persisted log/stdout**, so it creates no second payload log.
Terminal recording, screenshots and the original Pi session can still capture
that data. No credentials are read or scraped. JSON is treated as data, never
executed. Navigation/renderer failure never retries or cancels tools.

All Herdr endpoint requests happen on explicit user navigation, not on events,
rendering or timers. No server protocol, model calls or default model changes
are introduced. The viewer has a one-hour deadline; stopping the job stops only
the viewer. The standalone profile's own job activity should not be confused
with the original call's execution state.

## Tests

```sh
just pi-activity-test
```

Offline tests cover unchanged schemas/results/context, compact/expanded/error
rendering, non-TUI behavior, phases, literal job references, metadata ownership,
reused tabs, control sequences, and selected-only transcript viewing. An
installed-Pi SDK smoke test was also run without a model request. Live regular /
fullscreen clicking, resize and light/dark trials remain required before user
acceptance; other Pi versions need their own compatibility check.
