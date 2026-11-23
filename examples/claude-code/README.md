# Claude Code Confidential Agent

This example installs Claude Code in a TDX guest and configures it for Bailian
Pay-as-you-go `qwen4-max` through the Anthropic-compatible endpoint.
Credentials are runtime resources and are released to the guest only after
remote attestation succeeds.

> **Development example.** Claude Code is used over SSH, and this image does
> not include the attested management SSH service, so `claude-code.yaml`
> deploys a `debug` image with build-time SSH. Do not use it in production.
> For production, deploy the shared image with `confidential-agent init
> claude-code ...`, which uses a release image and per-instance management SSH
> keys delivered through remote attestation (see
> [one-click](../../docs/guides/one-click.md) or
> [manual deployment](../../docs/guides/manual-deployment.md)).

No Claude Code hook, long-running PEP service, or PEP policy is installed. The
`cai-pep` binary is included only so the installed skill can run
`cai-pep attest collect-and-verify` on demand.

## Prerequisites

Run commands from the repository root. You need:

- `confidential-agent` CLI available on `PATH`
- Alibaba Cloud credentials exported in your shell
- `DASHSCOPE_API_KEY` exported in your shell
- `openssl` and SSH tooling available locally

Optional overrides:

```bash
export DASHSCOPE_MODEL="${DASHSCOPE_MODEL:-qwen4-max}"
export DASHSCOPE_ANTHROPIC_BASE_URL="${DASHSCOPE_ANTHROPIC_BASE_URL:-https://dashscope.aliyuncs.com/apps/anthropic}"
```

## Prepare Resources

The AppSpec copies the RPM-installed `/usr/bin/cai-pep` into the image. From a
source checkout, install your `cai-pep` build at that path or edit
`build.files` in `claude-code.yaml`. Create the Rekor signing key:

```bash
confidential-agent key generate-cosign --output-key-prefix examples/claude-code/cosign
```

Create the remote-attested Claude Code resources:

```bash
mkdir -p examples/claude-code/secrets

cat > examples/claude-code/secrets/settings.json <<EOF
{
  "env": {
    "ANTHROPIC_AUTH_TOKEN": "${DASHSCOPE_API_KEY}",
    "ANTHROPIC_BASE_URL": "${DASHSCOPE_ANTHROPIC_BASE_URL:-https://dashscope.aliyuncs.com/apps/anthropic}",
    "ANTHROPIC_MODEL": "${DASHSCOPE_MODEL:-qwen4-max}",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "qwen4-max",
    "ANTHROPIC_DEFAULT_SONNET_MODEL": "${DASHSCOPE_MODEL:-qwen4-max}",
    "ANTHROPIC_DEFAULT_OPUS_MODEL": "${DASHSCOPE_MODEL:-qwen4-max}",
    "CLAUDE_CODE_SUBAGENT_MODEL": "${DASHSCOPE_MODEL:-qwen4-max}"
  }
}
EOF

printf '{"hasCompletedOnboarding": true}\n' > examples/claude-code/secrets/claude.json
chmod 0600 examples/claude-code/secrets/settings.json examples/claude-code/secrets/claude.json
```

The `secrets/` files are not baked into the image. They are declared as
Confidential Agent resources in `claude-code.yaml`.

## Deploy

Set `OPERATOR_CIDR` to the operator network's IPv4 CIDR, then validate, build,
and deploy:

```bash
confidential-agent peering add --role operator --cidr "$OPERATOR_CIDR" --label ops
confidential-agent spec validate --spec examples/claude-code/claude-code.yaml
confidential-agent build --spec examples/claude-code/claude-code.yaml
confidential-agent deploy --spec examples/claude-code/claude-code.yaml
confidential-agent status --live
```

If you use a non-default state directory, pass the same `--state-dir <dir>` to
each command.

## SSH Operation

Connect using the debug SSH command from deploy/status. In the guest, start
Claude Code:

```bash
cd /workspace
claude
```

For a non-interactive smoke test:

```bash
claude --print "Reply with CA_E2E_OK and no other text." \
  --max-turns 4 --output-format text
```

To show the remote attestation result through the installed skill:

```bash
claude --print "/tdx-remote-attestation Run the required remote attestation command and summarize the result." \
  --max-turns 8 --output-format text --allowedTools Bash
```

## Cleanup

Destroy the cloud resources when finished:

```bash
confidential-agent destroy claude-code
```
