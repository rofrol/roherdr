// Names a Pi session after its first prompt, so the terminal title Pi emits
// ("π - <name> - <cwd>") and Herdr's sidebar label show the task, as Claude
// CLI's do. Pi never names a session by itself; it only has /name.
//
// The name is set once: a session that already has one (/name, resume) keeps
// it, and a later /name wins. No LLM call, so no cost, latency or data leaving
// the machine.

const MAX_TITLE = 48;

// C0, DEL and C1 controls (ESC starts OSC/CSI sequences), bidi marks and
// isolates, zero-width space and the BOM. ZWJ/ZWNJ stay: they join emoji and
// letters in several scripts.
const UNSAFE =
  // biome-ignore lint/suspicious/noControlCharactersInRegex: stripping them is the point
  /[\u0000-\u001f\u007f-\u009f\u200b\u200e\u200f\u202a-\u202e\u2066-\u2069\ufeff]/g;

/** A one-line, bounded title from a prompt; empty when nothing usable is left. */
export function titleFromPrompt(text: string): string {
  const clean = text.normalize("NFKC").replace(/[\r\n\t]+/g, " ").replace(UNSAFE, "").replace(/\s+/g, " ").trim();
  const chars = Array.from(
    typeof Intl.Segmenter === "function"
      ? Array.from(new Intl.Segmenter(undefined, { granularity: "grapheme" }).segment(clean), (part) => part.segment)
      : clean,
  );
  return chars.length <= MAX_TITLE ? chars.join("") : `${chars.slice(0, MAX_TITLE - 1).join("").trimEnd()}…`;
}

export default function (pi: any) {
  // True once the session has a name, however it got it.
  let named = false;
  pi.on("session_start", () => {
    named = Boolean(pi.getSessionName());
  });
  pi.on("session_info_changed", (event: { name?: string }) => {
    named = Boolean(event.name);
  });
  pi.on("input", (event: { text: string; source: string }) => {
    if (!named && event.source === "interactive" && !event.text.trimStart().startsWith("/")) {
      const title = titleFromPrompt(event.text);
      if (title) pi.setSessionName(title);
    }
    return { action: "continue" };
  });
}
