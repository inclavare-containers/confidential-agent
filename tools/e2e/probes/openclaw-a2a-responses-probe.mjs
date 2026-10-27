#!/usr/bin/env node
import crypto from "node:crypto";

function arg(name, fallback = undefined) {
  const idx = process.argv.indexOf(`--${name}`);
  if (idx >= 0 && idx + 1 < process.argv.length) return process.argv[idx + 1];
  return fallback;
}

function requireArg(name) {
  const value = arg(name);
  if (!value) {
    console.error(`missing --${name}`);
    process.exit(2);
  }
  return value;
}

function normalizeBaseUrl(raw) {
  const replaced = raw.replace(/^ws:/, "http:").replace(/^wss:/, "https:");
  const url = new URL(replaced);
  url.pathname = url.pathname.replace(/\/+$/, "");
  url.search = "";
  url.hash = "";
  return url;
}

async function postJson(baseUrl, pathname, token, body, timeoutMs, stage) {
  const url = new URL(pathname, baseUrl);
  const retryableStatuses = new Set([401, 404, 502, 503]);
  const startedAt = Date.now();
  const deadline = startedAt + timeoutMs;
  // Only operator-safe context belongs in a timeout diagnostic. Fetch errors
  // and response bodies can contain request payloads or authentication data.
  const timedOut = () => new Error(
    `${stage}: ${pathname} timed out (budget_ms=${timeoutMs}, elapsed_ms=${Date.now() - startedAt})`,
  );
  const retry = async () => {
    const remaining = deadline - Date.now();
    if (remaining <= 0) throw timedOut();
    await new Promise(resolve => setTimeout(resolve, Math.min(2000, remaining)));
  };

  while (true) {
    const remaining = deadline - Date.now();
    if (remaining <= 0) throw timedOut();
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), remaining);
    let res;
    let text;
    try {
      res = await fetch(url, {
        method: "POST",
        headers: {
          authorization: `Bearer ${token}`,
          "content-type": "application/json",
        },
        body: JSON.stringify(body),
        signal: controller.signal,
      });
      text = await res.text();
    } catch {
      if (controller.signal.aborted || Date.now() >= deadline) throw timedOut();
      res = undefined;
    } finally {
      clearTimeout(timer);
    }
    if (Date.now() >= deadline) throw timedOut();
    if (!res) {
      await retry();
      continue;
    }

    let parsed;
    try {
      parsed = text ? JSON.parse(text) : {};
    } catch {
      if (!retryableStatuses.has(res.status)) {
        throw new Error(`${stage}: ${pathname} returned non-JSON HTTP ${res.status}: ${text}`);
      }
      await retry();
      continue;
    }
    if (res.ok) return parsed;
    if (!retryableStatuses.has(res.status)) {
      throw new Error(`${stage}: ${pathname} returned HTTP ${res.status}: ${JSON.stringify(parsed)}`);
    }
    await retry();
  }
}

function outputText(response) {
  return (response?.output ?? [])
    .flatMap((item) => (Array.isArray(item?.content) ? item.content : []))
    .map((part) => (typeof part?.text === "string" ? part.text : ""))
    .filter(Boolean)
    .join("\n")
    .trim();
}

function firstFunctionCall(response, name) {
  return (response?.output ?? []).find((item) => item?.type === "function_call" && item?.name === name);
}

function toolResultText(result) {
  const content = result?.content;
  if (!Array.isArray(content)) return "";
  return content
    .map((part) => (part?.type === "text" && typeof part.text === "string" ? part.text : ""))
    .filter(Boolean)
    .join("\n")
    .trim();
}

function parseToolResult(result) {
  const text = toolResultText(result);
  if (!text) throw new Error("tools.invoke result text is empty");
  try {
    return JSON.parse(text);
  } catch {
    return { text };
  }
}

const A2aChatSchema = {
  type: "object",
  additionalProperties: false,
  required: ["peer", "message"],
  properties: {
    peer: {
      type: "string",
      description: "Configured peer id to ask through the confidential A2A channel.",
    },
    message: {
      type: "string",
      description: "Message to send to the peer OpenClaw agent.",
    },
    timeout_ms: {
      type: "number",
      description: "Optional timeout in milliseconds.",
      minimum: 1000,
      maximum: 600000,
    },
  },
};

// Keep the client-side function distinct from the gateway's native a2a_chat
// tool. Recent OpenClaw versions execute a same-named native tool internally,
// which cannot satisfy the Responses API's required client function call.
const ClientA2aFunction = "e2e_a2a_chat";

const baseUrl = normalizeBaseUrl(requireArg("url"));
const token = requireArg("token");
const peer = requireArg("peer");
const expected = requireArg("expect");
const peerMessage = arg("message", `请只回复 ${expected}，不要输出其他内容。`);
const timeoutMs = Number(arg("timeout-ms", "180000"));
const sessionKey = arg("session", `confidential-agent-a2a-responses-${Date.now()}`);

async function main() {
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1000 || timeoutMs > 600000) {
    throw new Error("--timeout-ms must be an integer between 1000 and 600000");
  }
  const responseId = `resp_${crypto.randomUUID()}`;
  const first = await postJson(
    baseUrl,
    "/v1/responses",
    token,
    {
      model: "openclaw/default",
      input: [
        {
          type: "message",
          role: "user",
          content: [
            {
              type: "input_text",
              text: [
                `Call the ${ClientA2aFunction} function exactly once.`,
                `peer must be ${JSON.stringify(peer)}.`,
                `message must be ${JSON.stringify(peerMessage)}.`,
                "Do not answer directly before calling the function.",
              ].join("\n"),
            },
          ],
        },
      ],
      instructions:
        "You are the source OpenClaw agent in a confidential A2A E2E probe. You must use the requested function call and must not answer directly.",
      tools: [
        {
          type: "function",
          name: ClientA2aFunction,
          description:
            "Send a message to a configured peer OpenClaw agent over the confidential A2A channel and return the peer's final reply.",
          parameters: A2aChatSchema,
        },
      ],
      tool_choice: { type: "function", name: ClientA2aFunction },
      max_output_tokens: 2048,
      stream: false,
    },
    timeoutMs,
    "source-function-call",
  );

  const call = firstFunctionCall(first, ClientA2aFunction);
  if (!call) {
    throw new Error(`OpenResponses did not return a ${ClientA2aFunction} function call: ${JSON.stringify(first)}`);
  }
  const callArgs = JSON.parse(call.arguments || "{}");
  if (callArgs.peer !== peer) {
    throw new Error(`a2a_chat peer mismatch: expected ${peer}, got ${JSON.stringify(callArgs.peer)}`);
  }
  if (typeof callArgs.message !== "string" || !callArgs.message.includes(expected)) {
    throw new Error(`a2a_chat message does not contain expected marker ${expected}`);
  }
  callArgs.timeout_ms = Number(callArgs.timeout_ms ?? timeoutMs);

  const invoked = await postJson(
    baseUrl,
    "/tools/invoke",
    token,
    {
      sessionKey,
      tool: "a2a_chat",
      args: callArgs,
    },
    timeoutMs,
    "native-a2a-tool",
  );
  if (invoked?.ok !== true) {
    throw new Error(`tools.invoke failed: ${JSON.stringify(invoked)}`);
  }
  const peerResult = parseToolResult(invoked.result);
  const peerText = String(peerResult.text ?? "");
  if (!peerText.includes(expected)) {
    throw new Error(`peer OpenClaw reply does not include expected marker ${expected}: ${peerText}`);
  }

  const final = await postJson(
    baseUrl,
    "/v1/responses",
    token,
    {
      model: "openclaw/default",
      previous_response_id: first.id ?? responseId,
      input: [
        {
          type: "function_call_output",
          call_id: call.call_id,
          output: JSON.stringify(peerResult),
        },
      ],
      instructions:
        "Return only the peer response text from the function output. Do not add explanation and do not call any tools.",
      max_output_tokens: 256,
      stream: false,
    },
    timeoutMs,
    "final-source-response",
  );
  const finalText = outputText(final);
  if (!finalText.includes(expected)) {
    throw new Error(`final OpenClaw response does not include expected marker ${expected}: ${finalText}`);
  }

  console.log(
    JSON.stringify(
      {
        ok: true,
        sessionKey,
        firstResponseId: first.id,
        functionCall: {
          name: call.name,
          callId: call.call_id,
          arguments: callArgs,
        },
        peerResult,
        finalText,
      },
      null,
      2,
    ),
  );
}

main()
  .then(() => process.exit(0))
  .catch((error) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  });
