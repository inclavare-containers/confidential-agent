#!/usr/bin/env node
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";

const work = fs.mkdtempSync(path.join(os.tmpdir(), "cai-a2a-budget-test-"));
const preload = path.join(work, "preload.mjs");
fs.writeFileSync(preload, String.raw`
import fs from "node:fs";
import { setTimeout as schedule, clearTimeout as cancel } from "node:timers";
const scenario = process.env.A2A_BUDGET_SCENARIO;
const stats = { calls: [], timers: [], now_ms: 0 };
const active = new Map();
Date.now = () => stats.now_ms;
globalThis.setTimeout = (callback, milliseconds) => {
  const deadline = stats.now_ms + milliseconds;
  stats.timers.push(milliseconds);
  const handle = schedule(() => {
    if (!active.has(handle)) return;
    active.delete(handle);
    stats.now_ms = Math.max(stats.now_ms, deadline);
    callback();
  }, 0);
  active.set(handle, { callback, deadline });
  return handle;
};
globalThis.clearTimeout = handle => { active.delete(handle); cancel(handle); };
function advance(milliseconds) {
  stats.now_ms += milliseconds;
  for (const [handle, entry] of active) {
    if (entry.deadline > stats.now_ms) continue;
    active.delete(handle); cancel(handle); entry.callback();
  }
}
function reply(value, status = 200) {
  return new Response(JSON.stringify(value), { status, headers: { "content-type": "application/json" } });
}
function sensitiveError(options) {
  return new Error(options.headers.authorization + " " + options.body);
}
function hang(options) {
  return new Promise((_, reject) => {
    const aborted = () => reject(sensitiveError(options));
    if (options.signal.aborted) aborted();
    else options.signal.addEventListener("abort", aborted, { once: true });
  });
}
globalThis.fetch = async (url, options) => {
  const pathname = new URL(url).pathname;
  const body = JSON.parse(options.body);
  stats.calls.push(pathname);
  const call = stats.calls.length;
  if (scenario === "long-success" && call === 1) {
    advance(150000);
    if (options.signal.aborted) throw sensitiveError(options);
  }
  if (scenario === "body-read-retry" && call === 1) {
    return { ok: true, status: 200, async text() {
      advance(500);
      throw sensitiveError(options);
    } };
  }
  if (scenario === "backoff-deadline") {
    if (call !== 1) throw sensitiveError(options);
    advance(400);
    return reply({ payload: "BUDGET_TEST_PAYLOAD", authorization: "BUDGET_TEST_BEARER" }, 503);
  }
  if (scenario === "shrinking-native-retry" && call === 2) {
    advance(500);
    return reply({ payload: "BUDGET_TEST_PAYLOAD" }, 503);
  }
  if (scenario === "shrinking-native-retry" && call === 3) return hang(options);
  if (scenario === "final-timeout" && call === 3) return hang(options);
  if (pathname === "/tools/invoke") {
    return reply({ ok: true, result: { content: [{ type: "text", text: JSON.stringify({ text: "BUDGET_TEST_PAYLOAD" }) }] } });
  }
  if (body.previous_response_id) {
    return reply({ output: [{ type: "message", content: [{ type: "output_text", text: "BUDGET_TEST_PAYLOAD" }] }] });
  }
  return reply({ id: "first-response", output: [{ type: "function_call", name: "e2e_a2a_chat",
    call_id: "call-a2a", arguments: JSON.stringify({ peer: "peer-beta", message: "BUDGET_TEST_PAYLOAD" }) }] });
};
process.on("exit", () => fs.writeFileSync(process.env.A2A_BUDGET_STATS, JSON.stringify(stats)));
`);

function run(scenario, budget) {
  const statsPath = path.join(work, `${scenario}-${budget}.json`);
  const child = spawnSync(process.execPath, [
    "--import", pathToFileURL(preload).href,
    new URL("../probes/openclaw-a2a-responses-probe.mjs", import.meta.url).pathname,
    "--url", "http://127.0.0.1:8123/?private=BUDGET_TEST_PAYLOAD",
    "--token", "BUDGET_TEST_BEARER", "--peer", "peer-beta",
    "--message", "BUDGET_TEST_PAYLOAD", "--expect", "BUDGET_TEST_PAYLOAD",
    "--timeout-ms", String(budget),
  ], { env: { ...process.env, A2A_BUDGET_SCENARIO: scenario, A2A_BUDGET_STATS: statsPath },
    encoding: "utf8", timeout: 5000 });
  assert.equal(child.error, undefined, child.error?.message);
  const stats = JSON.parse(fs.readFileSync(statsPath, "utf8"));
  return { ...child, stats };
}
function timedOut(result, stage, pathname, budget) {
  assert.equal(result.status, 1, result.stderr);
  assert.equal(result.stdout, "");
  assert.match(result.stderr, new RegExp(`${stage}: ${pathname} timed out`));
  assert.ok(result.stderr.includes(`budget_ms=${budget}`));
  assert.ok(result.stderr.includes(`elapsed_ms=${budget}`));
  assert.equal(result.stderr.includes("BUDGET_TEST_BEARER"), false);
  assert.equal(result.stderr.includes("BUDGET_TEST_PAYLOAD"), false);
  assert.equal(result.stderr.includes("127.0.0.1"), false);
}
try {
  const long = run("long-success", 240000);
  assert.equal(long.status, 0, long.stderr);
  assert.equal(JSON.parse(long.stdout).ok, true);
  assert.deepEqual(long.stats.calls, ["/v1/responses", "/tools/invoke", "/v1/responses"]);
  assert.deepEqual(long.stats.timers, [240000, 240000, 240000]);
  assert.equal(long.stats.now_ms, 150000, "a request longer than 120s did not complete within its configured 240s budget");

  const bodyRead = run("body-read-retry", 6000);
  assert.equal(bodyRead.status, 0, bodyRead.stderr);
  assert.equal(JSON.parse(bodyRead.stdout).ok, true);
  assert.deepEqual(bodyRead.stats.calls, ["/v1/responses", "/v1/responses", "/tools/invoke", "/v1/responses"]);
  assert.deepEqual(bodyRead.stats.timers, [6000, 2000, 3500, 6000, 6000]);

  const backoff = run("backoff-deadline", 1000);
  timedOut(backoff, "source-function-call", "/v1/responses", 1000);
  assert.equal(backoff.stats.calls.length, 1, "retry started after the request deadline");
  assert.deepEqual(backoff.stats.timers, [1000, 600], "backoff exceeded the remaining budget");

  const shrinking = run("shrinking-native-retry", 6000);
  timedOut(shrinking, "native-a2a-tool", "/tools/invoke", 6000);
  assert.deepEqual(shrinking.stats.calls, ["/v1/responses", "/tools/invoke", "/tools/invoke"]);
  assert.deepEqual(shrinking.stats.timers, [6000, 6000, 2000, 3500], "retry restarted a full request timeout");

  const final = run("final-timeout", 1000);
  timedOut(final, "final-source-response", "/v1/responses", 1000);
  for (const invalid of ["NaN", "0", "600001", "1500.5"]) {
    const result = run("invalid-timeout", invalid);
    assert.equal(result.status, 1);
    assert.match(result.stderr, /--timeout-ms must be an integer between 1000 and 600000/);
    assert.equal(result.stats.calls.length, 0, "invalid timeout reached the gateway");
  }
  console.log("A2A configured >120s budget, bounded backoff/retries, safe stage-specific deadlines and timeout validation: PASS");
} finally {
  fs.rmSync(work, { recursive: true, force: true });
}
