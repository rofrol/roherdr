import { afterEach, beforeEach, expect, test } from "bun:test";
import extension, { INSTRUCTION, MARKER, SECTION } from "./pi-awaiting-reply.ts";

const saved = { ...process.env };
beforeEach(() => {
  process.env.HERDR_ENV = "1";
  process.env.HERDR_PANE_ID = "w1:p1";
  delete process.env.HERDR_AWAITING_REPLY_INSTRUCTIONS;
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
