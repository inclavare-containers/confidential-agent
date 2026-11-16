#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import path from "node:path";

function arg(name, fallback = undefined) {
  const idx = process.argv.indexOf(`--${name}`);
  if (idx >= 0 && idx + 1 < process.argv.length) return process.argv[idx + 1];
  return fallback;
}

function loadJson(file) {
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function writeAudit(config, event) {
  const auditPath = config.auditPath || "/var/log/cai-a2a-data-collab.jsonl";
  const line = JSON.stringify({ ts: new Date().toISOString(), role: config.role, ...event }) + "\n";
  fs.mkdirSync(path.dirname(auditPath), { recursive: true });
  fs.appendFileSync(auditPath, line, { mode: 0o640 });
}

function textFromMessage(message) {
  const parts = Array.isArray(message?.parts) ? message.parts : [];
  return parts
    .map((part) => {
      if (typeof part?.text === "string") return part.text;
      if (part?.kind === "text" && typeof part.text === "string") return part.text;
      return "";
    })
    .filter(Boolean)
    .join("\n")
    .trim();
}

async function readBody(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  return Buffer.concat(chunks).toString("utf8");
}

async function postJson(url, headers, body, timeoutMs = 180000) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const res = await fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json", ...headers },
      body: JSON.stringify(body),
      signal: controller.signal,
    });
    const text = await res.text();
    let parsed = {};
    try {
      parsed = text ? JSON.parse(text) : {};
    } catch {
      throw new Error(`${url} returned non-JSON HTTP ${res.status}: ${text}`);
    }
    if (!res.ok) throw new Error(`${url} returned HTTP ${res.status}: ${JSON.stringify(parsed)}`);
    return parsed;
  } catch (error) {
    if (error?.name === "AbortError") {
      throw new Error(`${url} request aborted after ${timeoutMs}ms`);
    }
    throw error;
  } finally {
    clearTimeout(timer);
  }
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

async function callLlm(config, messages, purpose) {
  const baseUrl = String(config.dashscopeBaseUrl || "https://dashscope.aliyuncs.com/compatible-mode/v1").replace(/\/+$/, "");
  const model = String(config.model || "qwen4-max");
  const apiKey = String(config.apiKey || "").trim();
  if (!apiKey) throw new Error("apiKey is required for real LLM inference");
  let response;
  try {
    response = await postJson(
      `${baseUrl}/chat/completions`,
      { authorization: `Bearer ${apiKey}` },
      {
        model,
        messages,
        temperature: 0.1,
        max_tokens: 1200,
        // This example needs concise aggregate summaries, not an unbounded
        // reasoning phase before the non-streaming response becomes available.
        enable_thinking: false,
      },
      Number(config.llmTimeoutMs || 180000),
    );
  } catch (error) {
    throw new Error(`LLM call failed (${purpose}, model ${model}): ${errorMessage(error)}`);
  }
  const content = String(response?.choices?.[0]?.message?.content ?? "").trim();
  if (!content) throw new Error(`LLM response content is empty (${purpose}, model ${model})`);
  writeAudit(config, {
    event: "llm_call",
    purpose,
    model,
    response_id: response.id || "",
    prompt_sha256: crypto
      .createHash("sha256")
      .update(JSON.stringify(messages))
      .digest("hex"),
  });
  return content;
}

function loadPrivateRows(config) {
  const dataPath = config.dataPath || "/etc/cai/private-risk-data.jsonl";
  return fs
    .readFileSync(dataPath, "utf8")
    .split(/\r?\n/)
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}

function aggregatePrivateData(config) {
  const rows = loadPrivateRows(config);
  if (rows.length === 0) {
    return {
      entity: "AlphaCorp",
      record_count: 0,
      average_risk_score: 0,
      high_risk_count: 0,
      high_risk_ratio: 0,
      regions: {},
      incident_categories: {},
    };
  }
  const scores = rows.map((row) => Number(row.risk_score || 0));
  const high = rows.filter((row) => Number(row.risk_score || 0) >= 80);
  const byRegion = {};
  const byCategory = {};
  for (const row of rows) {
    byRegion[row.region] = (byRegion[row.region] || 0) + 1;
    byCategory[row.incident_category] = (byCategory[row.incident_category] || 0) + 1;
  }
  return {
    entity: "AlphaCorp",
    record_count: rows.length,
    average_risk_score: Number((scores.reduce((a, b) => a + b, 0) / scores.length).toFixed(1)),
    high_risk_count: high.length,
    high_risk_ratio: Number((high.length / rows.length).toFixed(2)),
    regions: byRegion,
    incident_categories: byCategory,
  };
}

function parseJsonObject(text) {
  const fenced = text.match(/```(?:json)?\s*([\s\S]*?)```/i);
  const raw = fenced ? fenced[1] : text;
  const start = raw.indexOf("{");
  const end = raw.lastIndexOf("}");
  if (start >= 0 && end > start) {
    try {
      return JSON.parse(raw.slice(start, end + 1));
    } catch {}
  }
  return { summary: raw.trim() };
}

function resolvePeerUrl(config, alias) {
  const directoryPath = config.serviceDirectoryPath || "/etc/cai/service-directory.json";
  const directory = loadJson(directoryPath);
  const service = directory?.services?.[alias];
  const first = Array.isArray(service?.ports) ? service.ports[0] : null;
  const port = Number(first?.port);
  if (!Number.isInteger(port) || port <= 0) {
    throw new Error(`peer '${alias}' is missing from service directory`);
  }
  const address = String(first?.address || "127.0.0.1").trim() || "127.0.0.1";
  return `http://${address}:${port}/a2a`;
}

function taskResult({ taskId, contextId, text, artifactData }) {
  return {
    id: taskId,
    contextId,
    status: {
      state: "completed",
      timestamp: new Date().toISOString(),
      message: {
        role: "agent",
        parts: [{ kind: "text", text }],
      },
    },
    artifacts: [
      {
        artifactId: "aggregate-risk-artifact",
        name: "Aggregate Risk Artifact",
        parts: [
          { kind: "text", text },
          { kind: "data", data: artifactData },
        ],
      },
    ],
  };
}

async function handleDataOwner(config, requestText, contextId) {
  const aggregate = aggregatePrivateData(config);
  const llmText = await callLlm(
    config,
    [
      {
        role: "system",
        content:
          "You are a data owner agent inside a confidential VM. You may use only aggregate tool output. Never reveal raw rows, names, customer identifiers, or order identifiers. Return JSON with keys summary, risk_level, evidence, recommended_actions.",
      },
      {
        role: "user",
        content: JSON.stringify({ request: requestText, aggregate }, null, 2),
      },
    ],
    "data-owner-summarize-aggregate",
  );
  const parsed = parseJsonObject(llmText);
  const text = String(parsed.summary || llmText).trim();
  writeAudit(config, { event: "data_owner_artifact", aggregate });
  return taskResult({
    taskId: `task_${crypto.randomUUID()}`,
    contextId,
    text,
    artifactData: { aggregate, llm: parsed },
  });
}

async function callPeerA2a(config, peerAlias, message) {
  const url = resolvePeerUrl(config, peerAlias);
  const body = {
    jsonrpc: "2.0",
    id: `req_${crypto.randomUUID()}`,
    method: "message/send",
    params: {
      message: {
        role: "user",
        parts: [{ kind: "text", text: message }],
      },
    },
  };
  let response;
  try {
    response = await postJson(url, {}, body, Number(config.peerTimeoutMs || 240000));
  } catch (error) {
    throw new Error(`peer A2A call failed (${peerAlias} at ${url}): ${errorMessage(error)}`);
  }
  if (response?.error) throw new Error(`peer returned JSON-RPC error: ${JSON.stringify(response.error)}`);
  if (!response?.result) throw new Error(`peer returned no result: ${JSON.stringify(response)}`);
  writeAudit(config, { event: "peer_a2a_call", peer: peerAlias, url });
  return response.result;
}

async function handleAnalyst(config, requestText, contextId) {
  const peerAlias = config.peerAlias || "data-owner";
  const delegation = await callLlm(
    config,
    [
      {
        role: "system",
        content:
          "You are an analyst agent. Decide the concise natural-language subtask to send to a remote data owner. Ask only for aggregate statistics and privacy-preserving evidence.",
      },
      { role: "user", content: requestText },
    ],
    "analyst-plan-delegation",
  );
  const peerTask = await callPeerA2a(config, peerAlias, delegation);
  const artifact = peerTask?.artifacts?.[0]?.parts?.find((part) => part.kind === "data")?.data || {};
  const finalText = await callLlm(
    config,
    [
      {
        role: "system",
        content:
          "You are the analyst agent. Produce the final user-facing answer from the data owner's aggregate artifact. Mention that only aggregate data was used. Do not invent or reveal raw records.",
      },
      {
        role: "user",
        content: JSON.stringify({ original_request: requestText, data_owner_artifact: artifact }, null, 2),
      },
    ],
    "analyst-final-synthesis",
  );
  return taskResult({
    taskId: `task_${crypto.randomUUID()}`,
    contextId,
    text: finalText,
    artifactData: {
      peer: peerAlias,
      peer_task_id: peerTask.id,
      data_owner_artifact: artifact,
      llm_synthesis: finalText,
    },
  });
}

async function handleJsonRpc(config, body) {
  const request = JSON.parse(body || "{}");
  if (request.method !== "message/send") {
    return { jsonrpc: "2.0", id: request.id ?? null, error: { code: -32601, message: "method not found" } };
  }
  const requestText = textFromMessage(request?.params?.message);
  if (!requestText) {
    return { jsonrpc: "2.0", id: request.id ?? null, error: { code: -32602, message: "message text is required" } };
  }
  const contextId = request?.params?.message?.contextId || `ctx_${crypto.randomUUID()}`;
  const result =
    config.role === "analyst"
      ? await handleAnalyst(config, requestText, contextId)
      : await handleDataOwner(config, requestText, contextId);
  return { jsonrpc: "2.0", id: request.id ?? null, result };
}

const config = loadJson(arg("config", "/etc/cai/a2a-agent.json"));
const listenHost = config.listenHost || "0.0.0.0";
const listenPort = Number(config.listenPort || 18789);

const server = http.createServer(async (req, res) => {
  try {
    if (req.method === "GET" && (req.url === "/health" || req.url === "/")) {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ ok: true, role: config.role }) + "\n");
      return;
    }
    if (req.method !== "POST" || req.url !== "/a2a") {
      res.writeHead(404, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: "not found" }) + "\n");
      return;
    }
    const response = await handleJsonRpc(config, await readBody(req));
    res.writeHead(response.error ? 400 : 200, { "content-type": "application/json" });
    res.end(JSON.stringify(response) + "\n");
  } catch (error) {
    writeAudit(config, { event: "error", error: error instanceof Error ? error.message : String(error) });
    res.writeHead(500, { "content-type": "application/json" });
    res.end(JSON.stringify({ jsonrpc: "2.0", id: null, error: { code: -32000, message: String(error?.message || error) } }) + "\n");
  }
});

server.listen(listenPort, listenHost, () => {
  console.log(`A2A ${config.role} LLM agent listening on ${listenHost}:${listenPort}`);
});
