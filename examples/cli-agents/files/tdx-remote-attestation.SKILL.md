---
name: tdx-remote-attestation
description: Use when the user asks whether this agent is running in a trusted Intel TDX confidential computing environment or asks to show remote attestation evidence.
---

# Intel TDX Remote Attestation

Use the local CAI attestation helper to collect and verify the current guest's Intel TDX evidence. Do not substitute CPU flags, device files, logs, or ad hoc scripts for remote attestation.

Run this exact command:

```bash
cai-pep attest collect-and-verify \
  --aa-url http://localhost:8006 \
  --tee tdx \
  --claims
```

If the command fails, report that remote attestation did not complete and include the important error text. Do not claim the environment is verified.

When it succeeds, inspect the decoded claims and summarize:

- The hardware trustworthiness result at `submods.cpu0["ear.trustworthiness-vector"].hardware`; treat values `<= 32` as passing.
- The TDX measurement fields, especially `mr_td`, `rtmr_0`, `rtmr_1`, and `rtmr_2`.
- Whether GPU evidence such as `nvidia_gpu.0` is present; only report GPU TEE verification when evidence exists in the claims.
- Any missing claims that prevent a confident conclusion.

Keep the answer concise. Show short measurement prefixes when useful, but do not paste the full JWT unless the user explicitly asks.
