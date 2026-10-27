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
  const replaced = String(raw).trim().replace(/^ws:/i, "http:").replace(/^wss:/i, "https:");
  const url = new URL(replaced);
  url.pathname = url.pathname.replace(/\/+$/, "");
  url.search = "";
  url.hash = "";
  return url;
}

async function postJson(baseUrl, pathname, token, body, timeoutMs, sessionKey) {
  const url = new URL(pathname, baseUrl);
  const retryableStatuses = new Set([401, 404, 502, 503]);
  const retryDeadline = Date.now() + timeoutMs;
  let lastError;

  while (true) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    let res;
    let text;
    try {
      res = await fetch(url, {
        method: "POST",
        headers: {
          authorization: `Bearer ${token}`,
          "content-type": "application/json",
          ...(sessionKey ? { "x-openclaw-session-key": sessionKey } : {}),
        },
        body: JSON.stringify(body),
        signal: controller.signal,
      });
      text = await res.text();
    } catch (error) {
      lastError = error;
      if (Date.now() >= retryDeadline) throw lastError;
    } finally {
      clearTimeout(timer);
    }

    if (!res) {
      await new Promise((resolve) => setTimeout(resolve, 2000));
      continue;
    }

    let parsed;
    try {
      parsed = text ? JSON.parse(text) : {};
    } catch {
      lastError = new Error(`${pathname} returned non-JSON HTTP ${res.status}: ${text}`);
      if (!retryableStatuses.has(res.status) || Date.now() >= retryDeadline) throw lastError;
      await new Promise((resolve) => setTimeout(resolve, 2000));
      continue;
    }

    if (res.ok) return parsed;

    lastError = new Error(`${pathname} returned HTTP ${res.status}: ${JSON.stringify(parsed)}`);
    if (!retryableStatuses.has(res.status) || Date.now() >= retryDeadline) throw lastError;
    await new Promise((resolve) => setTimeout(resolve, 2000));
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

function collectObservedToolPhases(response) {
  const phases = [];
  const seen = new Set();

  function add(value) {
    const text = String(value ?? "").trim();
    if (!text || seen.has(text)) return;
    seen.add(text);
    phases.push(text);
  }

  function visit(value) {
    if (!value || typeof value !== "object") return;
    if (Array.isArray(value)) {
      for (const item of value) visit(item);
      return;
    }

    const type = typeof value.type === "string" ? value.type : "";
    const name = typeof value.name === "string" ? value.name : "";
    const toolName =
      typeof value.tool_name === "string"
        ? value.tool_name
        : typeof value.toolName === "string"
          ? value.toolName
          : "";
    const skillId =
      typeof value.skill_id === "string"
        ? value.skill_id
        : typeof value.skillId === "string"
          ? value.skillId
          : "";
    const command = typeof value.command === "string" ? value.command : "";

    if (/tool|function|call|exec|skill/i.test(type)) add(type);
    if (name && /tool|function|exec|cai-pep|attest|skill/i.test(`${type} ${name}`)) add(name);
    if (toolName) add(toolName);
    if (skillId) add(skillId);
    if (command && /cai-pep\s+attest|collect-and-verify/i.test(command)) add(command);

    for (const child of Object.values(value)) visit(child);
  }

  visit(response?.output ?? response);
  return phases;
}

const baseUrl = normalizeBaseUrl(requireArg("url"));
const token = requireArg("token");
const message = arg("message", "请只回复 CA_E2E_OK，不要输出其他内容。");
const expected = arg("expect", "CA_E2E_OK");
const expectedTool = arg("expect-tool", "");
const expectedRegex = arg("expect-regex", "");
const rejectedRegex = arg("reject-regex", "");
const instructions = arg("instructions", "Reply to the user message directly.");
const sessionKey = arg("session", `confidential-agent-e2e-${Date.now()}`);
const timeoutMs = Number(arg("timeout-ms", "180000"));
const requestTimeoutMs = timeoutMs;

async function main() {
  const body = {
    model: "openclaw/default",
    input: [
      {
        type: "message",
        role: "user",
        content: [{ type: "input_text", text: message }],
      },
    ],
    max_output_tokens: 2048,
    stream: false,
  };
  if (instructions) body.instructions = instructions;
  const response = await postJson(
    baseUrl,
    "/v1/responses",
    token,
    body,
    requestTimeoutMs,
    sessionKey,
  );
  const text = outputText(response);
  if (!text) throw new Error("OpenClaw response text is empty");
  if (expected && !text.includes(expected)) {
    throw new Error(`OpenClaw response does not include expected marker '${expected}': ${text}`);
  }
  if (expectedRegex && !new RegExp(expectedRegex, "i").test(text)) {
    throw new Error(`OpenClaw response does not match expected regex '${expectedRegex}': ${text}`);
  }
  if (rejectedRegex && new RegExp(rejectedRegex, "i").test(text)) {
    throw new Error(`OpenClaw response matched rejected regex '${rejectedRegex}': ${text}`);
  }
  const observedToolPhases = collectObservedToolPhases(response);
  if (
    expectedTool &&
    !observedToolPhases.some((phase) => phase.toLowerCase().includes(expectedTool.toLowerCase()))
  ) {
    throw new Error(
      `OpenClaw response did not expose expected tool '${expectedTool}'. observedToolPhases=${JSON.stringify(observedToolPhases)}`,
    );
  }
  console.log(
    JSON.stringify(
      {
        ok: true,
        runId: response.id ?? `resp_${crypto.randomUUID()}`,
        sessionKey,
        text,
        observedToolPhases,
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
