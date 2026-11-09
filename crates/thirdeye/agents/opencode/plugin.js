// thirdeye's OpenCode plugin — installed by `thirdeye install opencode`.
//
// It forwards what OpenCode does, unchanged, to `thirdeye hook --agent
// opencode`, adding only what tool events lack: the turn (the user's prompt
// message, remembered per session — a tool event carries the assistant's
// message id, which changes every step) and the directory. It interprets
// nothing; thirdeye's opencode adapter does. And it never throws into
// OpenCode: a broken viewer must not break the agent.
import { spawn } from "node:child_process";

const THIRDEYE = {{THIRDEYE}};

/** Ms a hook may hold OpenCode up for. A stuck thirdeye costs an event. */
const HOOK_MS = 2000;
/** Ms a flag may take — the model is waiting on it, not the human. */
const FLAG_MS = 5000;
/** A session with no prompt for this long is over. The shared background
 * service never shuts down, so nothing else would end it, and each session
 * keeps a viewer daemon until it ends. */
const IDLE_MS = 30 * 60 * 1000;

/** Run thirdeye; resolve with its stdout, or "" on any failure or timeout. */
function run(args, input, cwd, ms = HOOK_MS) {
  return new Promise((resolve) => {
    let out = "";
    try {
      const child = spawn(THIRDEYE, args, { cwd, stdio: ["pipe", "pipe", "ignore"] });
      const timer = setTimeout(() => {
        child.kill("SIGKILL");
        resolve("");
      }, ms);
      timer.unref?.();
      child.stdout.on("data", (chunk) => (out += chunk));
      child.on("error", () => {
        clearTimeout(timer);
        resolve("");
      });
      child.on("close", () => {
        clearTimeout(timer);
        resolve(out);
      });
      child.stdin.on("error", () => {});
      child.stdin.end(input);
    } catch {
      resolve("");
    }
  });
}

export default {
  id: "thirdeye",
  async setup(ctx) {
    const cwd = ctx.location.directory;
    const turns = new Map();
    const sessions = new Set();
    const idle = new Map();
    const send = (hook, session, event) => {
      if (session) sessions.add(session);
      let envelope;
      try {
        envelope = JSON.stringify({ hook, session, turn: turns.get(session), cwd, event });
      } catch {
        return Promise.resolve(""); // An event that will not serialize costs only itself.
      }
      return run(["hook", "--agent", "opencode"], envelope, cwd);
    };
    const end = (session) => {
      clearTimeout(idle.get(session));
      idle.delete(session);
      turns.delete(session);
      sessions.delete(session);
      return send("session.end", session, { type: "thirdeye.idle" });
    };

    // Not awaited: turns are grouped by id, so nothing needs this to land
    // before the model starts — and OpenCode would wait on it.
    await ctx.session.hook("prompt", (event) => {
      clearTimeout(idle.get(event.sessionID));
      turns.set(event.sessionID, event.messageID);
      void send("prompt", event.sessionID, event);
    });

    // Awaited for `shell` only — OpenCode waits for this hook, and the
    // window over what the command writes must open before it runs.
    await ctx.tool.hook("execute.before", async (event) => {
      if (event.tool === "shell") await send("tool.before", event.sessionID, event);
    });
    await ctx.tool.hook("execute.after", (event) => {
      void send("tool.after", event.sessionID, event);
    });

    // A tool of our own rather than an MCP server: its `execute` receives
    // the exact session, so the flag needs no guessing and no config.
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "thirdeye_flag",
        description:
          "Mark a file the human should look at, with a one-line reason. Use it the moment " +
          "you would otherwise write \"you should look at X\": the file jumps to the top of " +
          "the human's thirdeye viewer with your reason and the lines.",
        input: {
          type: "object",
          properties: {
            path: { type: "string", description: "The file, relative to the project or absolute." },
            reason: { type: "string", description: "One line: why the human should look." },
            start_line: { type: "integer", description: "First line worth reading." },
            end_line: { type: "integer", description: "Last line worth reading, inclusive." },
          },
          required: ["path", "reason"],
          additionalProperties: false,
        },
        async execute(input, context) {
          if (!context?.sessionID) return { content: "thirdeye could not tell which session this is." };
          // `--name=value`: a reason like "-1 is off by one" must not read as a flag.
          const args = ["flag", "--agent=opencode", `--session=${context.sessionID}`,
            `--path=${input.path}`, `--reason=${input.reason}`];
          if (Number.isInteger(input.start_line)) args.push(`--start-line=${input.start_line}`);
          if (Number.isInteger(input.end_line)) args.push(`--end-line=${input.end_line}`);
          const said = (await run(args, "", cwd, FLAG_MS)).trim();
          return { content: said || "thirdeye could not record the flag." };
        },
      });
    });

    const controller = new AbortController();
    void (async () => {
      for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
        const session = event.data?.sessionID;
        if (event.type === "session.execution.succeeded" || event.type === "session.execution.failed") {
          void send("turn.end", session, event);
          clearTimeout(idle.get(session));
          const timer = setTimeout(() => void end(session), IDLE_MS);
          timer.unref?.();
          idle.set(session, timer);
        } else if (event.type === "location.shutdown") {
          // No per-session end exists; the server stopping ends them all —
          // at once, since the process may not outlive a sequential loop.
          await Promise.all([...sessions].map(end));
        }
      }
    })().catch(() => {});
    return () => controller.abort();
  },
};
