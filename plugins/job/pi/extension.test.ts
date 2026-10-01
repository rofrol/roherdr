import { expect, test, mock } from "bun:test";

const schema = { type: "object" };
const originalResult = { content: [{ type: "text", text: "unchanged output" }], details: { untouched: true },
  structuredContent: { exit_code: 0, output: "unchanged output" } };
let calls: any[] = [];
const definition = (name: string) => ({ name, description: "original", parameters: schema,
  outputSchema: schema, defaultActive: false, exposure: name === "codemode" ? "model-only" : "direct",
  annotations: { destructiveHint: true }, promptGuidelines: ["original guideline"],
  prepareLoadout: (loadout: any) => loadout,
  execute: async (...args: any[]) => { calls.push(args); return originalResult; },
  renderCall: () => ({ render: () => ["raw call"], invalidate() {} }),
  renderResult: () => ({ render: () => ["raw result"], invalidate() {} }),
});
mock.module("@earendil-works/pi-coding-agent", () => ({
  createBashToolDefinition: () => definition("bash"), createReadToolDefinition: () => definition("read"),
  createEditToolDefinition: () => definition("edit"), createWriteToolDefinition: () => definition("write"),
  createCodemodeExtension: () => (pi: any) => pi.registerTool(definition("codemode")),
}));
mock.module("@earendil-works/pi-tui", () => ({
  Text: class { constructor(public text: string) {} render() { return [this.text]; } invalidate() {} },
  truncateToWidth: (text: string, width: number) => text.slice(0, width),
}));
const extension = (await import("./index.ts")).default;

function setup() {
  const tools = new Map<string, any>();
  const handlers = new Map<string, any>();
  const commands = new Map<string, any>();
  const pi: any = { registerTool: (tool: any) => tools.set(tool.name, tool),
    on: (name: string, handler: any) => handlers.set(name, handler),
    registerCommand: (name: string, command: any) => commands.set(name, command), getSettings: () => ({}) };
  extension(pi);
  return { tools, handlers, commands };
}

const prior = { socket: process.env.HERDR_SOCKET_PATH, pane: process.env.HERDR_PANE_ID };
function restore() {
  if (prior.socket === undefined) delete process.env.HERDR_SOCKET_PATH; else process.env.HERDR_SOCKET_PATH = prior.socket;
  if (prior.pane === undefined) delete process.env.HERDR_PANE_ID; else process.env.HERDR_PANE_ID = prior.pane;
}

test("outside Herdr only the built-in codemode is registered, undecorated, with no commands", () => {
  delete process.env.HERDR_SOCKET_PATH;
  try {
    const { tools, commands } = setup();
    expect([...tools.keys()]).toEqual(["codemode"]);
    expect(tools.get("codemode").renderShell).toBeUndefined();
    expect(commands.size).toBe(0);
  } finally { restore(); }
});

test("schemas, results, signal, context and codemode loadout preserved", async () => {
  process.env.HERDR_SOCKET_PATH = "test"; process.env.HERDR_PANE_ID = "w:p1";
  try {
    calls = [];
    const { tools, handlers } = setup();
    const ctx: any = { mode: "tui", sessionManager: { getSessionFile: () => undefined, getBranch: () => [] }, ui: {} };
    handlers.get("session_start")({}, ctx);
    const tool = tools.get("codemode");
    expect(tool.parameters).toBe(schema); expect(tool.outputSchema).toBe(schema);
    expect(tool.defaultActive).toBe(false); expect(tool.exposure).toBe("model-only");
    expect(tool.promptGuidelines).toEqual(["original guideline"]);
    const loadout = {}; expect(tool.prepareLoadout(loadout)).toBe(loadout);
    const updates: any[] = [];
    const update = (result: any) => updates.push(result);
    const signal = new AbortController().signal;
    const args = { code: "secret code not shown in summary" };
    expect(await tool.execute("a", args, signal, update, ctx)).toBe(originalResult);
    expect(calls[0]).toEqual(["a", args, signal, update, ctx]);
    expect(updates).toEqual([{ content: [], details: undefined }]);
    const render: any = { toolCallId: "a", expanded: false, isError: false };
    const theme = { fg: (_color: string, text: string) => text };
    expect(tool.renderCall(args, theme, render).render(80)).toEqual([]);
    const row = tool.renderResult(originalResult, { isPartial: false }, theme, render);
    expect(row.render(80)[0]).toContain("✓ codemode");
    expect(row.render(80)[0]).not.toContain("secret");
    expect(row.render(1)[0].length).toBeLessThanOrEqual(1);
    expect(tool.renderResult(originalResult, { expanded: true }, theme, render).render(80)).toEqual(["raw result"]);
    expect(tool.renderResult(originalResult, {}, theme, { ...render, isError: true }).render(80)).toEqual(["raw result"]);
    handlers.get("session_shutdown")();
  } finally { restore(); }
});

test("RPC/print calls preserve ordinary updates and rendering", async () => {
  process.env.HERDR_SOCKET_PATH = "test"; process.env.HERDR_PANE_ID = "w:p1";
  try {
    const { tools, handlers } = setup();
    const ctx: any = { mode: "rpc", sessionManager: { getSessionFile: () => undefined, getBranch: () => [] }, ui: {} };
    handlers.get("session_start")({}, ctx);
    const tool = tools.get("bash"); const updates: any[] = [];
    await tool.execute("b", { command: "true" }, undefined, (value: any) => updates.push(value), ctx);
    expect(updates).toEqual([]);
    expect(tool.renderCall({}, {}, {}).render()).toEqual(["raw call"]);
  } finally { restore(); }
});
