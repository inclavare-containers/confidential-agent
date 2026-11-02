#!/usr/bin/env bash
set -Eeuo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT_DIR/one-click/lib/main.sh"
task_tmp="$(mktemp -d)"
cleanup() {
  local marker pid
  for marker in "$task_tmp"/*/connect.pid; do
    [[ -f "$marker" ]] || continue
    pid="$(cat "$marker")"
    kill "$pid" 2>/dev/null || true
  done
  rm -rf "$task_tmp"
}
trap cleanup EXIT
for target in openclaw openclaw-vllm vllm-inference; do
  (
    unset CA_CONNECT_READINESS_PATH
    CA_ONE_CLICK_TARGET="$target"
    init_defaults
    apply_target_defaults
    expected=/openclaw
    [[ "$target" != vllm-inference ]] || expected=/health
    [[ "$CA_CONNECT_READINESS_PATH" == "$expected" ]]
  )
done
(
  CA_ONE_CLICK_TARGET=vllm-inference CA_CONNECT_READINESS_PATH=/custom
  init_defaults
  apply_target_defaults
  [[ "$CA_CONNECT_READINESS_PATH" == /custom ]]
)
cat > "$task_tmp/endpoint" <<'PY'
#!/usr/bin/env python3.11
from http.server import BaseHTTPRequestHandler, HTTPServer
class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200 if self.path == '/health' else 404)
        self.end_headers()
    def log_message(self, *_):
        pass
server = HTTPServer(('127.0.0.1', 0), Handler)
print(f'connect 127.0.0.1:{server.server_port} -> test', flush=True)
server.serve_forever()
PY
printf '#!/usr/bin/env bash\nexit 17\n' > "$task_tmp/exited"
chmod +x "$task_tmp/endpoint" "$task_tmp/exited"
probe() (
  CA_WORK_DIR="$task_tmp/$1"
  mkdir -p "$CA_WORK_DIR"
  CA_SKIP_DEPLOY=0 CA_START_CONNECT=1 CA_CONNECT_NAME=connect
  CA_BIN="$task_tmp/$2" CA_TOOLS_DIR="$task_tmp" CA_STATE_DIR="$task_tmp"
  CA_SERVICE_ID=vllm-inference CA_CONNECT_TIMEOUT_SEC=4 CA_CONNECT_READINESS_PATH="$3"
  start_connect
)
probe healthy endpoint /health > "$task_tmp/healthy.log" 2>&1
[[ -s "$task_tmp/healthy/connect.pid" ]]
if probe unavailable endpoint /unavailable > "$task_tmp/unavailable.log" 2>&1; then
  echo 'HTTP readiness timeout incorrectly returned success' >&2
  exit 1
fi
if probe exited exited /health > "$task_tmp/exited.log" 2>&1; then
  echo 'Connect process failure incorrectly returned success' >&2
  exit 1
fi
printf 'target readiness defaults, override, real HTTP readiness and failure exits passed\n'
