import { afterEach, beforeEach, expect, test } from "bun:test";
import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import extension, {
  COMMAND,
  INSTRUCTION,
  MARKER,
  NUDGE,
  NUDGE_PRINTED,
  SECTION,
  looksLikeQuestion,
  printedCommand,
} from "./pi-awaiting-reply.ts";

const saved = { ...process.env };
beforeEach(() => {
  process.env.HERDR_ENV = "1";
  process.env.HERDR_PANE_ID = "w1:p1";
  delete process.env.HERDR_AWAITING_REPLY_INSTRUCTIONS;
  delete process.env.HERDR_AWAITING_REPLY_STOP;
  process.env.XDG_STATE_HOME = mkdtempSync(join(tmpdir(), "herdr-stop-"));
});
afterEach(() => {
  process.env = { ...saved };
});

function run(event: any, ctx: any = { mode: "tui" }) {
  let handler: any;
  extension({ on: (name: string, fn: any) => name === "before_agent_start" && (handler = fn) });
  return handler(event, ctx);
}

test("the instruction goes into a named prompt section and mentions the command", () => {
  const event = { systemPromptOptions: { sections: {} as Record<string, string> } };
  expect(run(event)).toBeUndefined();
  expect(event.systemPromptOptions.sections[SECTION]).toBe(INSTRUCTION);
  expect(INSTRUCTION).toContain("herdr agent awaiting-reply");
  expect(INSTRUCTION).toContain("last command of the turn");
});

test("an older Pi gets it appended to the system prompt, once", () => {
  const first = run({ systemPrompt: "base" });
  expect(first).toEqual({ systemPrompt: `base\n\n${INSTRUCTION}` });
  expect(run({ systemPrompt: first.systemPrompt })).toBeUndefined();
  expect(first.systemPrompt.split(MARKER).length).toBe(2);
});

test("nothing is added outside Herdr, outside the TUI, or when switched off", () => {
  const sections = () => ({ systemPromptOptions: { sections: {} as Record<string, string> } });
  let event = sections();
  run(event, { mode: "print" });
  expect(event.systemPromptOptions.sections).toEqual({});
  process.env.HERDR_AWAITING_REPLY_INSTRUCTIONS = "0";
  event = sections();
  run(event);
  expect(event.systemPromptOptions.sections).toEqual({});
  delete process.env.HERDR_AWAITING_REPLY_INSTRUCTIONS;
  delete process.env.HERDR_ENV;
  event = sections();
  run(event);
  expect(event.systemPromptOptions.sections).toEqual({});
});

// The turn's messages as Pi's context holds them.
const user = (text: string, timestamp = 1) => ({ role: "user", content: text, timestamp });
const said = (text: string) => ({ role: "assistant", content: [{ type: "text", text }] });
const ran = (command: string) => ({
  role: "assistant",
  content: [{ type: "toolCall", name: "bash", arguments: { command } }],
});

function settle(messages: any[], opts: { mode?: string; outcome?: string; handler?: any } = {}) {
  let handler = opts.handler;
  if (!handler) {
    extension({ on: (name: string, fn: any) => name === "agent_before_settle" && (handler = fn) });
  }
  const result = handler(
    { outcome: opts.outcome ?? "completed", context: { contextMessages: messages } },
    { mode: opts.mode ?? "tui" },
  );
  return { result, handler };
}

test("an unreported question gets one hidden nudge and a continuation", () => {
  const messages = [user("go"), said("Done.\n\nPush the commits?")];
  const { result, handler } = settle(messages);
  expect(result).toEqual({
    entries: [{ type: "custom_message", customType: "herdr_awaiting_reply", content: NUDGE, display: false }],
    continue: true,
  });
  // The turn continues and ends on the same question: it is not asked twice.
  expect(settle([...messages, said("Push the commits?")], { handler }).result).toBeUndefined();
  // A later turn is checked again.
  const next = [...messages, said("Push the commits?"), user("hmm", 2), said("Which one?")];
  expect(settle(next, { handler }).result?.continue).toBe(true);
});

test("the command written out instead of run gets the specific nudge", () => {
  const { result } = settle([user("go"), said("Masz email?\n\nherdr agent awaiting-reply")]);
  expect(result.entries[0].content).toBe(NUDGE_PRINTED);
});

test("a reported question, a statement, and a report from an earlier turn", () => {
  expect(settle([user("go"), ran(COMMAND), said("Push the commits?")]).result).toBeUndefined();
  expect(settle([user("go"), said("All done, tests pass.")]).result).toBeUndefined();
  const earlier = [user("a"), ran(COMMAND), said("Which one?"), user("b", 2), said("Want me to push?")];
  expect(settle(earlier).result?.continue).toBe(true);
});

test("the check is silent outside the TUI, off, after an abort, and in shadow mode", () => {
  const messages = [user("go"), said("Push the commits?")];
  expect(settle(messages, { mode: "print" }).result).toBeUndefined();
  expect(settle(messages, { outcome: "aborted" }).result).toBeUndefined();
  process.env.HERDR_AWAITING_REPLY_STOP = "0";
  expect(settle(messages).result).toBeUndefined();
  process.env.HERDR_AWAITING_REPLY_STOP = "shadow";
  expect(settle(messages).result).toBeUndefined();
  const log = readFileSync(join(process.env.XDG_STATE_HOME!, "herdr", "awaiting-reply-stop.jsonl"), "utf8");
  const entry = JSON.parse(log.trim().split("\n").pop()!);
  expect(entry).toMatchObject({ agent: "pi", question: true, reported: false, blocked: false });
  delete process.env.HERDR_ENV;
  delete process.env.HERDR_AWAITING_REPLY_STOP;
  expect(settle(messages).result).toBeUndefined();
});

test("its question heuristic equals the audit script's", () => {
  const questions = [
    "Done.\n\nShould I push the commits?",
    "Wypchnąć commity?",
    "Finished.\n\nAnything else?",
    "```\nwhy?\n```\n\nFixed.",
    "> quoted?\n\nFinished.",
    "Let me know which option you prefer, A or B?",
    "Done. All tests pass.",
    "Opcje:\n1. a\n2. b\n\nCo wybierasz?",
    "Zrobione. Daj znać czy mam wypchnąć?",
    "",
  ];
  const printed = [
    "Czekam?\n\nherdr agent awaiting-reply",
    "Czekam?\n\n`herdr agent awaiting-reply`",
    "Czekam?\n\n```\n$ herdr agent awaiting-reply\n```",
    "Explaining: the command herdr agent awaiting-reply marks a pane.",
  ];
  const script = `
import json, sys
sys.path.insert(0, "scripts")
import awaiting_reply_audit as a
q, p = json.loads(sys.stdin.read())
print(json.dumps([[a.looks_like_question(t) for t in q], [a.printed_command(t) for t in p]]))
`;
  const out = Bun.spawnSync(["python3", "-c", script], {
    stdin: new TextEncoder().encode(JSON.stringify([questions, printed])),
    cwd: join(import.meta.dir, "..", ".."),
  });
  expect(out.exitCode).toBe(0);
  const [expectedQ, expectedP] = JSON.parse(out.stdout.toString());
  expect(questions.map(looksLikeQuestion)).toEqual(expectedQ);
  expect(printed.map(printedCommand)).toEqual(expectedP);
});
