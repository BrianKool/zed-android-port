import readline from "node:readline";

const endpoint = process.env.ZDROID_PHONE_USE_ENDPOINT ?? "http://127.0.0.1:8765/mcp";
const token = process.env.ZDROID_PHONE_USE_TOKEN;
const requestTimeoutMs = 30_000;
let sessionId;
let queue = Promise.resolve();

function fail(message) {
  process.stderr.write(`Zdroid-B Phone Use: ${message}\n`);
}

async function forward(line) {
  let payload;
  try {
    payload = JSON.parse(line);
  } catch (error) {
    fail(`invalid MCP message: ${error.message}`);
    return;
  }

  if (!token) throw new Error("local MCP token is missing; reopen Zdroid-B and retry");
  const headers = {
    "content-type": "application/json",
    authorization: `Bearer ${token}`,
  };
  if (sessionId) headers["mcp-session-id"] = sessionId;

  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), requestTimeoutMs);
  let response;
  try {
    response = await fetch(endpoint, {
      method: "POST",
      headers,
      body: JSON.stringify(payload),
      signal: controller.signal,
    });
  } finally {
    clearTimeout(timeout);
  }
  const nextSession = response.headers.get("mcp-session-id");
  if (nextSession) sessionId = nextSession;
  const body = await response.text();
  if (!response.ok) {
    throw new Error(`${response.status} ${response.statusText}${body ? `: ${body}` : ""}`);
  }
  if (body.trim()) process.stdout.write(`${body.trim()}\n`);
}

const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
input.on("line", (line) => {
  if (!line.trim()) return;
  queue = queue.then(() => forward(line)).catch((error) => fail(error.message));
});
input.on("close", () => queue.finally(() => process.exit(0)));
