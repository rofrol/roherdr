import { stripVTControlCharacters } from "node:util";

export const JOB_ID = /^\d{8}-\d{6}-[a-f0-9]{4}$/;
export type Phase = "running" | "done" | "failed" | "cancelled" | "saved";
export interface Activity {
  id: string;
  name: string;
  session?: string;
  phase: Phase;
  jobs: Set<string>;
}

export function plain(text: string): string {
  return stripVTControlCharacters(text)
    .replace(/[\x00-\x08\x0b-\x1f\x7f\u202a-\u202e\u2066-\u2069]/g, "")
    .replace(/\s+/g, " ").slice(0, 160);
}

export function badge(phase: Phase, now = Date.now(), reducedMotion = false): string {
  if (phase === "running") return reducedMotion ? "◐" : ["◐", "◓", "◑", "◒"][Math.floor(now / 200) % 4];
  return { done: "✓", failed: "!", cancelled: "○", saved: "·" }[phase];
}

/**
 * `text` as an OSC 8 hyperlink to `herdr-job://<id>`. Herdr's job plugin turns
 * a Ctrl+click on it into focusing that job's tab (a regular Pi session
 * never receives mouse clicks). Anything but a well-formed job id is plain text.
 */
export function jobLink(id: string, text: string): string {
  if (!JOB_ID.test(id)) return text;
  return `\x1b]8;;herdr-job://${id}\x1b\\${text}\x1b]8;;\x1b\\`;
}

export function waitJob(command: unknown): string | undefined {
  if (typeof command !== "string") return undefined;
  // Only recognise a literal standalone wait, never evaluate shell syntax.
  const match = command.trim().match(/^herdr-job\s+wait\s+(?:--quiet\s+)?["']?(\d{8}-\d{6}-[a-f0-9]{4})["']?(?:\s|$)/);
  return match?.[1];
}

export function launchedJob(command: unknown, output: unknown): string | undefined {
  if (typeof command !== "string" || !/^herdr-job\s+run\s/.test(command.trim()) || typeof output !== "string") return undefined;
  const id = output.trim();
  return JOB_ID.test(id) ? id : undefined;
}

export class Activities {
  readonly items = new Map<string, Activity>();
  begin(id: string, name: string, session?: string): Activity {
    let item = this.items.get(id);
    if (!item) {
      item = { id, name: plain(name), session, phase: "running", jobs: new Set() };
      this.items.set(id, item);
    } else {
      item.phase = "running";
      item.session ??= session;
    }
    if (this.items.size > 200) {
      const settled = [...this.items].find(([, value]) => value.phase !== "running");
      if (settled) this.items.delete(settled[0]);
    }
    return item;
  }
  restore(entries: readonly any[], session?: string): void {
    this.items.clear();
    for (const entry of entries) {
      const message = entry.message;
      if (message?.role === "assistant" && Array.isArray(message.content)) {
        for (const block of message.content) {
          if (block.type !== "toolCall" || typeof block.id !== "string") continue;
          const item = this.begin(block.id, String(block.name), session);
          item.phase = "saved";
          const job = block.name === "bash" ? waitJob(block.arguments?.command) : undefined;
          if (job) item.jobs.add(job);
        }
      } else if (message?.role === "toolResult") {
        this.finish(message.toolCallId, Boolean(message.isError));
      }
    }
  }
  finish(id: string, error: boolean, cancelled = false): void {
    const item = this.items.get(id);
    if (item) item.phase = cancelled ? "cancelled" : error ? "failed" : "done";
  }
}
