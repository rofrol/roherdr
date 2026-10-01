import { expect, test } from "bun:test";
import extension, { titleFromPrompt } from "./pi-title.ts";

function setup(existing?: string) {
  const handlers = new Map<string, any>();
  const names: string[] = [];
  let name = existing;
  const pi: any = {
    on: (event: string, handler: any) => handlers.set(event, handler),
    getSessionName: () => name,
    setSessionName: (next: string) => {
      names.push(next);
      name = next;
      handlers.get("session_info_changed")?.({ name: next });
    },
  };
  extension(pi);
  handlers.get("session_start")?.({});
  const send = (text: string, source = "interactive") => handlers.get("input")({ text, source });
  return { send, names, rename: (next?: string) => { name = next; handlers.get("session_info_changed")({ name: next }); } };
}

test("titles are one line, free of controls and bidi marks, and bounded", () => {
  expect(titleFromPrompt("  fix\n\tthe\x1b[31m bug ‮evil⁩  ")).toBe("fix the[31m bug evil");
  expect(titleFromPrompt("\x1b]0;owned\x07 hi")).toBe("]0;owned hi");
  expect(titleFromPrompt(" \n\t ")).toBe("");
  const long = titleFromPrompt("x".repeat(200));
  expect(Array.from(long).length).toBe(48);
  expect(long.endsWith("…")).toBe(true);
  expect(titleFromPrompt("👨‍👩‍👧".repeat(60))).toBe(`${"👨‍👩‍👧".repeat(47)}…`);
});

test("the first interactive prompt names the session, once", () => {
  const { send, names } = setup();
  expect(send("fix the auth refresh")).toEqual({ action: "continue" });
  send("now something else");
  expect(names).toEqual(["fix the auth refresh"]);
});

test("slash commands, blank input and other sources do not name the session", () => {
  const { send, names } = setup();
  send("/model");
  send("   ");
  send("from an extension", "extension");
  send("from rpc", "rpc");
  expect(names).toEqual([]);
  send("real task");
  expect(names).toEqual(["real task"]);
});

test("an existing or user-set name is kept", () => {
  const resumed = setup("resumed task");
  resumed.send("anything");
  expect(resumed.names).toEqual([]);
  const manual = setup();
  manual.rename("chosen with /name");
  manual.send("anything");
  expect(manual.names).toEqual([]);
});
