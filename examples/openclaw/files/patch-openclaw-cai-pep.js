#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

function walk(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      walk(full, out);
    } else if (entry.isFile() && /\.(?:[cm]?js)$/.test(full)) {
      out.push(full);
    }
  }
  return out;
}

function replaceOnce(source, pattern, replacement, label) {
  const matches = [...source.matchAll(new RegExp(pattern.source, pattern.flags.replace("g", "") + "g"))];
  if (matches.length !== 1) throw new Error(`unsupported OpenClaw ${label}: expected one patch location, found ${matches.length}`);
  return source.replace(pattern, replacement);
}

// Published OpenClaw modules retain named, indented function declarations.
// Scope edits to that declaration so unrelated hooks cannot satisfy a guard.
export function functionSource(source, name) {
  const header = new RegExp(`^([\\t ]*)(?:async )?function ${name}\\(`, "m").exec(source);
  if (!header) throw new Error(`OpenClaw function ${name} is missing`);
  const offset = header.index + header[0].length;
  const end = new RegExp(`^${header[1]}}`, "m").exec(source.slice(offset));
  if (!end) throw new Error(`OpenClaw function ${name} has no closing boundary`);
  return { start: header.index, end: offset + end.index + end[0].length,
    text: source.slice(header.index, offset + end.index + end[0].length) };
}

function editFunction(source, name, edit) {
  const part = functionSource(source, name);
  return source.slice(0, part.start) + edit(part.text) + source.slice(part.end);
}

export function patchHookRunner(source) {
  // A deferred exec result must not skip later veto or approval hooks.
  if (source.includes("blockReason: next.blockReason ?? acc?.blockReason")) {
    return editFunction(source, "runBeforeToolCall", (body) => {
      if (!body.includes("result: next.result !== undefined ? next.result : acc?.result")) {
        body = replaceOnce(body, /blockReason: next\.blockReason \?\? acc\?\.blockReason,/,
          "$&\n\t\t\t\t\tresult: next.result !== undefined ? next.result : acc?.result,", "hook result merge");
      }
      return body.replace(/shouldStop: \(result\) => [^\n]+,/,
        "shouldStop: (result) => result.block === true || (result.result !== undefined && !result.result?.caiPepDeferredId),")
        .replace('terminalLabel: "block=true or result!=undefined"', 'terminalLabel: "block=true or immediate result"');
    });
  }
  let next = source;
  if (!next.includes("result: lastDefined(acc?.result, next.result)")) {
    next = next.replace(/blockReason:\s*lastDefined\(acc\?\.blockReason,\s*next\.blockReason\),/m,
      "blockReason: lastDefined(acc?.blockReason, next.blockReason),\n        result: lastDefined(acc?.result, next.result),");
  }
  return next.replace(/shouldStop: \(result\) => [^\n]+,/,
    "shouldStop: (result) => result.block === true || (result.result !== undefined && !result.result?.caiPepDeferredId),")
    .replace('terminalLabel: "block=true or result!=undefined"', 'terminalLabel: "block=true or immediate result"');
}

function withAdmittedResultResolver(source) {
  if (source.includes("async function caiPepResolveAdmittedResult(")) return source;
  return `// Resolve only at the original implementation invocation boundary.
async function caiPepResolveAdmittedResult(result, params, signal, fallback) {
  signal?.throwIfAborted();
  if (result?.caiPepDeferredId) {
    result = await globalThis[Symbol.for("confidential-agent.cai-pep.resolve")](result, params);
    if (result === undefined) {
      signal?.throwIfAborted();
      return fallback();
    }
  }
  return result !== undefined ? result : fallback();
}
` + source;
}

export function patchAgentLoop(source) {
  if (source.includes("function toToolDefinitions(") && source.includes('finalizerMode: "adapter"')) {
    source = editFunction(source, "toToolDefinitions", (body) => {
      body = body.replace("if (hookOutcome.result !== undefined) return hookOutcome.result;", "");
      if (!body.includes("let caiPepResult;")) {
        body = replaceOnce(body, /let executeParams = params;/, "$&\n            let caiPepResult;", "adapter result carrier");
        body = replaceOnce(body, /throw new Error\(hookOutcome\.reason\);\s*}/,
          "$&\n                caiPepResult = hookOutcome.result;", "adapter hook result");
      }
      if (body.includes("await caiPepResolveAdmittedResult(")) return body;
      return replaceOnce(body, /return [^\n]*tool\.execute\(toolCallId, executeParams, signal, onUpdate\);/,
        "return await caiPepResolveAdmittedResult(caiPepResult, executeParams, signal, () => tool.execute(toolCallId, executeParams, signal, onUpdate));", "adapter admitted result");
    });
    source = editFunction(source, "toClientToolDefinitions", (body) => {
      body = body.replace(/if \(outcome\.result !== undefined\) \{\s*recorder\?\.discard\?\.\(toolCallId, func\.name\);\s*return [^;]+;\s*}/, "");
      if (body.includes("await caiPepResolveAdmittedResult(")) return body;
      return replaceOnce(body, /if \(typeof onClientToolCall === "function"\)/,
        `if (outcome.result !== undefined) {
          const result = await caiPepResolveAdmittedResult(outcome.result, paramsRecord, signal, () => undefined);
          if (result !== undefined) {
            recorder?.discard?.(toolCallId, func.name);
            return result;
          }
        }
        $&`, "client admitted result");
    });
    return withAdmittedResultResolver(source);
  }
  if (source.includes("await caiPepResolveAdmittedResult(")) return source;
  if (source.includes("result = ") && source.includes("await tool.execute(toolCallId, finalizedToolArgs, params.signal);")) {
    source = replaceOnce(source, /result = [^\n]*tool\.execute\(toolCallId, finalizedToolArgs, params\.signal\);/,
      "result = await caiPepResolveAdmittedResult(hookResult.result, finalizedToolArgs, params.signal, () => tool.execute(toolCallId, finalizedToolArgs, params.signal));", "MCP admitted result");
  } else if (source.includes("await tool.execute?.(toolCallId, hookResult.params, params.signal);")) {
    source = replaceOnce(source, /return [^\n]*tool\.execute\?\.\(toolCallId, hookResult\.params, params\.signal\);/,
      "return await caiPepResolveAdmittedResult(hookResult.result, hookResult.params, params.signal, () => tool.execute?.(toolCallId, hookResult.params, params.signal));", "gateway admitted result");
  }
  return withAdmittedResultResolver(source);
}

function agentLoopSupportsHookResult(source) {
  return source.includes("await caiPepResolveAdmittedResult(") || source.includes("confidential-agent.cai-pep.resolve") || source.includes("hookResult?.result !== undefined") ||
    source.includes("hookOutcome.result !== undefined") ||
    source.includes("hookResult.result !== undefined");
}

export function patchBeforeToolCallRuntime(source) {
  if (!source.includes("const policyAdjustedParams =")) {
    throw new Error("unsupported OpenClaw runtime: deferred PEP requires the pinned approval and execution gates");
  }
  source = editFunction(source, "runBeforeToolCallHook", (body) => {
    // Only carry the cloneable ticket through policy and approval preparation.
    if (!body.includes("let ownerDecisionMarked = false;")) throw new Error("unsupported OpenClaw runtime: expected one patch location for approval outcome");
    body = body.replace(/if \(caiPepResult\?\.caiPepDeferredId\) \{[\s\S]*?caiPepResult = await resolve\(caiPepResult, finalParams\);\s*}/, "");
    if (body.includes("let caiPepResult;")) return body;
    body = replaceOnce(body, /let ownerDecisionMarked = false;/,
      "let caiPepResult;\n\t\tlet ownerDecisionMarked = false;", "runtime result carrier");
    body = replaceOnce(body, /}, policyAdjustedToolContext, receipt\);/,
      "$&\n\t\t\tcaiPepResult = hookResult?.result;", "runtime hook result");
    return replaceOnce(body, /const allowed = \{\s*blocked: false,\s*params: finalParams\s*};/,
      `const allowed = {
        blocked: false,
        params: finalParams,
        ...(caiPepResult !== undefined ? { result: caiPepResult } : {})
      };`, "runtime allowed result");
  });
  source = editFunction(source, "wrapToolWithBeforeToolCallHook", (body) => {
    body = body.replace(/if \(outcome\.result !== undefined\) \{[\s\S]*?return outcome\.loopWarning \? appendToolLoopWarning\(outcome\.result, outcome\.loopWarning\) : outcome\.result;\s*}/, "");
    if (body.includes("caiPepResolveAdmittedResult(outcome.result")) return body;
    // This site follows validation, scheduler pause/launch, voice confirmation,
    // cancellation, source guards and execution admission. Preserve diagnostics.
    return replaceOnce(body, /const invoke = \(\) => liveness\.run\([\s\S]*?;/,
      "const invoke = () => liveness.run(() => caiPepResolveAdmittedResult(outcome.result, executeParams, signal, () => execute(...args)));", "admitted implementation result");
  });
  return withAdmittedResultResolver(source);
}

export function patchOpenclaw(openclawRoot) {
  const distRoot = path.join(openclawRoot, "dist");
  if (!fs.existsSync(distRoot)) throw new Error(`openclaw dist not found at ${distRoot}`);
  const sources = new Map(walk(distRoot).map((file) => [file, fs.readFileSync(file, "utf8")]));
  const hooks = [...sources].filter(([, text]) => text.includes("runBeforeToolCall") && text.includes("terminalLabel"));
  const runtimes = [...sources].filter(([, text]) => text.includes("function wrapToolWithBeforeToolCallHook") && text.includes("function runBeforeToolCallHook"));
  const agents = [...sources].filter(([file, text]) => !runtimes.some(([runtime]) => runtime === file)
    && text.includes("runBeforeToolCall") && /\btool\.execute(?:\?\.)?\s*\(/.test(text));
  if (!hooks.length || !runtimes.length || !agents.length) {
    throw new Error("unable to locate all OpenClaw hook and execution modules");
  }
  const updates = new Map();
  for (const [file, text] of hooks) {
    const next = patchHookRunner(text);
    if (!(next.includes("result: lastDefined(acc?.result, next.result)")
        || next.includes("result: next.result !== undefined ? next.result : acc?.result"))
        || !next.includes("!result.result?.caiPepDeferredId")) {
      throw new Error(`failed to patch hook result transport in ${file}`);
    }
    updates.set(file, next);
  }
  for (const [file, text] of runtimes) {
    const next = patchBeforeToolCallRuntime(text);
    if (!(next.includes("hookResult?.result !== undefined") || next.includes("caiPepResult = hookResult?.result"))
        || !next.includes("caiPepResolveAdmittedResult(outcome.result")) {
      throw new Error(`failed to patch before_tool_call runtime in ${file}`);
    }
    updates.set(file, next);
  }
  for (const [file, text] of agents) {
    const next = patchAgentLoop(text);
    if (!agentLoopSupportsHookResult(next)) throw new Error(`failed to patch tool execution loop in ${file}`);
    updates.set(file, next);
  }
  // Validate every transformation before mutating any installed module.
  for (const [file, text] of updates) {
    const check = `${file}.cai-pep-check.mjs`;
    let created = false;
    try {
      fs.writeFileSync(check, text, { flag: "wx" });
      created = true;
      execFileSync(process.execPath, ["--check", check], { stdio: "pipe" });
    } finally {
      if (created) fs.rmSync(check, { force: true });
    }
  }
  for (const [file, text] of updates) {
    if (text !== sources.get(file)) fs.writeFileSync(file, text);
    console.log(`cai-pep patch: verified ${file}`);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const root = process.argv[2] || path.join(execFileSync("npm", ["root", "-g"], { encoding: "utf8" }).trim(), "openclaw");
    patchOpenclaw(root);
  } catch (error) {
    console.error(`cai-pep patch failed: ${error.message}`);
    process.exit(1);
  }
}
