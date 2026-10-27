#!/usr/bin/env node
// Exercise the actual pinned OpenClaw modules, copied to a private test tree.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import net from "node:net";
import plugin from "../../../examples/openclaw/files/cai-pep-plugin/index.js";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { functionSource, patchOpenclaw } from "../../../examples/openclaw/files/patch-openclaw-cai-pep.js";

const installed = process.argv[2] || path.join(execFileSync("npm", ["root", "-g"], { encoding: "utf8" }).trim(), "openclaw");
const work = fs.mkdtempSync(path.join(os.tmpdir(), "cai-pep-runtime-test-"));
const root = path.join(work, "openclaw");
const originals = new Map();
function copyModules(directory, relative = "") {
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const name = path.join(relative, entry.name);
    const source = path.join(directory, entry.name);
    if (entry.isDirectory()) copyModules(source, name);
    else if (entry.isFile() && /\.[cm]?js$/.test(name)) {
      const text = fs.readFileSync(source, "utf8");
      if (!text.includes("runBeforeToolCall")) continue;
      const target = path.join(root, "dist", name);
      fs.mkdirSync(path.dirname(target), { recursive: true });
      fs.writeFileSync(target, text);
      originals.set(target, text);
    }
  }
}
function loadFunction(source, name, environment) {
  return new Function("environment", `with (environment) { return (${functionSource(source, name).text}); }`)(environment);
}
const noop = () => {};
try {
  copyModules(path.join(installed, "dist"));
  patchOpenclaw(root);
  const patched = new Map([...originals.keys()].map(file => [file, fs.readFileSync(file, "utf8")]));
  patchOpenclaw(root);
  for (const [file, text] of patched) assert.equal(fs.readFileSync(file, "utf8"), text, "patch is not idempotent");
  const find = marker => [...patched.values()].find(text => text.includes(marker));
  const hookSource = find("function createHookRunner(");
  const runtimeSource = find("function wrapToolWithBeforeToolCallHook(");
  const adapterSource = find("function toToolDefinitions(");
  assert.ok(hookSource && runtimeSource && adapterSource);
  const sandboxResult = { content: [{ type: "text", text: "PEP_SANDBOX_RESULT" }] };
  let laterHookCalls = 0;
  let hookResult = sandboxResult;
  const hooks = {
    registry: {}, logger: undefined, modifyingHookTimeoutMsByHook: {},
    getHooksForName: () => [
      { pluginId: "pep", handler: async () => ({ result: hookResult }) },
      { pluginId: "later", handler: async () => { laterHookCalls++; return {}; } },
    ],
    readClaimingHookAdmission: noop,
    cloneHookIsolationValue: (_, value) => structuredClone(value),
    awaitHook: (_, promise) => promise,
    HookIsolationError: class extends Error {},
    shouldCatchHookErrors: () => false,
    handleHookError: ({ error }) => { throw error; },
    stickyTrue: (a, b) => a === true || b === true,
    recordBeforeToolCallDecision: noop,
  };
  hooks.runModifyingHook = loadFunction(hookSource, "runModifyingHook", hooks);
  const before = loadFunction(hookSource, "runBeforeToolCall", hooks);
  const merged = await before({ toolName: "exec", params: {} }, {});
  assert.deepEqual(merged.result, sandboxResult);
  assert.equal(laterHookCalls, 0, "an ordinary immediate result stopped being terminal");
  hookResult = { caiPepDeferredId: "test-ticket" };
  assert.deepEqual((await before({ toolName: "exec", params: {} }, {})).result, hookResult);
  assert.equal(laterHookCalls, 1, "a PEP ticket skipped later approval or veto hooks");

  // Run the real outcome construction, including the upstream approval gates.
  const hookBody = functionSource(runtimeSource, "runBeforeToolCallHook").text;
  const start = hookBody.indexOf("let finalParams = policyAdjustedParams;");
  const end = hookBody.indexOf("return withLoopWarning(allowed);") + "return withLoopWarning(allowed);".length;
  assert.ok(start >= 0 && end > start);
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
  const deliver = new AsyncFunction("environment", `with (environment) { ${hookBody.slice(start, end)} }`);
  const transport = result => ({
    policyAdjustedParams: {}, trustedApprovalResolution: undefined, hasBeforeToolCallHooks: true,
    policyAdjustedToolIdentity: {}, policyAdjustedDerivedToolParams: {}, policyAdjustedToolContext: {},
    toolName: "exec", args: {}, hookRunner: { runBeforeToolCall: async () => result },
    isPlainObject: value => value !== null && typeof value === "object",
    getGatewayToolCallerIdentity: noop,
    resolveBeforeToolCallApprovalOutcome: async () => ({ blocked: true, reason: "approval required" }),
    resolveSkillWorkshopApprovalForFinalParams: async () => undefined,
    withLoopWarning: value => value,
    markPrivateDecision: (value, key) => { value[key] = true; },
    mergeParamsWithApprovalOverrides: (base, changes) => ({ ...base, ...changes }),
    reconcileCodeModeExecBeforeHookParams: ({ adjustedParams }) => adjustedParams,
  });
  assert.deepEqual((await deliver(transport(merged))).result, sandboxResult);
  assert.equal((await deliver(transport({ block: true, result: sandboxResult }))).blocked, true);
  assert.equal((await deliver(transport({ requireApproval: {}, result: sandboxResult }))).blocked, true);
  const finalGate = transport(merged);
  finalGate.resolveSkillWorkshopApprovalForFinalParams = async () => ({ blocked: true });
  assert.equal((await deliver(finalGate)).blocked, true, "PEP result bypassed the final approval gate");

  const resolveApproved = async environment => {
    const outcome = await deliver(environment);
    if (outcome.result?.caiPepDeferredId) outcome.result = await globalThis[Symbol.for("confidential-agent.cai-pep.resolve")](outcome.result, outcome.params);
    return outcome;
  };
  // Exercise the real plugin and socket transport. Approval rejection/pending
  // must prevent even connecting to PEP, not merely suppress native execution.
  const socketPath = path.join(work, "pep.sock");
  let submissions = 0;
  let reply = request => ({ id: request.id, result: { status: "ok", decision: "allow",
    stdout: "SANDBOX", stderr: "stderr", exit_code: 7, audit_id: "audit-test",
    backend: "docker", sandbox_profile: "default-docker", duration_ms: 1 } });
  const server = net.createServer({ allowHalfOpen: true }, socket => {
    let raw = "";
    socket.on("data", chunk => { raw += chunk; });
    socket.on("end", () => {
      submissions++;
      const response = reply(JSON.parse(raw));
      socket.end(typeof response === "string" ? response : JSON.stringify(response));
    });
  });
  await new Promise(resolve => server.listen(socketPath, resolve));
  let handler;
  plugin.register({ config: {}, pluginConfig: { socketPath, pepRequired: true },
    logger: { info: noop, error: noop }, on: (_, value) => { handler = value; } });
  const params = { command: "printf SANDBOX", workdir: work };
  const ticket = () => handler({ toolName: "exec", params }, { runId: "run-test", sessionKey: "session-test" });
  try {
    const deferred = await ticket();
    assert.equal(submissions, 0, "before_tool_call submitted intent before approval");
    assert.ok(deferred.result.caiPepDeferredId);
    const rejected = transport({ ...deferred, requireApproval: {} });
    rejected.policyAdjustedParams = params;
    assert.equal((await deliver(rejected)).blocked, true);
    assert.equal(submissions, 0, "rejected approval executed PEP");
    const pending = transport({ ...await ticket(), requireApproval: {} });
    pending.resolveBeforeToolCallApprovalOutcome = async () => ({ blocked: false, deferredApproval: {} });
    assert.ok((await deliver(pending)).deferredApproval);
    assert.equal(submissions, 0, "pending approval executed PEP");
    const finalRejected = transport(await ticket());
    finalRejected.resolveSkillWorkshopApprovalForFinalParams = async () => ({ blocked: true });
    assert.equal((await deliver(finalRejected)).blocked, true);
    assert.equal(submissions, 0, "final approval rejection executed PEP");
    const approved = transport(deferred);
    approved.policyAdjustedParams = params;
    const deferredApproved = await deliver(approved);
    assert.equal(submissions, 0, "approval preparation eagerly submitted PEP");
    const result = (await resolveApproved(approved)).result;
    assert.equal(submissions, 1, "approved intent did not execute exactly once");
    assert.equal(result.stdout, "SANDBOX");
    assert.equal(result.stderr, "stderr");
    assert.equal(result.exitCode, 7);
    assert.equal(result.auditId, "audit-test");
    assert.equal(result.isError, true);
    assert.equal(result.content[0].text, "SANDBOX\nstderr");
    await assert.rejects(() => resolveApproved(approved), /ticket expired/);
    assert.equal(submissions, 1, "replayed ticket executed again");
    for (const invalid of ["not-json", {}, { result: {} }, { id: "wrong", result: { status: "ok" } }]) {
      reply = () => invalid;
      const environment = transport(await ticket()); environment.policyAdjustedParams = params;
      await assert.rejects(() => resolveApproved(environment), /invalid|no result/);
    }
  } finally {
    await new Promise(resolve => server.close(resolve));
  }
  const unavailable = transport(await ticket()); unavailable.policyAdjustedParams = params;
  await assert.rejects(() => resolveApproved(unavailable), /unavailable/);

  let nativeCalls = 0;
  let outcome = await deliver(transport(merged));
  const recorded = [];
  let launch = true;
  let voiceAllowed = true;
  let admissionAllowed = true;
  let validationAllowed = true;
  const runtime = {
    caiPepResolveAdmittedResult: loadFunction(runtimeSource, "caiPepResolveAdmittedResult", {}),
    INTERNAL_DISPOSED_RESULT: { content: [], details: { status: "skipped" } },
    finalizeBeforeToolCallExecutionParams: ({ adjustedParams }) => ({ ...adjustedParams, finalized: true }),
    validateToolExecutionParams: async () => { if (!validationAllowed) throw new Error("invalid execution params"); },
    reconcileLoopCallExecutionParams: noop,
    consumeFinalClientVoiceToolConfirmation: () => ({ allowed: voiceAllowed, reason: "voice denied" }),
    runAgentToolSourceExecutionGuard: noop,
    recordAdjustedParamsForToolCall: noop, captureDiagnosticToolProgress: noop,
    recordToolExecutionStarted: noop,
    startToolExecutionLiveness: () => ({ run: run => run(), close: noop }),
    runWithGenericToolActionDecision: (_, __, run) => run(),
    prepareToolTerminalPresentation: noop, rememberPendingTerminalPresentation: noop,
    resolveToolResultTerminalDiagnostic: () => ({ type: "tool.execution.completed" }),
    findSkillUsageMatch: noop, isTrustedToolExecutionPreflightError: () => false,
    recordPreExecutionBlockedToolCall: noop, recordGenericToolActionDecision: noop,
    tagBeforeToolCallFailure: error => error,
    captureAgentPluginRuntimeRefresh: () => ({ assertActive: noop, assertCurrent: noop }),
    isCodeModeControlTool: () => false, getPluginToolMeta: noop,
    captureAgentToolExecutionBudget: () => () => { if (!admissionAllowed) throw new Error("admission denied"); }, resolveToolDiagnosticIdentity: () => ({}),
    resolveDiagnosticModelContentCapturePolicy: () => ({}),
    normalizeToolPolicyName: value => value,
    readInternalExecutionControl: value => value?.pause ? value : undefined,
    prepareBeforeToolCallExecutionParams: async ({ params }) => params,
    normalizeCodeModeExecBeforeHookParams: ({ params }) => params,
    getCodeModeExecBeforeHookMetadata: noop,
    runBeforeToolCallHook: async () => outcome,
    recordLoopOutcome: async value => recorded.push(value),
    recordToolExecutionTracked: noop, clearTrackedToolExecution: noop,
    createInternalExecutionPreparer: run => async args => ({ kind: "immediate",
      outcome: { kind: "result", result: await run(args, { pause: async () => ({ launch }) }) }, dispose: noop }),
    attachInternalToolExecutionPreparer: noop, copyBeforeToolCallWrapperMetadata: noop,
    bindBeforeToolCallMetadata: noop,
    summarizeToolParams: () => ({}), buildToolMutationState: () => ({ mutatingAction: true }),
    buildBlockedToolResult: ({ reason }) => ({ isError: true, reason }),
  };
  const tool = { name: "exec", execute: async () => { nativeCalls++; return "HOST_EXECUTED"; } };
  const wrapped = loadFunction(runtimeSource, "wrapToolWithBeforeToolCallHook", runtime)(tool, undefined, { emitDiagnostics: false });
  assert.deepEqual(await wrapped.execute("call", {}), sandboxResult);
  assert.equal(nativeCalls, 0, "PEP result fell through to native execution");
  assert.deepEqual(recorded.at(-1).result, sandboxResult);
  outcome = { blocked: true, kind: "veto", reason: "PEP denied" };
  assert.equal((await wrapped.execute("denied", {})).isError, true);
  assert.equal(nativeCalls, 0, "PEP denial fell through to native execution");

  // The ticket must survive preparation untouched until the actual launch.
  const resolverSymbol = Symbol.for("confidential-agent.cai-pep.resolve");
  const realResolver = globalThis[resolverSymbol];
  let sandboxCalls = 0;
  let resolvedParams;
  globalThis[resolverSymbol] = async (_, params) => { sandboxCalls++; resolvedParams = params; return sandboxResult; };
  outcome = { blocked: false, params: { command: "updated-by-hook" }, result: { caiPepDeferredId: "test-ticket" } };
  launch = false;
  assert.equal((await wrapped.execute("suppressed", {})).details.status, "skipped");
  assert.equal(sandboxCalls, 0, "scheduler launch=false executed PEP");
  launch = true; voiceAllowed = false;
  assert.equal((await wrapped.execute("voice-denied", {})).isError, true);
  assert.equal(sandboxCalls, 0, "final voice rejection executed PEP");
  voiceAllowed = true; admissionAllowed = false;
  await assert.rejects(() => wrapped.execute("admission-denied", {}), /admission denied/);
  assert.equal(sandboxCalls, 0, "execution admission rejection executed PEP");
  admissionAllowed = true; validationAllowed = false;
  await assert.rejects(() => wrapped.execute("validation-denied", {}), /invalid execution params/);
  assert.equal(sandboxCalls, 0, "parameter validation rejection executed PEP");
  validationAllowed = true;
  const aborted = AbortSignal.abort(new Error("cancelled"));
  await assert.rejects(() => wrapped.execute("aborted", {}, aborted), /cancelled/);
  assert.equal(sandboxCalls, 0, "cancelled execution submitted PEP");
  assert.deepEqual(await wrapped.execute("admitted", {}), sandboxResult);
  assert.equal(sandboxCalls, 1);
  assert.equal(resolvedParams.command, "updated-by-hook");
  assert.equal(resolvedParams.finalized, true, "PEP did not receive finalized execution params");
  assert.equal(nativeCalls, 0);

  const adapter = {
    ...runtime, getInternalToolExecutionPreparer: noop, isToolWrappedWithBeforeToolCallHook: () => false,
    projectAgentToolDefinition: value => value, copyCodeModeControlToolIdentity: noop,
    attachAdapterExecutionPreparer: value => value, recordStructuredReplayTrustForToolCall: noop,
    executeAdaptedToolOperation: ({ run }) => run(),
    finalizeBeforeToolCallExecutionParams: ({ adjustedParams }) => adjustedParams,
    resolveAbortSignal: value => value,
    consumeFinalClientVoiceToolConfirmation: () => ({ allowed: voiceAllowed, reason: "voice denied" }),
    recordAdjustedParamsForToolCall: noop,
  };
  const adapted = loadFunction(adapterSource, "toToolDefinitions", adapter)([tool])[0];
  outcome = { blocked: false, params: {}, result: sandboxResult };
  assert.deepEqual(await adapted.execute("replacement", {}), sandboxResult);
  assert.equal(nativeCalls, 0);
  outcome = { blocked: false, params: { command: "adapter-final" }, result: { caiPepDeferredId: "adapter-ticket" } };
  const beforeAdapter = sandboxCalls;
  voiceAllowed = false;
  assert.equal((await adapted.execute("adapter-voice-denied", {})).isError, true);
  assert.equal(sandboxCalls, beforeAdapter);
  voiceAllowed = true;
  const suppressed = await adapted.execute("adapter-suppressed", {}, undefined, undefined, { pause: async () => ({ launch: false }) });
  assert.equal(suppressed.details.status, "skipped");
  assert.equal(sandboxCalls, beforeAdapter, "adapter scheduler rejection executed PEP");
  await assert.rejects(() => adapted.execute("adapter-cancelled", {}, aborted), /cancelled/);
  assert.equal(sandboxCalls, beforeAdapter);
  assert.deepEqual(await adapted.execute("adapter-admitted", {}), sandboxResult);
  assert.equal(sandboxCalls, beforeAdapter + 1);
  assert.equal(resolvedParams.command, "adapter-final");
  globalThis[resolverSymbol] = realResolver;
  const lateAbort = new AbortController();
  adapter.runBeforeToolCallHook = async () => {
    lateAbort.abort(new Error("late cancellation"));
    return outcome;
  };
  await assert.rejects(() => adapted.execute("adapter-late-cancelled", {}, lateAbort.signal), /late cancellation/);
  assert.equal(sandboxCalls, beforeAdapter + 1, "adapter abort during before_tool_call executed PEP");
  adapter.runBeforeToolCallHook = async () => outcome;
  outcome = { blocked: false, params: {} };
  assert.equal(await adapted.execute("ordinary", {}), "HOST_EXECUTED");
  assert.equal(nativeCalls, 1, "an ordinary, unintercepted tool stopped executing");

  globalThis[resolverSymbol] = async () => undefined;
  outcome = { blocked: false, params: {}, result: { caiPepDeferredId: "optional-unavailable" } };
  assert.equal(await adapted.execute("optional-pep-fallback", {}), "HOST_EXECUTED");
  assert.equal(nativeCalls, 2, "explicit optional PEP stopped falling back at the admitted native invocation");
  globalThis[resolverSymbol] = realResolver;

  // An upstream layout change must fail before any installed module changes.
  for (const [file, text] of originals) fs.writeFileSync(file, text);
  const runtimeFile = [...originals].find(([, text]) => text.includes("function wrapToolWithBeforeToolCallHook("))[0];
  fs.writeFileSync(runtimeFile, originals.get(runtimeFile).replace("let ownerDecisionMarked = false;", "let changedUpstreamLayout = false;"));
  assert.throws(() => patchOpenclaw(root), /expected one patch location/);
  const hookFile = [...originals].find(([, text]) => text.includes("function createHookRunner("))[0];
  assert.equal(fs.readFileSync(hookFile, "utf8"), originals.get(hookFile), "failure left a partial hook patch");
  console.log("Pinned OpenClaw PEP propagation, deferred approval safety, real plugin socket protocol, fail-closed validation, native suppression and atomic patching: PASS");
} finally {
  fs.rmSync(work, { recursive: true, force: true });
}
