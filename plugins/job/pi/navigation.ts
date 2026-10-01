import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { readFile, readdir } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { JOB_ID } from "./model.ts";

const exec = promisify(execFile);
export async function command(program: string, args: string[]): Promise<string> {
  const result = await exec(program, args, { timeout: 15000, maxBuffer: 4 * 1024 * 1024 });
  return result.stdout;
}

export function matchesJob(meta: any, id: string, owner: string, tab: any): boolean {
  if (!JOB_ID.test(id) || meta?.id !== id || meta.owner_pane !== owner || !owner || !tab) return false;
  if (meta.tab_id !== tab.tab_id || typeof meta.name !== "string") return false;
  const name = meta.name.split(/\s+/u).filter(Boolean).join(" ");
  const short = [...name].length <= 30 ? name : [...name].slice(0, 29).join("") + "…";
  const labels = meta.tab_status ? [short] : ["⧖", "✓", "✗"].map(icon => `${icon} ${short}`);
  return labels.includes(tab.label);
}

// Invoked only by a click/command, never by a renderer, timer or lifecycle event.
export async function focusJob(id: string, owner: string, run = command,
  root = join(homedir(), ".local/state/herdr-job")): Promise<boolean> {
  if (!JOB_ID.test(id)) return false;
  let meta: any;
  try { meta = JSON.parse(await readFile(join(root, id, "meta.json"), "utf8")); }
  catch { return false; }
  if (meta.id !== id || meta.owner_pane !== owner || !owner) return false;
  // Reject reused tab identities before issuing any focus request.
  for (const other of await readdir(root)) {
    if (!JOB_ID.test(other) || other === id) continue;
    try {
      const newer = JSON.parse(await readFile(join(root, other, "meta.json"), "utf8"));
      if (newer.tab_id === meta.tab_id && newer.created > meta.created) return false;
    } catch { /* Removed/unfinished metadata does not identify a tab. */ }
  }
  const tabs = JSON.parse(await run("herdr", ["tab", "list"])).result?.tabs;
  const tab = tabs?.find((value: any) => value.tab_id === meta.tab_id);
  if (!matchesJob(meta, id, owner, tab)) return false;
  await run("herdr", ["tab", "focus", meta.tab_id]);
  return true;
}
