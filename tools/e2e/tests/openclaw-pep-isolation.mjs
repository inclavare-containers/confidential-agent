#!/usr/bin/env node
import assert from "node:assert/strict";
import fs from "node:fs";
import { functionSource } from "../../../examples/openclaw/files/patch-openclaw-cai-pep.js";

// Load the actual probe helpers without starting gateways or allocating sandboxes.
const source = fs.readFileSync(new URL("../probes/openclaw-pep-probe.mjs", import.meta.url), "utf8");
const load = name => new Function("assert", `return (${functionSource(source, name).text});`)(assert);
const readIsolation = load("readSandboxIsolation");
const checkCapabilities = load("assertSandboxCapabilities");
const inspect = {
  State: { Pid: 123 },
  HostConfig: { CapDrop: ["CAP_CHOWN", "CAP_NET_RAW"], SecurityOpt: ["no-new-privileges"],
    NetworkMode: "none", ReadonlyRootfs: true, Memory: 134217728, PidsLimit: 32 },
  Config: { Env: ["PRIVATE_VALUE=DO_NOT_COPY"] },
  Mounts: [{ Type: "bind", Source: "/tmp/probe/workspace", Destination: "/workspace", RW: true }],
};
const zero = "CapEff:\t0000000000000000\nCapBnd:\t0000000000000000\nNoNewPrivs:\t1\n";
const evidence = readIsolation(inspect, pid => { assert.equal(pid, 123); return zero; });
checkCapabilities(evidence);
assert.equal(evidence.kernel.NoNewPrivs, "1");
assert.equal(evidence.networkMode, "none");
assert.equal(JSON.stringify(evidence).includes("PRIVATE_VALUE"), false);
assert.equal(JSON.stringify(evidence).includes("DO_NOT_COPY"), false);

// An inspect "ALL" label or empty EffectiveCaps array cannot hide retained
// kernel capabilities; either effective OR bounding capability is a failure.
const claimedDrop = { ...inspect, HostConfig: { ...inspect.HostConfig, CapDrop: ["ALL"] }, EffectiveCaps: [] };
for (const field of ["CapEff", "CapBnd"]) {
  const retained = readIsolation(claimedDrop, () => zero.replace(`${field}:\t0000000000000000`, `${field}:\t0000000000000001`));
  assert.throws(() => checkCapabilities(retained), new RegExp(field));
}
const missing = readIsolation(inspect, () => "CapEff:\t0000000000000000\n");
assert.throws(() => checkCapabilities(missing), /CapBnd/);
const unreadable = readIsolation(inspect, () => { throw Object.assign(new Error("gone"), { code: "ENOENT" }); });
assert.equal(unreadable.kernelReadError, "ENOENT");
assert.throws(() => checkCapabilities(unreadable), /could not read/);
const noPid = readIsolation({ ...inspect, State: { Pid: 0 } }, () => { throw new Error("invalid PID was read"); });
assert.throws(() => checkCapabilities(noPid), /could not read/);
for (const value of ["0", "2", null]) {
  const status = zero.replace("NoNewPrivs:\t1\n", value === null ? "" : `NoNewPrivs:\t${value}\n`);
  assert.throws(() => checkCapabilities(readIsolation(inspect, () => status)), /NoNewPrivs/);
}
console.log("PEP isolation enforces zero effective+bounding masks and NoNewPrivs=1, rejects missing fields, and omits environment secrets: PASS");
