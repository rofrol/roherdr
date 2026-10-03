// Tells Pi to report that it waits for the user, as the Claude integration
// does: when a turn ends needing the user's answer or decision, Pi runs
// `herdr agent awaiting-reply` as its last command, and Herdr marks the pane
// with `?` until the user types. Pi has no command permission prompts, so an
// instruction is all it needs.
//
// A model sometimes ends such a turn without running it (or writes the command
// in its message instead of running it). `agent_before_settle` is Pi's last
// boundary before a run ends, so a check there asks once, as the Claude Stop
// hook does: when the final message looks like a question and the command was
// not run in the turn, it appends a hidden message and continues one request.
// HERDR_AWAITING_REPLY_STOP=0 turns the check off, =shadow only logs it to
// ~/.local/state/herdr/awaiting-reply-stop.jsonl.
//
// A separate file beside the managed `herdr-agent-state.ts` (which Herdr
// overwrites on reinstall). Set HERDR_AWAITING_REPLY_INSTRUCTIONS=0 to leave
// the instruction and the check out.

import { appendFileSync, mkdirSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

export const SECTION = "herdr_awaiting_reply";
export const MARKER = "[Herdr awaiting-reply v1]";
export const INSTRUCTION = `${MARKER}
When you end a turn needing the user's answer or decision before you can continue (a plain-text question, a choice between options, a confirmation, or a request to check something first, even without a question mark), call the Bash tool with \`herdr agent awaiting-reply\` (never write the command in your reply) on its own as the last command of the turn, right before your final message. Never append it to another command, never run it earlier in the turn, and run it at most once per turn. Ignore its failure. Do not run it when you simply finished and ask nothing, or for courtesy offers such as asking whether anything else is needed.`;

export const COMMAND = "herdr agent awaiting-reply";
export const CUSTOM_TYPE = "herdr_awaiting_reply";

// The same question heuristic as scripts/awaiting_reply_audit.py (a test keeps them equal).
const ASK_PHRASES =
  /\b(let me know|tell me|which (one|option|variant)|should i|shall i|do you want|would you like|want me to|how would you like|czy mam|czy chcesz|daj (mi )?zna[ćc]|powiedz|który wariant|którą opcję|co wybierasz|jak wolisz|zainstalować|wypchnąć|zrobić)\b/i;
const COURTESY =
  /(anything else|something else|coś jeszcze|czy mogę jeszcze w czymś pomóc|let me know if you (need|have) anything)/i;

function stripCode(text: string): string {
  return text
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/`[^`\n]*`/g, " ")
    .split("\n")
    .filter((line) => !line.trimStart().startsWith(">"))
    .join("\n");
}

function paragraphs(text: string): string[] {
  return text
    .split(/\n\s*\n/)
    .map((p) => p.trim())
    .filter((p) => p);
}

export function lastParagraph(text: string): string {
  return paragraphs(stripCode(text)).pop() ?? "";
}

export function looksLikeQuestion(text: string): boolean {
  const paragraph = lastParagraph(text);
  if (!paragraph || COURTESY.test(paragraph)) {
    return false;
  }
  const tail = paragraph.slice(-400);
  if (/\?[\s)"'»”*_]*$/.test(tail)) {
    return true;
  }
  const sentences = tail.split(/(?<=[.!?])\s+/);
  const last = sentences.slice(-2).join(" ");
  return last.includes("?") && ASK_PHRASES.test(last);
}

/** The final paragraph is the command written out, not run (small models do this). */
export function printedCommand(text: string): boolean {
  const last = paragraphs(text).pop();
  if (!last) {
    return false;
  }
  return last.replaceAll("`", "").trim().replace(/^\$\s*/, "").trim() === COMMAND;
}

function textOf(content: any): string {
  if (typeof content === "string") {
    return content;
  }
  return (Array.isArray(content) ? content : [])
    .filter((b: any) => b?.type === "text")
    .map((b: any) => b.text ?? "")
    .join("\n");
}

/** What the current turn (everything after the last user message) did. */
export function checkTurn(messages: any[]) {
  let start = -1;
  messages.forEach((m, i) => {
    if (m?.role === "user" && textOf(m.content).trim()) {
      start = i;
    }
  });
  let reported = false;
  let finalText = "";
  for (const m of messages.slice(start + 1)) {
    if (m?.role !== "assistant") {
      continue;
    }
    for (const b of Array.isArray(m.content) ? m.content : []) {
      if (b?.type === "toolCall" || b?.type === "tool_use") {
        const args = b.arguments ?? b.input ?? {};
        if (/^bash$/i.test(b.name ?? "") && String(args.command ?? "").trim() === COMMAND) {
          reported = true;
        }
      } else if (b?.type === "text" && String(b.text ?? "").trim()) {
        finalText = b.text;
      }
    }
  }
  const printed = printedCommand(finalText);
  const question = looksLikeQuestion(finalText) || printed;
  return { turn: start, reported, finalText, printed, question };
}

export const NUDGE = `Herdr: your last message looks like a question for the user, but you did not run \`${COMMAND}\`. If you are waiting for the user's answer or decision, call the Bash tool with \`${COMMAND}\` now as the only command, then stop without repeating your message. If you are not waiting for the user, just stop.`;
export const NUDGE_PRINTED = `Herdr: you wrote \`${COMMAND}\` in your message instead of running it. Call the Bash tool with the command \`${COMMAND}\` now, as the only command, then stop without repeating your message.`;

function logStop(entry: Record<string, unknown>) {
  try {
    const dir = join(process.env.XDG_STATE_HOME || join(homedir(), ".local", "state"), "herdr");
    mkdirSync(dir, { recursive: true });
    appendFileSync(
      join(dir, "awaiting-reply-stop.jsonl"),
      JSON.stringify({ time: Date.now() / 1000, pane: process.env.HERDR_PANE_ID, agent: "pi", ...entry }) + "\n",
    );
  } catch {
    // The log is a diagnostic aid only.
  }
}

function enabled(): boolean {
  return (
    process.env.HERDR_ENV === "1" &&
    !!process.env.HERDR_PANE_ID &&
    process.env.HERDR_AWAITING_REPLY_INSTRUCTIONS !== "0"
  );
}

export default function (pi: any) {
  // The user message of the last turn we asked about: one ask per turn.
  let askedForTurn: unknown;
  pi.on("agent_before_settle", (event: any, ctx: any) => {
    const mode = process.env.HERDR_AWAITING_REPLY_STOP ?? "block";
    if (!enabled() || ctx?.mode !== "tui" || mode === "0" || event?.outcome !== "completed") {
      return;
    }
    const messages = event?.context?.contextMessages;
    if (!Array.isArray(messages)) {
      return;
    }
    const verdict = checkTurn(messages);
    const key = verdict.turn >= 0 ? (messages[verdict.turn]?.timestamp ?? verdict.turn) : undefined;
    const block = verdict.question && !verdict.reported && askedForTurn !== key;
    logStop({
      question: verdict.question,
      reported: verdict.reported,
      blocked: block && mode !== "shadow",
      printed: verdict.printed,
      tail: lastParagraph(verdict.finalText).slice(-200),
    });
    if (!block || mode === "shadow") {
      return;
    }
    askedForTurn = key;
    return {
      entries: [
        {
          type: "custom_message",
          customType: CUSTOM_TYPE,
          content: verdict.printed ? NUDGE_PRINTED : NUDGE,
          display: false,
        },
      ],
      continue: true,
    };
  });
  pi.on("before_agent_start", (event: any, ctx: any) => {
    if (!enabled() || ctx?.mode !== "tui") {
      return;
    }
    // A named prompt section is kept across compaction and updated by key.
    if (event?.systemPromptOptions?.sections) {
      event.systemPromptOptions.sections[SECTION] = INSTRUCTION;
      return;
    }
    // Older Pi releases expose only the system prompt: append once.
    if (typeof event?.systemPrompt === "string" && !event.systemPrompt.includes(MARKER)) {
      return { systemPrompt: `${event.systemPrompt}\n\n${INSTRUCTION}` };
    }
  });
}
