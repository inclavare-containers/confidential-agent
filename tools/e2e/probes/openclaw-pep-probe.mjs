#!/usr/bin/env node
// Run on the newly provisioned debug guest (or a local Docker test machine).
// This starts a private real OpenClaw gateway and PEP, without changing services.
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { spawn, execFileSync } from "node:child_process";

const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
function arg(name, fallback) {
  const index = process.argv.indexOf(`--${name}`);
  return index < 0 ? fallback : process.argv[index + 1];
}
const root = fs.mkdtempSync(path.join(arg("work-dir", os.tmpdir()), "cai-pep-e2e-"));
const workspace = path.join(root, "workspace");
const socketPath = path.join(root, "pep.sock");
const pepBin = arg("pep-bin", "/usr/local/bin/cai-pep");
const openclawBin = arg("openclaw-bin", "openclaw");
const openclawRoot = arg("openclaw-root", "");
const pluginDir = arg("plugin-dir", "/usr/local/share/confidential-agent/openclaw/cai-pep-plugin");
const policyPath = arg("policy", "/etc/cai/pep/policy.json");
const outputPath = arg("output", "");
const runTdx = process.argv.includes("--tdx");
const checks = [];
let isolationEvidence = null;
let pep;
let gateway;
let fake;
let modelServer;
let gatewayLog = "";
let pepLog = "";
fs.mkdirSync(workspace, { recursive: true });

function docker(args) {
  return execFileSync("docker", args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
}
async function until(test, message, timeout = 30000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const result = await test();
    if (result) return result;
    await delay(100);
  }
  throw new Error(message);
}
async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise(resolve => child.once("exit", resolve));
  child.kill("SIGTERM");
  await Promise.race([exited, delay(5000)]);
  if (child.exitCode === null && child.signalCode === null) {
    child.kill("SIGKILL");
    await exited;
  }
}
function auditEvents() {
  return pepLog.split("\n").flatMap(line => {
    try { const event = JSON.parse(line); return event.component === "cai-pep" ? [event] : []; }
    catch { return []; }
  });
}
function runningSandboxes() {
  const ids = docker(["ps", "-aq", "--filter", "label=confidential-agent.cai-pep=true"]).split(/\s+/).filter(Boolean);
  if (!ids.length) return [];
  return JSON.parse(docker(["inspect", ...ids])).filter(item =>
    item.Mounts?.some(mount => mount.Source === workspace));
}
function readSandboxIsolation(sandbox, readStatus = pid => fs.readFileSync(`/proc/${pid}/status`, "utf8")) {
  const limits = sandbox.HostConfig ?? {};
  // Deliberately exclude image environment/config: only isolation evidence is
  // safe and useful in the public success or private failure report.
  const evidence = {
    pid: Number(sandbox.State?.Pid), networkMode: limits.NetworkMode,
    readOnlyRootfs: limits.ReadonlyRootfs, memoryBytes: limits.Memory,
    pidsLimit: limits.PidsLimit, nanoCpus: limits.NanoCpus,
    cpuQuota: limits.CpuQuota, cpuPeriod: limits.CpuPeriod,
    inspectCapDrop: limits.CapDrop, securityOpt: limits.SecurityOpt,
    mounts: (sandbox.Mounts ?? []).map(mount => ({ type: mount.Type,
      source: mount.Source, destination: mount.Destination, writable: mount.RW })),
    kernel: {},
  };
  if (!Number.isSafeInteger(evidence.pid) || evidence.pid <= 0) {
    evidence.kernelReadError = "container has no running init PID";
    return evidence;
  }
  try {
    const status = readStatus(evidence.pid);
    for (const name of ["CapEff", "CapBnd", "NoNewPrivs"]) {
      evidence.kernel[name] = status.match(new RegExp(`^${name}:\\s*(\\S+)`, "m"))?.[1] ?? null;
    }
  } catch (error) {
    evidence.kernelReadError = error.code ?? error.message;
  }
  return evidence;
}
function assertSandboxCapabilities(evidence) {
  assert.equal(evidence.kernelReadError, undefined, "could not read the running container init capability masks");
  for (const name of ["CapEff", "CapBnd"]) {
    assert.match(evidence.kernel[name] ?? "", /^0+$/, `running container init ${name} must be an all-zero mask`);
  }
  assert.equal(evidence.kernel.NoNewPrivs, "1", "running container init NoNewPrivs must be 1");
}
async function startPep(config) {
  fs.rmSync(socketPath, { force: true });
  const configPath = path.join(root, "policy.json");
  fs.writeFileSync(configPath, JSON.stringify(config));
  pep = spawn(pepBin, ["serve", "--config", configPath], { stdio: ["ignore", "ignore", "pipe"] });
  pep.stderr.on("data", chunk => { pepLog += chunk; });
  await until(() => fs.existsSync(socketPath), "private PEP failed to listen");
}

async function main() {
  const policy = JSON.parse(fs.readFileSync(policyPath, "utf8"));
  const image = arg("image", policy.docker_image);
  docker(["image", "inspect", image]);
  const config = { ...policy, socket_path: socketPath, docker_image: image,
    workspace_host_path: workspace, workspace_mount_target: "/workspace",
    allowed_workspace_prefixes: [workspace], timeout_secs: 2,
    stdout_max_bytes: 256, stderr_max_bytes: 128, memory_mb: 128,
    cpus: 0.5, pids_limit: 32, docker_network_mode: "none" };
  await startPep(config);

  const portServer = net.createServer();
  await new Promise(resolve => portServer.listen(0, "127.0.0.1", resolve));
  const port = portServer.address().port;
  await new Promise(resolve => portServer.close(resolve));
  const token = crypto.randomBytes(32).toString("hex");
  // The pinned gateway intentionally excludes exec from HTTP tools/invoke.
  // Drive the actual built-in agent tool through /v1/responses instead. This
  // model is deterministic; the separate Bailian probe proves a real model too.
  modelServer = http.createServer(async (request, response) => {
    let raw = "";
    for await (const chunk of request) raw += chunk;
    const body = JSON.parse(raw);
    const messages = body.messages ?? [];
    const serialized = JSON.stringify(messages);
    const matches = [...serialized.matchAll(/CA_PEP_CASE:([A-Za-z0-9+/=]+)/g)];
    const selected = matches.at(-1)?.[1];
    assert.ok(selected, "private model received no deterministic test command");
    const args = JSON.parse(Buffer.from(selected, "base64").toString());
    const toolReply = messages.at(-1)?.role === "tool";
    const delta = toolReply ? { role: "assistant", content: "CA_E2E_TOOL_DONE" } : {
      role: "assistant", tool_calls: [{ index: 0, id: `call-${crypto.randomUUID()}`,
        type: "function", function: { name: "exec", arguments: JSON.stringify(args) } }],
    };
    response.writeHead(200, { "content-type": "text/event-stream" });
    const chunk = (delta, finish_reason) => `data: ${JSON.stringify({ id: "chatcmpl-pep-e2e",
      object: "chat.completion.chunk", created: Math.floor(Date.now() / 1000), model: "pep-probe",
      choices: [{ index: 0, delta, finish_reason }] })}\n\n`;
    response.end(chunk(delta, null) + chunk({}, toolReply ? "stop" : "tool_calls") + "data: [DONE]\n\n");
  });
  await new Promise(resolve => modelServer.listen(0, "127.0.0.1", resolve));
  const modelPort = modelServer.address().port;
  const approvalPlugin = path.join(root, "approval-plugin");
  fs.mkdirSync(approvalPlugin);
  fs.writeFileSync(path.join(approvalPlugin, "package.json"), JSON.stringify({ name: "ca-e2e-approval", version: "1.0.0",
    type: "module", openclaw: { extensions: ["./index.js"] } }));
  fs.writeFileSync(path.join(approvalPlugin, "openclaw.plugin.json"), JSON.stringify({ id: "ca-e2e-approval", activation: { onStartup: true, onCapabilities: ["hook"] },
    configSchema: { type: "object", properties: {}, additionalProperties: false } }));
  const approvalObserved = path.join(root, "approval-observed");
  const observedResults = path.join(root, "observed-results.jsonl");
  fs.writeFileSync(path.join(approvalPlugin, "index.js"), `import fs from "node:fs";
    export default { id: "ca-e2e-approval", register(api) {
      api.on("before_tool_call", event => {
        if (event.toolName !== "exec") return {};
        const symbol = Symbol.for("confidential-agent.cai-pep.resolve");
        const resolve = globalThis[symbol];
        if (resolve && !resolve.e2eTap) {
          const tap = async (ticket, params) => {
            const result = await resolve(ticket, params);
            fs.appendFileSync(${JSON.stringify(observedResults)}, JSON.stringify({ command: params.command, result }) + "\\n");
            return result;
          };
          tap.e2eTap = true; globalThis[symbol] = tap;
        }
        if (event.params.command.includes("later-veto")) return { block: true, blockReason: "E2E later veto" };
        if (event.params.command.includes("approval-pending")) {
          fs.writeFileSync(${JSON.stringify(approvalObserved)}, "observed");
          return { requireApproval: { pluginId: "ca-e2e-approval", title: "E2E approval gate",
            description: "E2E approval is required", timeoutMs: 1500, allowedDecisions: ["deny"] } };
        }
        return {};
      }, { priority: -1000 });
    } };`);
  const gatewayConfig = {
    gateway: { mode: "local", bind: "loopback", port, auth: { mode: "token", token },
      http: { endpoints: { responses: { enabled: true } } }, controlUi: { enabled: false } },
    models: { providers: { "pep-probe": { baseUrl: `http://127.0.0.1:${modelPort}/v1`,
      apiKey: "private-e2e-model", api: "openai-completions", models: [{ id: "probe", name: "probe",
        reasoning: false, input: ["text"], contextWindow: 262144, maxTokens: 4096 }] } } },
    agents: { defaults: { workspace, model: { primary: "pep-probe/probe" } } },
    tools: { profile: "full", exec: { security: "full", ask: "off" } },
    plugins: { enabled: true, allow: ["cai-pep", "ca-e2e-approval"], load: { paths: [pluginDir, approvalPlugin] },
      entries: { "cai-pep": { enabled: true, config: { socketPath, pepRequired: true,
        defaultWorkdir: workspace, responseTimeoutMs: runTdx ? 240000 : 5000 } },
        "ca-e2e-approval": { enabled: true } } },
  };
  const configPath = path.join(root, "openclaw.json");
  fs.writeFileSync(configPath, JSON.stringify(gatewayConfig), { mode: 0o600 });
  const cliArgs = ["gateway", "run", "--port", String(port), "--allow-unconfigured"];
  gateway = spawn(openclawRoot ? process.execPath : openclawBin,
    openclawRoot ? [path.join(openclawRoot, "openclaw.mjs"), ...cliArgs] : cliArgs, {
      env: { ...process.env, OPENCLAW_STATE_DIR: path.join(root, "state"),
        OPENCLAW_CONFIG_PATH: configPath, OPENCLAW_GATEWAY_TOKEN: token,
        PATH: `${path.dirname(process.execPath)}:${process.env.PATH}`,
        OPENCLAW_NO_RESPAWN: "1", OPENCLAW_SKIP_CHANNELS: "1" }, stdio: ["ignore", "pipe", "pipe"],
    });
  gateway.stdout.on("data", chunk => { gatewayLog += chunk; });
  gateway.stderr.on("data", chunk => { gatewayLog += chunk; });
  const base = `http://127.0.0.1:${port}`;
  await until(async () => {
    if (gateway.exitCode !== null) throw new Error(`private OpenClaw gateway exited: ${gatewayLog.slice(-6000)}`);
    try { return (await fetch(`${base}/health`, { signal: AbortSignal.timeout(1000) })).ok; }
    catch { return false; }
  }, "private OpenClaw gateway failed to become ready", 90000);
  async function invoke(name, command, workdir = workspace) {
    const before = fs.existsSync(observedResults) ? fs.readFileSync(observedResults, "utf8").split("\n").filter(Boolean).length : 0;
    const response = await fetch(`${base}/v1/responses`, {
      method: "POST", headers: { authorization: `Bearer ${token}`, "content-type": "application/json",
        "x-openclaw-session-key": `agent:main:ca-pep-${name}` },
      body: JSON.stringify({ model: "openclaw/default", stream: false, max_output_tokens: 4096,
        input: `Execute exactly one exec call. CA_PEP_CASE:${Buffer.from(JSON.stringify({ command, workdir })).toString("base64")}` }),
      signal: AbortSignal.timeout(runTdx && name === "tdx" ? 240000 : 45000),
    });
    const body = await response.json();
    if (!response.ok) throw new Error(`private agent request failed (${response.status}): ${JSON.stringify(body)}; ${gatewayLog.slice(-4000)}`);
    const records = fs.existsSync(observedResults) ? fs.readFileSync(observedResults, "utf8").split("\n").filter(Boolean).slice(before).map(JSON.parse) : [];
    assert.ok(records.length <= 1, `${name}: more than one real PEP tool execution`);
    return records.length ? { status: 200, ok: true, result: records[0].result } : { status: 403, ok: false };
  }
  function allowed(reply) {
    assert.equal(reply.ok, true, JSON.stringify(reply));
    assert.equal(reply.status, 200);
    assert.match(reply.result.auditId, /^audit-/);
    assert.ok(auditEvents().some(event => event.audit_id === reply.result.auditId && event.event === "intent_allow"),
      "tool result audit ID has no matching real PEP journal event");
    return reply.result;
  }
  async function denied(name, command, workdir, expectedRule) {
    const before = auditEvents().length;
    const reply = await invoke(name, command, workdir);
    assert.equal(reply.ok, false, JSON.stringify(reply));
    assert.equal(fs.existsSync(path.join(workspace, name)), false, `${name} had a host/sandbox side effect`);
    if (expectedRule) {
      const events = auditEvents().slice(before);
      assert.ok(events.some(event => event.event === "intent_error" && event.rule_id === expectedRule),
        `${name}: expected policy rule ${expectedRule}, observed ${JSON.stringify(events)}`);
      assert.ok(events.every(event => event.event !== "intent_allow"));
    }
    checks.push(name);
  }

  const first = allowed(await invoke("allow", "printf x >> executions; printf PEP_STDOUT; printf PEP_STDERR >&2; exit 7"));
  assert.equal(first.stdout, "PEP_STDOUT");
  assert.equal(first.stderr, "PEP_STDERR");
  assert.equal(first.exitCode, 7);
  assert.equal(first.isError, true);
  assert.equal(first.backend, "docker");
  assert.equal(fs.readFileSync(path.join(workspace, "executions"), "utf8"), "x", "exec was not exactly once");
  assert.equal(runningSandboxes().length, 0);
  checks.push("gateway-plugin-live-pep-real-container-exactly-once", "stdout-stderr-exit-audit-propagation");

  const beforeApproval = auditEvents().length;
  await denied("later-veto", "printf x > later-veto");
  assert.equal(auditEvents().length, beforeApproval, "a later veto ran PEP before rejection");
  // Native approval handling may immediately deny because no approval route is
  // connected. This marker is not evidence of an interactive pending request.
  const approvalCall = invoke("approval-pending", "printf x > approval-pending");
  await until(() => fs.existsSync(approvalObserved), "upstream approval hook was never called");
  assert.equal(fs.existsSync(path.join(workspace, "approval-pending")), false, "exec ran before upstream approval");
  assert.equal(auditEvents().length, beforeApproval, "PEP received an unapproved intent");
  const approvalReply = await approvalCall;
  assert.equal(approvalReply.ok, false, "unapproved call unexpectedly succeeded");
  assert.equal(fs.existsSync(path.join(workspace, "approval-pending")), false);
  assert.equal(auditEvents().length, beforeApproval, "approval timeout/rejection ran PEP");
  checks.push("upstream-approval-required-or-unavailable-denial-has-no-pep-or-native-side-effects");

  await denied("deny-command", "printf x > deny-command; curl http://example.invalid", workspace, "command.deny_pattern");
  for (const [label, prefix] of [["etc", "/etc"], ["proc", "/proc"], ["sys", "/sys"], ["dev", "/dev"],
    ["root", "/root"], ["run", "/run"], ["var-run", "/var/run"], ["openclaw-config", "/home/openclaw/.openclaw"]]) {
    const name = `deny-sensitive-${label}`;
    await denied(name, `printf x > ${name}; cat ${prefix}/ca-e2e-nonexistent`, workspace, "fs.deny_prefix");
  }
  const outside = path.join(root, "outside"); fs.mkdirSync(outside);
  await denied("deny-workdir", "printf x > deny-workdir", outside, "fs.workdir_prefix");
  assert.equal(fs.existsSync(path.join(outside, "deny-workdir")), false);
  fs.symlinkSync(outside, path.join(workspace, "escape"));
  await denied("deny-symlink", "printf x > deny-symlink", path.join(workspace, "escape"), "fs.workdir_prefix");
  assert.equal(fs.existsSync(path.join(outside, "deny-symlink")), false);
  await denied("deny-missing-workdir", "printf x > deny-missing-workdir", path.join(workspace, "missing"), "fs.invalid_workdir");

  const inspectedCall = invoke("isolation", "sleep 1; pwd");
  const sandbox = await until(() => runningSandboxes()[0], "sandbox never became observable");
  isolationEvidence = readSandboxIsolation(sandbox);
  const limits = sandbox.HostConfig;
  assert.equal(limits.NetworkMode, "none");
  assert.equal(limits.ReadonlyRootfs, true);
  assert.equal(limits.Memory, 128 * 1024 * 1024);
  assert.equal(limits.PidsLimit, 32);
  assert.ok(limits.NanoCpus === 500000000 || limits.CpuQuota / limits.CpuPeriod === 0.5);
  // Docker and Podman report CapDrop/EffectiveCaps differently. The running
  // process's effective and bounding masks establish actual enforcement.
  assertSandboxCapabilities(isolationEvidence);
  assert.ok(limits.SecurityOpt.some(option => option.startsWith("no-new-privileges")));
  assert.deepEqual(sandbox.Mounts.filter(mount => mount.Type === "bind").map(mount => [mount.Source, mount.Destination]),
    [[workspace, "/workspace"]]);
  assert.equal(allowed(await inspectedCall).stdout.trim(), "/workspace");
  const readOnlyName = `cai-pep-readonly-${crypto.randomUUID()}`;
  const readOnly = allowed(await invoke("read-only", `p=/; if printf x > "\${p}${readOnlyName}" 2>&1; then exit 99; fi; printf ROOT_READ_ONLY`));
  assert.equal(readOnly.exitCode, 0);
  assert.ok(readOnly.stdout.includes("ROOT_READ_ONLY"));
  checks.push("live-container-network-none-readonly-workspace-only-mount-caps-no-new-privileges-resource-limits");

  const capped = allowed(await invoke("output-cap", 'i=0; while [ "$i" -lt 512 ]; do printf x; printf y >&2; i=$((i+1)); done'));
  assert.equal(capped.stdout, "x".repeat(256) + "\n[truncated by cai-pep]");
  assert.equal(capped.stderr, "y".repeat(128) + "\n[truncated by cai-pep]");
  const timeoutStarted = Date.now();
  const timed = allowed(await invoke("timeout", "sleep 4; printf escaped > timeout-late"));
  assert.notEqual(timed.exitCode, 0);
  assert.ok(timed.stderr.includes("[timed out by cai-pep]"));
  assert.ok(Date.now() - timeoutStarted < 6000, "timeout waited for surviving container");
  assert.equal(runningSandboxes().length, 0, "timeout leaked a real sandbox");
  await delay(2200);
  assert.equal(fs.existsSync(path.join(workspace, "timeout-late")), false, "timed-out sandbox executed late side effect");
  checks.push("output-limits", "timeout-kills-container-and-prevents-late-side-effects", "success-and-timeout-cleanup");

  if (runTdx) {
    await stop(pep);
    await startPep({ ...config, stdout_max_bytes: 1024 * 1024, stderr_max_bytes: 65536 });
    const tdx = allowed(await invoke("tdx", "cai-pep attest collect-and-verify --claims"));
    assert.equal(tdx.backend, "host-attestation");
    assert.equal(tdx.exitCode, 0, `live TDX helper failed to verify evidence: ${tdx.stderr.slice(-1200)}`);
    const tokens = tdx.stdout.match(/eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/g) ?? [];
    const claims = tokens.flatMap(token => {
      try { return [JSON.parse(Buffer.from(token.split(".")[1], "base64url").toString())]; }
      catch { return []; }
    });
    assert.ok(claims.some(value => {
      const cpu = value.submods?.cpu0;
      const hardware = cpu?.["ear.trustworthiness-vector"]?.hardware;
      return typeof hardware === "number" && hardware <= 32 &&
        /mr_td|rtmr_[0-3]|measurement.uki/.test(JSON.stringify(cpu?.["ear.veraison.annotated-evidence"] ?? {}));
    }), "TDX helper returned no trusted CPU and measurement claims");
    checks.push("live-tdx-host-attestation-helper-and-measurement-claims");
  }

  await stop(pep);
  await denied("pep-stopped", "printf x > pep-stopped");
  for (const [name, payload] of [["malformed-json", "not-json"], ["malformed-schema", JSON.stringify({ result: {} })]]) {
    fs.rmSync(socketPath, { force: true });
    fake = net.createServer({ allowHalfOpen: true }, socket => {
      socket.on("data", () => {}); socket.on("end", () => socket.end(payload));
    });
    await new Promise(resolve => fake.listen(socketPath, resolve));
    await denied(name, `printf x > ${name}`);
    await new Promise(resolve => fake.close(resolve)); fake = undefined;
  }
  if (!runTdx) {
  fs.rmSync(socketPath, { force: true });
  const hungSockets = new Set();
  fake = net.createServer({ allowHalfOpen: true }, socket => {
    hungSockets.add(socket); socket.on("data", () => {}); socket.on("close", () => hungSockets.delete(socket));
  });
  await new Promise(resolve => fake.listen(socketPath, resolve));
  await denied("pep-unresponsive", "printf x > pep-unresponsive");
  for (const socket of hungSockets) socket.destroy();
  await new Promise(resolve => fake.close(resolve)); fake = undefined;
  }
  assert.equal(runningSandboxes().length, 0);
  const report = { ok: true, scope: "exec only", transport: "real OpenClaw agent via HTTP v1/responses and deterministic model", checks,
    isolationEvidence, auditCount: auditEvents().length, tdx: runTdx ? "executed" : "not requested" };
  if (outputPath) fs.writeFileSync(outputPath, JSON.stringify(report, null, 2) + "\n");
  console.log(JSON.stringify(report, null, 2));
}

try { await main(); }
catch (error) {
  // Keep diagnostics in the private artifact; never print config or auth tokens.
  const report = { ok: false, checks, isolationEvidence, error: error.message };
  if (outputPath) {
    fs.writeFileSync(outputPath, JSON.stringify(report, null, 2) + "\n");
    fs.writeFileSync(`${outputPath}.gateway.log`, gatewayLog, { mode: 0o600 });
    fs.writeFileSync(`${outputPath}.pep.log`, pepLog, { mode: 0o600 });
  }
  console.error(`OpenClaw PEP probe failed: ${error.message}`);
  process.exitCode = 1;
} finally {
  if (fake) await new Promise(resolve => fake.close(resolve));
  await stop(gateway);
  await stop(pep);
  if (modelServer) await new Promise(resolve => modelServer.close(resolve));
  try { for (const sandbox of runningSandboxes()) docker(["rm", "-f", sandbox.Id]); } catch {}
  if (process.argv.includes("--keep-work-dir")) console.error(`private probe artifacts: ${root}`);
  else fs.rmSync(root, { recursive: true, force: true });
}
