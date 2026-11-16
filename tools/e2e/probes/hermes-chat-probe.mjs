#!/usr/bin/env node

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
  const url = new URL(String(raw).trim());
  url.pathname = url.pathname.replace(/\/+$/, "");
  url.search = "";
  url.hash = "";
  return url;
}

async function requestJson(baseUrl, pathname, token, options, timeoutMs, perAttemptTimeoutMs = 30000) {
  const url = new URL(pathname, baseUrl);
  const deadline = Date.now() + timeoutMs;
  let lastError;

  while (Date.now() < deadline) {
    const controller = new AbortController();
    const remainingMs = Math.max(1, deadline - Date.now());
    const timer = setTimeout(() => controller.abort(), Math.min(remainingMs, perAttemptTimeoutMs));
    try {
      const headers = { ...(options.headers ?? {}) };
      if (token) headers.authorization = `Bearer ${token}`;
      const res = await fetch(url, { ...options, headers, signal: controller.signal });
      const text = await res.text();
      let parsed = {};
      if (text) {
        try {
          parsed = JSON.parse(text);
        } catch {
          throw new Error(`${pathname} returned non-JSON HTTP ${res.status}: ${text}`);
        }
      }
      if (res.ok) return parsed;
      lastError = new Error(`${pathname} returned HTTP ${res.status}: ${JSON.stringify(parsed)}`);
    } catch (error) {
      lastError = error;
    } finally {
      clearTimeout(timer);
    }
    await new Promise((resolve) => setTimeout(resolve, 3000));
  }

  throw lastError ?? new Error(`${pathname} timed out`);
}

async function requestExpectedChat(baseUrl, token, body, expected, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  let attempts = 0;

  while (Date.now() < deadline) {
    attempts += 1;
    const remainingMs = Math.max(1, deadline - Date.now());
    try {
      const chat = await requestJson(
        baseUrl,
        "/v1/chat/completions",
        token,
        {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(body),
        },
        Math.min(remainingMs, 120000),
        Math.min(remainingMs, 120000),
      );
      const text = chatText(chat);
      if (text.includes(expected)) return { chat, text, attempts };
      lastError = new Error(`Hermes response does not include expected marker '${expected}': ${text}`);
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 3000));
  }

  throw lastError ?? new Error("/v1/chat/completions timed out");
}

function chatText(response) {
  const choice = response?.choices?.[0] ?? {};
  const content = choice?.message?.content ?? choice?.delta?.content ?? "";
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content
      .map((part) => (typeof part?.text === "string" ? part.text : ""))
      .filter(Boolean)
      .join("\n");
  }
  return "";
}

const baseUrl = normalizeBaseUrl(requireArg("url"));
const token = requireArg("token");
const model = arg("model", "qwen4-max");
const message = arg("message", "Reply with CA_E2E_OK and no other text.");
const expected = arg("expect", "CA_E2E_OK");
const timeoutMs = Number(arg("timeout-ms", "180000"));

async function main() {
  const health = await requestJson(baseUrl, "/health", "", { method: "GET" }, timeoutMs);
  const models = await requestJson(baseUrl, "/v1/models", token, { method: "GET" }, timeoutMs);
  const { text, attempts } = await requestExpectedChat(
    baseUrl,
    token,
    {
      model,
      messages: [
        { role: "system", content: "Return the requested marker exactly." },
        { role: "user", content: message },
      ],
      max_tokens: 64,
      stream: false,
    },
    expected,
    timeoutMs,
  );
  console.log(
    JSON.stringify(
      {
        ok: true,
        health,
        modelCount: Array.isArray(models?.data) ? models.data.length : null,
        attempts,
        text,
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
