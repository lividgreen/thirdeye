// thirdeye's pi extension — installed by `thirdeye install pi`.
//
// It forwards what pi does, unchanged, to `thirdeye hook --agent pi`, adding
// only the session, the directory and a turn id — pi has none, so one is
// minted per prompt. It interprets nothing; thirdeye's pi adapter does. And it
// never throws into pi or holds it up: a broken viewer must not break the
// agent.
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";

const THIRDEYE = {{THIRDEYE}};

/** Ms a hook may hold pi up for. A stuck thirdeye costs an event. */
const HOOK_MS = 2000;
/** Ms a flag may take — the model is waiting on it, not the human. */
const FLAG_MS = 5000;

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

const sessionOf = (ctx) => {
  try {
    return ctx?.sessionManager?.getSessionId?.();
  } catch {
    return undefined;
  }
};

export default function (pi) {
  let turn;

  const send = (hook, ctx, event) => {
    let envelope;
    try {
      envelope = JSON.stringify({ hook, session: sessionOf(ctx), turn, cwd: ctx?.cwd, event });
    } catch {
      return Promise.resolve(""); // An event that will not serialize costs only itself.
    }
    return run(["hook", "--agent", "pi"], envelope, ctx?.cwd);
  };

  pi.on("session_start", () => {
    turn = undefined;
  });

  // Not awaited: turns are grouped by id, so nothing needs this to land
  // before the model starts. Only the prompt is forwarded — the system
  // prompt riding on this event is large and of no use to thirdeye.
  pi.on("before_agent_start", (event, ctx) => {
    turn = randomUUID();
    void send("prompt", ctx, { prompt: event.prompt });
  });

  // Awaited for `bash` only — pi waits for this handler, and the window over
  // what the command writes must open before it runs.
  pi.on("tool_call", async (event, ctx) => {
    if (event.toolName === "bash") await send("tool.before", ctx, event);
  });
  pi.on("tool_result", (event, ctx) => {
    void send("tool.after", ctx, event);
  });

  pi.on("agent_settled", (event, ctx) => {
    void send("turn.end", ctx, { aborted: event.aborted });
  });

  // Awaited, briefly: pi may exit right after. Not on `reload`: that
  // rebuilds the extension while the same session carries on, and ending it
  // would stop the daemon and close the viewer mid-session.
  pi.on("session_shutdown", async (event, ctx) => {
    if (event.reason === "reload") return;
    await send("session.end", ctx, { reason: event.reason });
  });

  // A tool of our own: its context carries the exact session.
  pi.registerTool({
    name: "thirdeye_flag",
    label: "thirdeye flag",
    description:
      "Mark a file the human should look at, with a one-line reason. Use it the moment " +
      "you would otherwise write \"you should look at X\": the file jumps to the top of " +
      "the human's thirdeye viewer with your reason and the lines.",
    parameters: {
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
    async execute(_toolCallId, params, _signal, _onUpdate, ctx) {
      const session = sessionOf(ctx);
      const text = (t) => ({ content: [{ type: "text", text: t }], details: undefined });
      if (!session) return text("thirdeye could not tell which session this is.");
      // `--name=value`: a reason like "-1 is off by one" must not read as a flag.
      const args = ["flag", "--agent=pi", `--session=${session}`,
        `--path=${params.path}`, `--reason=${params.reason}`];
      if (Number.isInteger(params.start_line)) args.push(`--start-line=${params.start_line}`);
      if (Number.isInteger(params.end_line)) args.push(`--end-line=${params.end_line}`);
      const said = (await run(args, "", ctx?.cwd, FLAG_MS)).trim();
      return text(said || "thirdeye could not record the flag.");
    },
  });
}
