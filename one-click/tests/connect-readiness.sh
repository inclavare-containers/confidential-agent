#!/usr/bin/env bash
set -Eeuo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
. "$ROOT_DIR/one-click/lib/main.sh"
task_tmp="$(mktemp -d)"
server_pid=""
trap '[[ -z "$server_pid" ]] || kill "$server_pid"; rm -rf "$task_tmp"' EXIT
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
cat > "$task_tmp/endpoint" <<'PYTHON'
#!/usr/bin/env python3.11
from http.server import BaseHTTPRequestHandler, HTTPServer
class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200 if self.path == '/health' else 404)
        self.end_headers()
    def log_message(self, *_):
        pass
server = HTTPServer(('127.0.0.1', 0), Handler)
print(server.server_port, flush=True)
server.serve_forever()
PYTHON
python3.11 "$task_tmp/endpoint" > "$task_tmp/port" &
server_pid=$!
for _ in {1..100}; do
  [[ ! -s "$task_tmp/port" ]] || break
  sleep .05
done
CA_ONE_CLICK_TARGET=vllm-inference
CA_CONNECT_HOST=127.0.0.1 CA_CONNECT_PORT="$(cat "$task_tmp/port")"
CA_CONNECT_READINESS_PATH=/health
probe_connect_endpoint
CA_CONNECT_READINESS_PATH=/unavailable
if probe_connect_endpoint 2>/dev/null; then
  echo 'Readiness probe accepted an unavailable endpoint' >&2
  exit 1
fi
printf 'target readiness defaults, override and real HTTP probe passed\n'
