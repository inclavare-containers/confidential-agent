#!/usr/bin/env bash

assert_codex_secret_rendering() {
  local dashscope_key="$1"
  local remote_token="$2"
  local spec="$CODEX_DIR/codex.yaml"
  local install_script="$CODEX_DIR/install-codex.sh"
  for secret in "$dashscope_key" "$remote_token"; do
    for path in "$spec" "$install_script"; do
      if [[ -n "$secret" ]] && grep -Fq "$secret" "$path"; then
        echo "Codex rendered file contains secret material: $path" >&2
        return 1
      fi
    done
  done
  grep -Fq "wire_api = \"responses\"" "$CODEX_DIR/secrets/config.toml"
  grep -Fq "packages: [ca-certificates, curl, git, jq, nodejs, npm, procps-ng, tar, xz]" "$spec"
  grep -Fq "source: $ROOT_DIR/examples/cli-agents/files/install-cli-agent-runtime.sh" "$spec"
  grep -Fq "target: /usr/local/libexec/confidential-agent/cli-agent/install-cli-agent-runtime.sh" "$spec"
  grep -Fq "target: /root/.codex/config.toml" "$spec"
  grep -Fq "target: /root/.agents/skills/tdx-remote-attestation/SKILL.md" "$spec"
  grep -Fq "app_service: cai-codex-app-server.service" "$spec"
  grep -Fq "scripts: [./install-codex.sh]" "$spec"
  grep -Fq "image_variant: debug" "$spec"
  grep -Fq "target: /root/.codex/app-server-token" "$spec"
  grep -Fq "source: ./secrets/app-server-token" "$spec"
  grep -Fq "CODEX_VERSION" "$install_script"
  grep -Fq "CLI_AGENT_NODE_VERSION" "$install_script"
  grep -Fq "NPM_REGISTRY" "$install_script"
  grep -Fq "/usr/local/libexec/confidential-agent/cli-agent/install-cli-agent-runtime.sh codex" "$install_script"
  assert_init_script_extends_example \
    "$install_script" \
    "$ROOT_DIR/examples/codex/install-codex.sh" \
    CLI_AGENT_NODE_VERSION NPM_REGISTRY CODEX_VERSION
  test -s "$CODEX_DIR/secrets/config.toml"
  test -s "$CODEX_DIR/secrets/codex.env"
  test -s "$CODEX_DIR/secrets/app-server-token"
  record "- Codex rendered spec keeps provider/API and remote tokens in remote-attested resources only."
}

codex_ssh_info() {
  python3.11 - "$WORK_DIR/status-live.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    data = json.load(f)
item = data[0] if isinstance(data, list) else data
local = item.get("local") or item
cloud = local.get("cloud") or local.get("deploy") or {}
build = local.get("build") or {}
debug = build.get("debug_ssh") or {}
print(cloud.get("public_ip") or cloud.get("private_ip") or "")
print(debug.get("private_key") or "")
PY
}

collect_codex_guest_diagnostics() {
  local host="$1"
  local key="$2"
  local label="$3"
  local out="$WORK_DIR/codex-guest-diagnostics-$label.txt"
  record_cmd "ssh codex '<runtime diagnostics>'"
  ssh_guest "$key" "$host" /bin/bash >"$out" 2>&1 <<'REMOTE' || true
set +e
echo "### versions"
node --version
npm --version
codex --version
cai-pep --help | sed -n '1,40p'
echo "### resources"
find /root/.codex /root/.agents /root/.config/confidential-agent/codex -maxdepth 4 -printf "%p %m %u:%g %s bytes\n" 2>&1
echo "### services"
systemctl status cai-codex-app-server.service --no-pager -l
journalctl -b -u cai-codex-app-server.service -n 240 --no-pager
echo "### local ready"
curl -fsS -D - --max-time 10 http://127.0.0.1:4500/readyz 2>&1
REMOTE
  record_file_as_block "Codex guest diagnostics ($label):" "$out" text
}

assert_codex_guest_runtime() {
  local host="$1"
  local key="$2"
  local out="$WORK_DIR/codex-guest-runtime.txt"
  local err="$WORK_DIR/codex-guest-runtime.err"
  record_cmd "ssh codex '<runtime assertions>'"
  if ssh_guest "$key" "$host" /bin/bash >"$out" 2>"$err" <<'REMOTE'
set -euo pipefail
command -v codex
command -v ps
ps -p "$$" -o pid= -o lstart= -o args= >/dev/null
command -v cai-pep
codex --version
(cai-pep --help 2>&1 || true) | grep -Fq "cai-pep attest collect-and-verify"
systemctl is-active --quiet cai-codex-app-server.service
test -s /root/.codex/config.toml
test -s /root/.config/confidential-agent/codex/codex.env
test -s /root/.codex/app-server-token
test -s /root/.agents/skills/tdx-remote-attestation/SKILL.md
test "$(stat -c %a /root/.codex/config.toml)" = "600"
test "$(stat -c %a /root/.config/confidential-agent/codex/codex.env)" = "600"
grep -Fq 'wire_api = "responses"' /root/.codex/config.toml
grep -Fq 'model_provider = "Model_Studio"' /root/.codex/config.toml
grep -Fq 'env_key = "OPENAI_API_KEY"' /root/.codex/config.toml
curl -fsS --max-time 10 http://127.0.0.1:4500/readyz >/dev/null
REMOTE
  then
    record_file_as_block "Codex guest runtime assertions:" "$out" text
    record_file_as_block "Codex guest runtime assertion stderr:" "$err" text
    return 0
  fi
  record_file_as_block "Codex guest runtime assertions:" "$out" text
  record_file_as_block "Codex guest runtime assertion stderr:" "$err" text
  return 1
}

run_codex_exec_prompt() {
  local host="$1"
  local key="$2"
  local label="$3"
  local prompt="$4"
  local expect="$5"
  local out="$WORK_DIR/codex-$label.out"
  local err="$WORK_DIR/codex-$label.err"
  record_cmd "ssh codex 'codex exec <redacted prompt>'"
  if ssh_guest "$key" "$host" /bin/bash >"$out" 2>"$err" <<REMOTE
set -euo pipefail
set -a
. /root/.config/confidential-agent/codex/codex.env
set +a
export HOME=/root
export CODEX_HOME=/root/.codex
mkdir -p /workspace/codex-e2e
cd /workspace/codex-e2e
git init -q
codex --ask-for-approval never exec --skip-git-repo-check --sandbox danger-full-access --json $(printf '%q' "$prompt")
REMOTE
  then
    record_file_as_block "Codex $label stdout:" "$out" json
    record_file_as_block "Codex $label stderr:" "$err" text
    grep -Fq "$expect" "$out" || return 1
    return 0
  fi
  record_file_as_block "Codex $label stdout:" "$out" json
  record_file_as_block "Codex $label stderr:" "$err" text
  return 1
}

run_codex_remote_probe() {
  local connect_port="$1"
  local token="$2"
  local out="$WORK_DIR/codex-remote.out"
  local err="$WORK_DIR/codex-remote.err"

  record_cmd "CODEX_REMOTE_TOKEN=<redacted> websocket upgrade ws://127.0.0.1:$connect_port/"
  if CODEX_REMOTE_TOKEN="$token" CONNECT_PORT="$connect_port" python3.11 >"$out" 2>"$err" <<'PY'
import base64
import os
import socket


def probe(label: str, token: str) -> str:
    port = int(os.environ["CONNECT_PORT"])
    key = base64.b64encode(os.urandom(16)).decode("ascii")
    request = (
        "GET / HTTP/1.1\r\n"
        f"Host: 127.0.0.1:{port}\r\n"
        "Connection: Upgrade\r\n"
        "Upgrade: websocket\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        f"Sec-WebSocket-Key: {key}\r\n"
        f"Authorization: Bearer {token}\r\n"
        "\r\n"
    ).encode("ascii")
    with socket.create_connection(("127.0.0.1", port), timeout=10) as sock:
        sock.settimeout(10)
        sock.sendall(request)
        response = b""
        while b"\r\n\r\n" not in response:
            chunk = sock.recv(4096)
            if not chunk:
                break
            response += chunk
    status = response.split(b"\r\n", 1)[0].decode("latin1", "replace")
    print(f"{label}: {status}")
    return status


good = probe("configured-token", os.environ["CODEX_REMOTE_TOKEN"])
bad = probe("wrong-token", "ca-e2e-wrong-token")
if " 101 " not in good:
    raise SystemExit(f"configured token was not accepted: {good}")
if " 401 " not in bad:
    raise SystemExit(f"wrong token was not rejected: {bad}")
PY
  then
    record_file_as_block "Codex remote WebSocket probe stdout:" "$out" text
    record_file_as_block "Codex remote WebSocket probe stderr:" "$err" text
    record "- Codex remote WebSocket endpoint accepted the configured token and rejected an invalid token."
    return 0
  fi
  record_file_as_block "Codex remote WebSocket probe stdout:" "$out" text
  record_file_as_block "Codex remote WebSocket probe stderr:" "$err" text
  return 1
}
