// Tells Pi to report that it waits for the user, as the Claude integration
// does: when a turn ends needing the user's answer or decision, Pi runs
// `herdr agent awaiting-reply` as its last command, and Herdr marks the pane
// with `?` until the user types. Pi has no command permission prompts, so an
// instruction is all it needs.
//
// A separate file beside the managed `herdr-agent-state.ts` (which Herdr
// overwrites on reinstall). Set HERDR_AWAITING_REPLY_INSTRUCTIONS=0 to leave
// the instruction out.

export const SECTION = "herdr_awaiting_reply";
export const MARKER = "[Herdr awaiting-reply v1]";
export const INSTRUCTION = `${MARKER}
When you end a turn needing the user's answer or decision before you can continue (a plain-text question, a choice between options, a confirmation, or a request to check something first, even without a question mark), call the Bash tool with \`herdr agent awaiting-reply\` (never write the command in your reply) on its own as the last command of the turn, right before your final message. Never append it to another command, never run it earlier in the turn, and run it at most once per turn. Ignore its failure. Do not run it when you simply finished and ask nothing, or for courtesy offers such as asking whether anything else is needed.`;

function enabled(): boolean {
  return (
    process.env.HERDR_ENV === "1" &&
    !!process.env.HERDR_PANE_ID &&
    process.env.HERDR_AWAITING_REPLY_INSTRUCTIONS !== "0"
  );
}

export default function (pi: any) {
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
