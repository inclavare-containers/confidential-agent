import net from "node:net";
import crypto from "node:crypto";

const passthroughConfigSchema = {
  safeParse(value) {
    return { success: true, data: value ?? {} };
  },
  uiHints: {
    socketPath: { label: "PEP Socket Path" },
    pepRequired: { label: "Fail Closed When PEP Unavailable" },
    defaultWorkdir: { label: "Default Sandbox Workdir" },
  },
};

function normalizeWorkdir(params, defaultWorkdir) {
  if (!params || typeof params !== "object") {
    return defaultWorkdir;
  }
  if (typeof params.workdir === "string" && params.workdir.trim()) {
    return params.workdir.trim();
  }
  if (typeof params.cwd === "string" && params.cwd.trim()) {
    return params.cwd.trim();
  }
  return defaultWorkdir;
}

function normalizeCommand(params) {
  if (!params || typeof params !== "object") {
    return "";
  }
  if (typeof params.command === "string") {
    return params.command;
  }
  if (typeof params.cmd === "string") {
    return params.cmd;
  }
  return "";
}

function buildRequest({ command, workdir, ctx, socketPath }) {
  return {
    method: "submit_intent",
    id: `req-${crypto.randomUUID()}`,
    params: {
      version: 1,
      run_id: ctx?.runId ?? `run-${Date.now()}`,
      session_key: ctx?.sessionKey ?? "agent:unknown",
      agent_id: ctx?.agentId ?? "main",
      tool_name: "exec",
      skill_id: "cai-pep.exec",
      params: {
        command,
        workdir,
      },
      request_context: {
        provider: "openclaw",
        socket_path: socketPath,
      },
      issued_at_ms: Date.now(),
    },
  };
}

function mapExecResult(result) {
  const stdout = result.stdout;
  const stderr = result.stderr;
  return {
    content: [{ type: "text", text: [stdout, stderr].filter(Boolean).join("\n") || `(exit ${result.exit_code})` }],
    isError: result.exit_code !== 0,
    stdout: typeof result?.stdout === "string" ? result.stdout : "",
    stderr: typeof result?.stderr === "string" ? result.stderr : "",
    exitCode:
      typeof result?.exit_code === "number"
        ? result.exit_code
        : typeof result?.exitCode === "number"
          ? result.exitCode
          : 1,
    durationMs: typeof result?.duration_ms === "number" ? result.duration_ms : undefined,
    auditId: typeof result?.audit_id === "string" ? result.audit_id : undefined,
    backend: typeof result?.backend === "string" ? result.backend : "docker",
    sandboxProfile:
      typeof result?.sandbox_profile === "string" ? result.sandbox_profile : "default-docker",
  };
}

function submitIntent(socketPath, payload, timeoutMs = 120000) {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection({ path: socketPath });
    let raw = "";

    socket.setTimeout(timeoutMs, () => socket.destroy(new Error("cai-pep response timed out")));
    socket.on("connect", () => {
      socket.end(JSON.stringify(payload));
    });
    socket.on("data", (chunk) => {
      raw += chunk.toString("utf8");
      if (raw.length > 2 * 1024 * 1024) socket.destroy(new Error("cai-pep response is too large"));
    });
    socket.on("end", () => {
      try {
        resolve(JSON.parse(raw));
      } catch (error) {
        reject(new Error(`invalid cai-pep response JSON: ${error.message}`));
      }
    });
    socket.on("error", (error) => {
      reject(error);
    });
  });
}

// Internal protocol shared with patch-openclaw-cai-pep.js. Tickets are one-use
// and bounded; rejected or pending approvals never open the PEP socket.
const deferredExecs = globalThis[Symbol.for("confidential-agent.cai-pep.tickets")] ??= new Map();
globalThis[Symbol.for("confidential-agent.cai-pep.resolve")] ??= async (ticket, params) => {
  const entry = deferredExecs.get(ticket.caiPepDeferredId);
  deferredExecs.delete(ticket.caiPepDeferredId);
  if (!entry || Date.now() - entry.createdAt > 300000) throw new Error("cai-pep execution ticket expired");
  return entry.execute(params);
};

const plugin = {
  id: "cai-pep",
  name: "CAI PEP",
  description: "Redirect exec tool calls to cai-pep over a Unix socket",
  configSchema: passthroughConfigSchema,

  register(api) {
    api.logger.info("cai-pep plugin registered");
    api.on("before_tool_call", async (event, ctx) => {
      if (event?.toolName !== "exec") {
        return {};
      }

      const topLevel = api.config?.confidentialAgent ?? {};
      const pluginConfig = api.pluginConfig ?? {};
      const socketPath =
        pluginConfig.socketPath ??
        topLevel.pepSocket ??
        "/run/cai/pep.sock";
      const pepRequired = pluginConfig.pepRequired ?? topLevel.pepRequired ?? true;
      const defaultWorkdir =
        pluginConfig.defaultWorkdir ?? topLevel.defaultWorkdir ?? "/workspace";

      const command = normalizeCommand(event?.params);
      const workdir = normalizeWorkdir(event?.params, defaultWorkdir);
      if (!command.trim()) {
        return {
          block: true,
          blockReason: "cai-pep: exec command is empty",
        };
      }

      // The hook may still be followed by OpenClaw approval gates. Only publish
      // a cloneable ticket here; the patched runtime consumes it after approval.
      const id = crypto.randomUUID();
      const now = Date.now();
      for (const [key, entry] of deferredExecs) {
        if (now - entry.createdAt > 300000 || deferredExecs.size >= 1024) deferredExecs.delete(key);
      }
      deferredExecs.set(id, { createdAt: now, async execute(finalParams) {
        const finalCommand = normalizeCommand(finalParams);
        if (!finalCommand.trim()) throw new Error("cai-pep: exec command is empty");
        const payload = buildRequest({ command: finalCommand,
          workdir: normalizeWorkdir(finalParams, defaultWorkdir), ctx, socketPath });
        let response;
        try {
          response = await submitIntent(socketPath, payload, pluginConfig.responseTimeoutMs ?? 240000);
        } catch (error) {
          api.logger.error?.(`cai-pep request failed: ${error.message}`);
          if (!pepRequired) return undefined;
          throw new Error(`cai-pep unavailable: ${error.message}`);
        }
        if (response?.error) {
          const reason = response.error.audit_id
            ? `${response.error.message} (audit_id=${response.error.audit_id})`
            : response.error.message;
          throw new Error(`cai-pep deny: ${reason}`);
        }
        const result = response?.result;
        if (response?.id !== payload.id || result?.status !== "ok" || result?.decision !== "allow" ||
            typeof result.stdout !== "string" || typeof result.stderr !== "string" ||
            !Number.isInteger(result.exit_code) || typeof result.audit_id !== "string" || !result.audit_id ||
            !Number.isFinite(result.duration_ms) || result.duration_ms < 0 ||
            !((result.backend === "docker" && result.sandbox_profile === "default-docker") ||
              (result.backend === "host-attestation" && result.sandbox_profile === "attestation-helper"))) {
          throw new Error("cai-pep returned an invalid result payload");
        }
        return mapExecResult(result);
      } });
      return { result: { caiPepDeferredId: id } };
    });
  },
};

export default plugin;
