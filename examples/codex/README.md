# Codex Confidential Agent

This example installs Codex in a TDX guest, configures Bailian Pay-as-you-go
`qwen4-max` through the Responses API, and exposes Codex `app-server` remote
mode through a TNG-protected `connect` port.

No Codex hook, long-running PEP service, or PEP policy is installed. The
`cai-pep` binary is included only so the installed skill can run
`cai-pep attest collect-and-verify` on demand.

## Prerequisites

Run commands from the repository root. You need:

- `confidential-agent` CLI available on `PATH`
- Alibaba Cloud credentials exported in your shell
- `DASHSCOPE_API_KEY` exported in your shell
- `cargo`, `openssl`, and SSH tooling available locally
- a local `codex` CLI only when you want to use interactive remote mode

Optional overrides:

```bash
export DASHSCOPE_MODEL="${DASHSCOPE_MODEL:-qwen4-max}"
export DASHSCOPE_BASE_URL="${DASHSCOPE_BASE_URL:-https://dashscope.aliyuncs.com/compatible-mode/v1}"
```

## Prepare Resources

Build the local helper binary referenced by the AppSpec and create the Rekor
signing key:

```bash
cargo build -p cai-pep
confidential-agent key generate-cosign --output-key-prefix examples/codex/cosign
```

Create the remote-attested Codex resources:

```bash
mkdir -p examples/codex/secrets

cat > examples/codex/secrets/config.toml <<EOF
model_provider = "Model_Studio"
model = "${DASHSCOPE_MODEL:-qwen4-max}"

[model_providers.Model_Studio]
name = "Model_Studio"
base_url = "${DASHSCOPE_BASE_URL:-https://dashscope.aliyuncs.com/compatible-mode/v1}"
env_key = "OPENAI_API_KEY"
wire_api = "responses"
EOF

cat > examples/codex/secrets/codex.env <<EOF
OPENAI_API_KEY=${DASHSCOPE_API_KEY}
CODEX_HOME=/root/.codex
EOF

openssl rand -base64 32 > examples/codex/secrets/app-server-token
chmod 0600 examples/codex/secrets/config.toml examples/codex/secrets/codex.env examples/codex/secrets/app-server-token
```

The `secrets/` files are not baked into the image. They are declared as
Confidential Agent resources in `codex.yaml`.

## Deploy

Authorize your operator CIDR, then validate, build, and deploy:

```bash
confidential-agent peering add --role operator --cidr <operator-cidr> --label ops
confidential-agent spec validate --spec examples/codex/codex.yaml
confidential-agent build --spec examples/codex/codex.yaml
confidential-agent deploy --spec examples/codex/codex.yaml
confidential-agent status --live
```

If you use a non-default state directory, pass the same `--state-dir <dir>` to
each command.

## SSH Operation

Use the debug SSH command shown by deploy/status, then start Codex:

```bash
ssh -i <debug-private-key> root@<guest-ip>
cd /workspace
set -a
. /root/.config/confidential-agent/codex/codex.env
set +a
codex
```

For a non-interactive smoke test:

```bash
codex --ask-for-approval never exec \
  --skip-git-repo-check --sandbox danger-full-access \
  "Reply with CA_E2E_OK and no other text."
```

To show the remote attestation result through the installed skill:

```bash
codex --ask-for-approval never exec \
  --skip-git-repo-check --sandbox danger-full-access \
  'Use $tdx-remote-attestation to run the required remote attestation command and summarize the result.'
```

## Remote Operation

Start a TNG-protected local tunnel to the guest app-server:

```bash
confidential-agent connect start \
  --service codex \
  --ready-json ./codex-connect-ready.json \
  --wait-ready 180
```

Read the mapped local port from `codex-connect-ready.json`, then connect a
local Codex CLI to it with the app-server token resource:

```bash
export CODEX_REMOTE_TOKEN="$(cat examples/codex/secrets/app-server-token)"
codex --remote ws://127.0.0.1:<mapped-port> \
  --remote-auth-token-env CODEX_REMOTE_TOKEN
```

Stop the tunnel when finished:

```bash
confidential-agent connect stop --ready-json ./codex-connect-ready.json
```

## Cleanup

Destroy the cloud resources when finished:

```bash
confidential-agent destroy codex
```
