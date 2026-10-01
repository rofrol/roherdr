import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

const hook = join(import.meta.dir, "herdr-agent-state.sh");
const marker = "[Herdr behavior context v1]";
const run = (payload: unknown, overrides: Record<string, string> = {}, action = "session") => {
  const env = { ...process.env, HERDR_ENV: "1", HERDR_PANE_ID: "context:p1",
    HERDR_SOCKET_PATH: "/var/tmp/herdr-unused-context-test.sock",
    HERDR_AWAITING_REPLY_INSTRUCTIONS: "1", HERDR_AGENT_CONTEXT: "1", ...overrides };
  delete env.CURSOR_VERSION;
  if (overrides.CURSOR_VERSION !== undefined) env.CURSOR_VERSION = overrides.CURSOR_VERSION;
  const result = Bun.spawnSync(["sh", hook, action], {
    env, stdin: new TextEncoder().encode(JSON.stringify(payload)), stdout: "pipe", stderr: "pipe", timeout: 5000,
  });
  expect(result.exitCode).toBe(0);
  expect(result.stderr.toString()).toBe("");
  return result.stdout.toString();
};

const unixTest = process.platform === "win32" ? test.skip : test;

unixTest("Claude emits one native SessionStart context on startup, resume and compaction", () => {
  for (const source of ["startup", "resume", "compact", "clear", "fork"]) {
    const output = run({ hook_event_name: "SessionStart", source });
    const parsed = JSON.parse(output).hookSpecificOutput;
    expect(parsed.hookEventName).toBe("SessionStart");
    expect(parsed.additionalContext.split(marker)).toHaveLength(2);
    expect(parsed.additionalContext).toContain("herdr agent awaiting-reply");
    expect(parsed.additionalContext).toContain("earlier turn");
    expect(parsed.additionalContext).toContain("approval requirements");
    expect(output.trim().split("\n")).toHaveLength(1);
  }
});

unixTest("Claude behavior and awaiting-reply opt-outs remain independent", () => {
  const payload = { hook_event_name: "SessionStart", source: "startup" };
  const behaviorOnly = JSON.parse(run(payload, { HERDR_AWAITING_REPLY_INSTRUCTIONS: "0" })).hookSpecificOutput.additionalContext;
  expect(behaviorOnly).toContain(marker);
  expect(behaviorOnly).not.toContain("herdr agent awaiting-reply");
  const replyOnly = JSON.parse(run(payload, { HERDR_AGENT_CONTEXT: "0" })).hookSpecificOutput.additionalContext;
  expect(replyOnly).not.toContain(marker);
  expect(replyOnly).toContain("herdr agent awaiting-reply");
  expect(run(payload, { HERDR_AGENT_CONTEXT: "0", HERDR_AWAITING_REPLY_INSTRUCTIONS: "0" })).toBe("");
});

unixTest("Claude context stays silent outside Herdr, in subagents and for other events", () => {
  const payload = { hook_event_name: "SessionStart" };
  for (const env of [{ HERDR_ENV: "0" }, { HERDR_PANE_ID: "" }, { HERDR_SOCKET_PATH: "" }, { CURSOR_VERSION: "test" }]) {
    expect(run(payload, env)).toBe("");
  }
  expect(run({ ...payload, agent_id: "subagent" })).toBe("");
  expect(run({ ...payload, cursor_version: "test" })).toBe("");
  expect(run({ hook_event_name: "Stop" })).toBe("");
  expect(run({})).toBe("");
});

unixTest("Claude per-prompt reminder does not repeat the behavior contract", () => {
  const output = run({}, {}, "reminder");
  const context = JSON.parse(output).hookSpecificOutput;
  expect(context.hookEventName).toBe("UserPromptSubmit");
  expect(context.additionalContext).not.toContain(marker);
  expect(context.additionalContext).toContain("herdr agent awaiting-reply");
});

test("Pi and both Claude assets carry the same behavior contract", () => {
  const body = /You are running in a Herdr pane\.[\s\S]*?This guidance is not permission to bypass them\./;
  const pi = readFileSync(join(import.meta.dir, "../pi/herdr-agent-state.ts"), "utf8").match(body)?.[0];
  expect(pi).toBeDefined();
  for (const file of ["herdr-agent-state.sh", "herdr-agent-state.ps1"]) {
    const text = readFileSync(join(import.meta.dir, file), "utf8");
    expect(text).toContain(marker);
    expect(text.match(body)?.[0]).toBe(pi);
  }
});
