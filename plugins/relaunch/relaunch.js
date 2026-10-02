// local.relaunch startup hook: after a herdr server restart, rerun the
// commands that were in the foreground of panes when the old server died.
// Records are written by relaunch.zsh (zsh preexec/precmd), one file per pane:
//   line 1 tab id, line 2 cwd, rest: command as typed.
//
//   node relaunch.js            restore (startup hook)
//   node relaunch.js --dry-run  print what a restore would run
//
// Runs only into a pane with the same id and tab, that is not an agent pane
// (herdr resumes those natively) and is an idle shell. Creates nothing.
const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");
const os = require("node:os");

const herdr = process.env.HERDR_BIN_PATH || "herdr";
const SOCKET = process.env.HERDR_SOCKET_PATH || path.join(os.homedir(), ".config/herdr/herdr.sock");
const BASE = path.join(process.env.XDG_STATE_HOME || path.join(os.homedir(), ".local/state"), "herdr/plugins/local.relaunch");
const DIR = path.join(BASE, SOCKET.replace(/\//g, "_")); // must match relaunch.zsh
const WAIT_MS = Number(process.env.RELAUNCH_WAIT_MS || 15000);
const SHELLS = new Set(["zsh", "bash", "fish", "sh", "dash", "nu", "login"]);
const sleep = ms => new Promise(r => setTimeout(r, ms));
const LOG = path.join(BASE, "relaunch.log");

const LOG_MAX_BYTES = 256 * 1024;

// Print and append to LOG (0600: lines hold command lines, like the records):
// the server keeps hook output only in memory, so the result of a run after a
// reboot is otherwise lost at the next server start.
function trimLog() {
  try {
    if (fs.statSync(LOG).size <= LOG_MAX_BYTES) return;
    const text = fs.readFileSync(LOG, "utf8");
    const tail = text.slice(text.indexOf("\n", text.length - LOG_MAX_BYTES / 2) + 1);
    fs.writeFileSync(LOG, tail, { mode: 0o600 });
  } catch {}
}
function log(line) {
  console.log(line);
  try { fs.appendFileSync(LOG, `${new Date().toISOString()} ${line}\n`, { mode: 0o600 }); } catch {}
}

function api(args) {
  const r = spawnSync(herdr, args, { encoding: "utf8" });
  if (r.status !== 0) throw new Error(`herdr ${args.join(" ")}: ${r.stderr || r.stdout}`);
  return r.stdout.trim() ? JSON.parse(r.stdout).result : null; // pane run prints nothing
}

// "idle" | "busy" | null (not known yet)
function paneState(paneId) {
  let info;
  try { info = api(["pane", "process-info", "--pane", paneId]).process_info; } catch { return null; }
  const fg = info.foreground_processes || [];
  if (!info.shell_pid || !fg.length) return null;
  return fg.every(p => p.pid === info.shell_pid || SHELLS.has(String(p.name).replace(/^-/, ""))) ? "idle" : "busy";
}

// `install` points BASE/relaunch.zsh (sourced from .zshrc) at this plugin;
// re-point it when the plugin has moved, e.g. after an update.
function relinkHook() {
  const link = path.join(BASE, "relaunch.zsh");
  const target = path.join(__dirname, "relaunch.zsh");
  try {
    if (fs.readlinkSync(link) === target) return;
    fs.unlinkSync(link);
    fs.symlinkSync(target, link);
    log(`relinked ${link} -> ${target}`);
  } catch {} // not installed through `install`: nothing to fix
}

const quote = s => `'${s.replace(/'/g, `'\\''`)}'`;

(async () => {
  const dryRun = process.argv[2] === "--dry-run";
  const out = dryRun ? console.log : log;
  if (!dryRun) { trimLog(); log(`run pid ${process.pid}`); relinkHook(); }
  let files = [];
  try { files = fs.readdirSync(DIR); } catch { return; }
  const live = new Map(api(["pane", "list"]).panes.map(p => [p.pane_id, p]));
  for (const file of files) {
    const full = path.join(DIR, file);
    const paneId = file.replace(/_/g, ":");
    let text;
    // A record can vanish while earlier panes are awaited; skip it, do not abort the run.
    try { text = fs.readFileSync(full, "utf8"); } catch { out(`skip ${paneId}: record gone`); continue; }
    const [tabId, cwd, ...rest] = text.replace(/\n$/, "").split("\n");
    const command = rest.join("\n");
    const pane = live.get(paneId);
    if (!pane) { out(`drop ${paneId}: pane gone`); if (!dryRun) fs.rmSync(full); continue; }
    const why = !command ? "empty" : pane.tab_id !== tabId ? "pane changed" : pane.agent || pane.agent_session ? "agent pane" : null;
    if (why) { out(`skip ${paneId}: ${why}`); continue; }
    const line = pane.cwd === cwd ? command : `cd -- ${quote(cwd)} && ${command}`;
    if (dryRun) { console.log(`would run in ${paneId}: ${line}`); continue; }
    const deadline = Date.now() + WAIT_MS;
    let s = paneState(paneId);
    while (s === null && Date.now() < deadline) { await sleep(500); s = paneState(paneId); }
    if (s !== "idle") { out(`skip ${paneId}: ${s || "unknown"}`); continue; }
    await sleep(500); // let the shell draw its prompt
    api(["pane", "run", paneId, line]);
    out(`ran in ${paneId}: ${line}`);
  }
})().catch(e => { console.error(String(e)); try { fs.appendFileSync(LOG, `${new Date().toISOString()} error ${e}\n`, { mode: 0o600 }); } catch {} process.exit(1); });
