import { describe, expect, test } from "bun:test";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Activities, badge, plain, waitJob, launchedJob, jobLink } from "./model.ts";
import { focusJob, matchesJob } from "./navigation.ts";

const id = "20261001-012400-abcd";
const meta = { id, owner_pane: "w:p1", tab_id: "w:t2", name: "Tests", tab_status: true, created: 1 };
const tab = { tab_id: "w:t2", label: "Tests" };

describe("compact activity", () => {
  test("literal wait and launch references, no arbitrary stdout IDs", () => {
    expect(waitJob(`herdr-job wait ${id}`)).toBe(id);
    expect(waitJob(`herdr-job wait --quiet '${id}'`)).toBe(id);
    expect(waitJob(`echo herdr-job wait ${id}`)).toBeUndefined();
    expect(launchedJob("herdr-job run -- true", id)).toBe(id);
    expect(launchedJob("echo hello", id)).toBeUndefined();
    expect(launchedJob("herdr-job run -- true", `${id}\nsecret`)).toBeUndefined();
  });
  test("real settlement stops animation; cancellation stays distinct", () => {
    const records = new Activities();
    records.begin("a", "bash");
    expect(badge("running", 0)).not.toBe(badge("running", 200));
    expect(badge("running", 0, true)).toBe(badge("running", 200, true));
    records.finish("a", false);
    expect(records.items.get("a")?.phase).toBe("done");
    expect(badge("done", 0)).toBe(badge("done", 999));
    records.finish("a", true, true);
    expect(records.items.get("a")?.phase).toBe("cancelled");
  });
  test("summaries strip controls, bidi and wrap, and bound input", () => {
    expect(plain("\x1b[31mhello\x1b[0m\n\u202e world")).toBe("hello world");
    expect(plain("a".repeat(200)).length).toBe(160);
  });
  test("owner, job and label must all match", () => {
    expect(matchesJob(meta, id, "w:p1", tab)).toBe(true);
    expect(matchesJob(meta, id, "other", tab)).toBe(false);
    expect(matchesJob(meta, "../../auth", "w:p1", tab)).toBe(false);
    expect(matchesJob(meta, id, "w:p1", { ...tab, label: "Renamed" })).toBe(false);
  });
  test("stale/reused identities never focus", async () => {
    const root = await mkdtemp(join(tmpdir(), "activity-nav-"));
    const requests: string[][] = [];
    const run = async (_program: string, args: string[]) => {
      requests.push(args);
      return JSON.stringify({ result: { tabs: [tab] } });
    };
    try {
      await mkdir(join(root, id));
      await writeFile(join(root, id, "meta.json"), JSON.stringify(meta));
      expect(await focusJob(id, "w:p1", run, root)).toBe(true);
      expect(requests).toEqual([["tab", "list"], ["tab", "focus", "w:t2"]]);
      requests.length = 0;
      const newer = "20261001-012500-abcd";
      await mkdir(join(root, newer));
      await writeFile(join(root, newer, "meta.json"), JSON.stringify({ ...meta, id: newer, created: 2 }));
      expect(await focusJob(id, "w:p1", run, root)).toBe(false);
      expect(requests).toEqual([]);
    } finally { await rm(root, { recursive: true }); }
  });
});

test("a job id becomes a herdr-job link, anything else stays plain text", () => {
  expect(jobLink("20261001-124548-1827", "open job")).toBe(
    "\x1b]8;;herdr-job://20261001-124548-1827\x1b\\open job\x1b]8;;\x1b\\",
  );
  for (const bad of ["", "../../etc", "20261001-124548-1827/x", "x\x1b]8;;http://evil\x1b\\"]) {
    expect(jobLink(bad, "open job")).toBe("open job");
  }
});
