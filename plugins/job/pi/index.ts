import type { ExtensionAPI, ExtensionContext, ToolDefinition } from "@earendil-works/pi-coding-agent";
import { createBashToolDefinition, createReadToolDefinition, createEditToolDefinition,
  createWriteToolDefinition, createCodemodeExtension } from "@earendil-works/pi-coding-agent";
import { Text, truncateToWidth, type Component, type TuiMouseEvent } from "@earendil-works/pi-tui";
import { fileURLToPath } from "node:url";
import { Activities, badge, plain, waitJob, launchedJob, jobLink, type Activity, type Phase } from "./model.ts";
import { command, focusJob } from "./navigation.ts";

type ToolRenderContext = Parameters<NonNullable<ToolDefinition["renderCall"]>>[2];
const empty: Component = { render: () => [], invalidate() {} };
const viewer = fileURLToPath(new URL("./viewer.py", import.meta.url));

export default function (pi: ExtensionAPI) {
  if (!process.env.HERDR_SOCKET_PATH || !process.env.HERDR_PANE_ID) {
    // Outside Herdr: ordinary rendering, but keep `codemode`, so the built-in
    // one can stay switched off (`-builtin:codemode`) without losing it.
    createCodemodeExtension()(pi);
    return;
  }
  const owner = process.env.HERDR_PANE_ID;
  const activities = new Activities();
  let context: ExtensionContext | undefined;
  let enabled = true;
  let navigating = false;
  const views = new Map<string, string>();
  const compact = () => enabled && context?.mode === "tui";
  const session = () => context?.sessionManager.getSessionFile();

  const open = async (activity: Activity) => {
    if (!context || navigating) return;
    navigating = true;
    try {
      const ids = [...activity.jobs];
      if (ids.length > 1) {
        const selected = await context.ui.select("Open job", ids);
        if (!selected) return;
        if (await focusJob(selected, owner)) return;
      } else if (ids[0] && await focusJob(ids[0], owner)) return;
      const existing = views.get(activity.id);
      if (existing && await focusJob(existing, owner)) return;
      if (!activity.session) {
        context.ui.setToolsExpanded(true);
        context.ui.notify("No saved transcript; tool details expanded locally.", "info");
        return;
      }
      const id = (await command("herdr-job", ["run", "--keep", "--notify", "never",
        "--name", `Pi ${plain(activity.name)} details`, "--why", "User-opened transcript details",
        "--", "python3", viewer, "--session", activity.session, "--call", activity.id])).trim();
      views.set(activity.id, id);
      if (!await focusJob(id, owner)) throw new Error("Detail tab unavailable");
    } catch {
      context.ui.setToolsExpanded(true);
      context.ui.notify("Could not open job details; tool output expanded locally.", "warning");
    } finally { navigating = false; }
  };

  class Row implements Component {
    constructor(private activity: Activity, private phase: Phase, private theme: any) {}
    render(width: number): string[] {
      const phase = this.activity.phase === "cancelled" ? "cancelled" : this.phase;
      const color = phase === "failed" ? "error" : phase === "done" ? "success" : "accent";
      const descriptions: Record<string, string> = {
        read: "Read file", edit: "Edit file", write: "Write file",
        bash: this.activity.jobs.size ? "Wait/start job" : "Shell command", codemode: "Tool batch",
      };
      // A row that started or waited on a job links to it as a whole: Ctrl+click.
      const job = [...this.activity.jobs].pop();
      const text = `${badge(phase, Date.now(), process.env.HERDR_ACTIVITY_REDUCED_MOTION === "1")} ${plain(this.activity.name)} — ${descriptions[this.activity.name] ?? "Tool operation"}: ${phase} · details${job ? " · ctrl+click opens job" : ""}`;
      const row = this.theme.fg(color, text);
      return [truncateToWidth(job ? jobLink(job, row) : row, Math.max(0, width))];
    }
    invalidate() {}
    handleMouse(event: TuiMouseEvent) {
      if (event.type !== "click" || event.button !== "left" || event.shift || event.alt || event.ctrl) return;
      void open(this.activity);
      return { handled: true, render: false };
    }
  }

  const decorate = (definition: ToolDefinition<any, any>) => {
    const originalExecute = definition.execute;
    return {
      ...definition,
      renderShell: "self" as const,
      async execute(id: string, args: any, signal: AbortSignal | undefined, update: any, ctx: any) {
        context = ctx;
        const item = activities.begin(id, definition.name, ctx.sessionManager.getSessionFile());
        const target = definition.name === "bash" ? waitJob(args.command) : undefined;
        if (target) item.jobs.add(target);
        if (compact()) update?.({ content: [], details: undefined });
        try {
          const result = await originalExecute(id, args, signal, update, ctx);
          activities.finish(id, Boolean(result.isError), signal?.aborted);
          if (definition.name === "bash") {
            const text = result.content.filter((part: any) => part.type === "text").map((part: any) => part.text).join("\n");
            const job = launchedJob(args.command, text);
            if (job) item.jobs.add(job);
          }
          return result;
        } catch (error) {
          activities.finish(id, true, signal?.aborted);
          throw error;
        }
      },
      renderCall(args: any, theme: any, ctx: ToolRenderContext) {
        if (!compact() || ctx.expanded) {
          return definition.renderCall?.(args, theme, ctx) ?? new Text(`${definition.name}\n${JSON.stringify(args, null, 2)}`, 0, 0);
        }
        return empty;
      },
      renderResult(result: any, options: any, theme: any, ctx: ToolRenderContext) {
        if (!compact() || options.expanded || ctx.isError) {
          return definition.renderResult?.(result, options, theme, ctx) ?? new Text(
            result.content.filter((part: any) => part.type === "text").map((part: any) => part.text).join("\n"), 0, 0);
        }
        const activity = activities.items.get(ctx.toolCallId) ?? {
          id: ctx.toolCallId, name: definition.name, session: session(), phase: "saved", jobs: new Set<string>(),
        };
        return new Row(activity, options.isPartial ? "running" : "done", theme);
      },
    };
  };

  // Public factories keep execution, mutation queues, loadout, schema and results intact.
  const cwd = process.cwd();
  const bash = createBashToolDefinition(cwd);
  const configuredBash: typeof bash = {
    ...bash,
    execute: (id, args, signal, update, ctx) => {
      // Action APIs are unavailable during factory loading; resolve settings at execution.
      const settings = pi.getSettings();
      return createBashToolDefinition(ctx.cwd, {
        shellPath: settings.shellPath, commandPrefix: settings.shellCommandPrefix,
      }).execute(id, args, signal, update, ctx);
    },
  };
  pi.registerTool(decorate(configuredBash));
  for (const definition of [
    createReadToolDefinition(cwd), createEditToolDefinition(cwd), createWriteToolDefinition(cwd),
  ]) pi.registerTool(decorate(definition));
  createCodemodeExtension()({ ...pi, registerTool: (definition: any) => pi.registerTool(decorate(definition)) } as ExtensionAPI);

  pi.on("session_start", (_event, ctx) => {
    context = ctx;
    activities.restore(ctx.sessionManager.getBranch(), ctx.sessionManager.getSessionFile());
  });
  pi.on("tool_execution_start", (event, ctx) => {
    context = ctx;
    if (!event.parentToolCallId || event.toolName !== "bash") return;
    const parent = activities.items.get(event.parentToolCallId);
    const job = waitJob((event.args as any)?.command);
    if (parent && job) parent.jobs.add(job);
  });
  pi.on("tool_execution_end", (event) => {
    if (!event.parentToolCallId) return;
    const parent = activities.items.get(event.parentToolCallId);
    const child = activities.items.get(event.toolCallId);
    if (parent && child) for (const id of child.jobs) parent.jobs.add(id);
  });
  pi.on("session_shutdown", () => {
    context = undefined;
    activities.items.clear();
    views.clear();
  });
  pi.registerCommand("activity", {
    description: "Open compact activity details; on/off switches rendering only",
    handler: async (args, ctx) => {
      context = ctx;
      if (args.trim() === "on" || args.trim() === "off") {
        enabled = args.trim() === "on";
        ctx.ui.setToolsExpanded(!enabled);
        ctx.ui.notify(`Compact activity ${enabled ? "enabled" : "disabled"}.`, "info");
        return;
      }
      const items = [...activities.items.values()].reverse();
      if (!items.length) { ctx.ui.setToolsExpanded(true); return; }
      const labels = items.map((item, index) => `${index + 1}. ${plain(item.name)} — ${item.phase}`);
      const selected = await ctx.ui.select("Activity details", labels);
      if (selected) await open(items[labels.indexOf(selected)]);
    },
  });
}
