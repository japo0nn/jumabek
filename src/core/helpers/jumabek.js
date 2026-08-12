/**
 * The JumaBek skill protocol, for skills written for Node.
 *
 * One JSON line arrives on stdin, one JSON line goes back on stdout. That is
 * the whole contract, and the only rule that matters: **stdout belongs to the
 * protocol**. A stray `console.log` in the middle of a response corrupts the
 * line the core is parsing, so `run()` quietly repoints `console.log` at
 * stderr and writes the protocol through a private handle. Log whatever you
 * like; it lands in the log, not in the protocol.
 *
 *     const jumabek = require("./jumabek");
 *
 *     jumabek.run({
 *       name: "greeter",
 *       version: "0.1.0",
 *       description: "Says hello to whoever is named in the arguments",
 *       methods: [{
 *         method: "greet",
 *         description: "Greet someone by name",
 *         args_description: "the name to greet, as plain text",
 *       }],
 *       async execute(method, args) {
 *         if (method === "greet") return `hello, ${args}`;
 *         throw new jumabek.SkillError(`no such method: ${method}`, "NotFound");
 *       },
 *     });
 *
 * `args` is always a string — the core does not interpret it. If your method
 * takes structured input, agree on JSON and parse it yourself.
 */

"use strict";

const readline = require("readline");

/**
 * The error kinds the core understands. Anything else is sent as
 * ExecutionFailed, because an unknown kind would fail to deserialise and cost
 * the caller a turn.
 */
const KINDS = [
  "NotFound",
  "ExecutionFailed",
  "InvalidArgs",
  "Recoverable",
  "Fatal",
];

/**
 * Throw this to answer with an error instead of dying. A skill that crashes
 * gets restarted and the caller learns nothing; a skill that answers with an
 * error tells the model what to do differently.
 */
class SkillError extends Error {
  constructor(message, kind = "ExecutionFailed") {
    super(message);
    this.name = "SkillError";
    this.kind = KINDS.includes(kind) ? kind : "ExecutionFailed";
  }
}

/** Answer with plain text. */
const text = (value) => ({ Text: String(value) });

/** Answer with structured data the model can pick apart. */
const jsonOutput = (value) => ({ Json: value });

/** Answer with nothing — the call worked and has no result. */
const empty = () => "Empty";

/**
 * Turn whatever a handler returned into an Output payload. Returning a bare
 * string or an object is the common case and is meant to just work; the
 * explicit helpers are for when the guess would be wrong.
 */
function output(result) {
  if (result === undefined || result === null) return { Output: "Empty" };
  if (result === "Empty") return { Output: "Empty" };

  if (typeof result === "object" && !Array.isArray(result)) {
    const keys = Object.keys(result);
    if (keys.length === 1 && ["Text", "Json", "Binary"].includes(keys[0])) {
      return { Output: result };
    }
  }

  if (Buffer.isBuffer(result)) return { Output: { Binary: Array.from(result) } };
  if (typeof result === "object") return { Output: { Json: result } };
  return { Output: { Text: String(result) } };
}

async function runExecute(params, execute) {
  if (params === undefined || params === null) {
    return { Error: { InvalidArgs: "Not provided any parameters" } };
  }

  let method;
  let args;
  try {
    const parsed = typeof params === "string" ? JSON.parse(params) : params;
    method = parsed.method;
    args = parsed.args || "";
    if (typeof method !== "string") throw new Error("no method in params");
  } catch (error) {
    return { Error: { InvalidArgs: String(error.message || error) } };
  }

  try {
    return output(await execute(method, args));
  } catch (error) {
    if (error instanceof SkillError) {
      return { Error: { [error.kind]: error.message } };
    }
    return {
      Error: { ExecutionFailed: `${error.name || "Error"}: ${error.message}` },
    };
  }
}

/**
 * Serve the protocol until stdin closes.
 *
 * `methods` is a list of objects with `method`, `description` and
 * `args_description`. The validator refuses a skill that leaves any of them
 * empty, so describe them as if the reader has never seen the skill.
 */
function run({
  name,
  version,
  description,
  methods,
  execute,
  healthCheck = null,
}) {
  const protocol = process.stdout;
  console.log = console.error;

  const metadata = { name, version, description };
  const answer = (id, payload) => {
    protocol.write(`${JSON.stringify({ id, payload })}\n`);
  };

  const lines = readline.createInterface({ input: process.stdin });

  // Requests are answered strictly in order: the core matches responses by id
  // and refuses one that arrives before the request it belongs to.
  let queue = Promise.resolve();

  lines.on("line", (line) => {
    const trimmed = line.trim();
    if (!trimmed) return;

    queue = queue.then(async () => {
      let request;
      try {
        request = JSON.parse(trimmed);
      } catch (error) {
        answer(0, { Error: { InvalidArgs: String(error.message || error) } });
        return;
      }

      const id = request.id || 0;
      let payload;

      switch (request.method) {
        case "get_metadata":
          payload = { Metadata: metadata };
          break;
        case "available_methods":
          payload = { Methods: methods };
          break;
        case "health_check":
          payload = { Health: healthCheck ? Boolean(healthCheck()) : true };
          break;
        case "execute":
          payload = await runExecute(request.params, execute);
          break;
        default:
          payload = { Error: { NotFound: "Method not found" } };
      }

      answer(id, payload);
    });
  });
}

module.exports = { run, SkillError, text, jsonOutput, empty };
