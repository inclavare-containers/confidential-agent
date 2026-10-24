"""Only complete metrics GETs may participate in distribution assertions."""
from contextlib import contextmanager
import errno
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import ssl
import sys
import tempfile
import threading
import time
import unittest
import urllib.error

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import inference_case
from metrics_probe import complete_metrics, retryable_transport_error


@contextmanager
def endpoint(modes):
    state = {"gets": 0, "posts": 0, "counter": 0}

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        def do_GET(self):
            index = state["gets"]
            state["gets"] += 1
            mode = modes[min(index, len(modes) - 1)]
            if mode == "http-error":
                self.send_error(503)
                return
            body = f'vllm:request_success_total{{model_name="test"}} {state["counter"]}\n'.encode()
            if mode == "semantic-error":
                body = b"no model counter here\n"
            body += b"# complete metrics response\n"
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Content-Type", "text/plain")
            self.send_header("Connection", "close")
            self.end_headers()
            try:
                if mode == "drip":
                    for byte in body:
                        self.wfile.write(bytes([byte]))
                        self.wfile.flush()
                        time.sleep(0.03)
                else:
                    if mode == "slow-truncated":
                        time.sleep(0.15)
                    if mode in ["truncated", "slow-truncated"]:
                        body = body.split(b"#", 1)[0]
                    self.wfile.write(body)
                    self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass
            self.close_connection = True

        def do_POST(self):
            self.rfile.read(int(self.headers.get("Content-Length", 0)))
            state["posts"] += 1
            state["counter"] += 1
            self.send_response(200)
            self.send_header("Content-Length", "2")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(b"{}")
            self.close_connection = True

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}", state
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


class MetricsProbeTests(unittest.TestCase):
    def test_truncated_then_complete_preserves_each_attempt_and_does_not_repeat_posts(self):
        with tempfile.TemporaryDirectory() as directory, endpoint(
                ["truncated", "complete", "truncated", "complete"]) as (base, state):
            root = Path(directory)
            before = inference_case.request_count(base, root / "before-i-owned")
            for _ in range(16):
                inference_case.request(base, "/v1/chat/completions", {"model": "test"})
            after = inference_case.request_count(base, root / "after-i-owned")
            self.assertEqual((before, after), (0, 16))
            self.assertEqual((state["gets"], state["posts"]), (4, 16))
            results = [json.loads(p.read_text()) for p in sorted(root.glob("*.result.json"))]
            self.assertEqual(len(results), 4)
            self.assertEqual(sum(r["response"]["complete"] for r in results), 2)
            self.assertTrue(all(r["endpoint"] == base for r in results))
            partial = (root / "before-i-owned-attempt-1.body").read_bytes()
            self.assertIn(b"vllm:request_success_total", partial)
            self.assertNotIn(b"# complete", partial)

    def test_three_partial_bodies_with_valid_counters_never_pass(self):
        with tempfile.TemporaryDirectory() as directory, endpoint(["truncated"]) as (base, state):
            with self.assertRaisesRegex(RuntimeError, "complete metrics response unavailable"):
                inference_case.request_count(base, Path(directory) / "metrics-i-owned")
            self.assertEqual(state["gets"], 3)
            results = list(Path(directory).glob("*.result.json"))
            self.assertEqual(len(results), 3)
            self.assertTrue(all(not json.loads(p.read_text())["response"]["complete"] for p in results))

    def test_one_absolute_deadline_bounds_a_slow_trickling_body(self):
        with tempfile.TemporaryDirectory() as directory, endpoint(["drip"]) as (base, state):
            started = time.monotonic()
            with self.assertRaises(RuntimeError):
                complete_metrics(base, Path(directory) / "slow-i-owned", timeout=0.3)
            self.assertLess(time.monotonic() - started, 1.2)
            self.assertEqual(state["gets"], 1)
            record = json.loads(next(Path(directory).glob("*.result.json")).read_text())
            self.assertTrue(record["timed_out"])
            self.assertGreater(record["bytes"], 0)

    def test_retry_does_not_reset_the_total_deadline(self):
        with tempfile.TemporaryDirectory() as directory, endpoint(
                ["slow-truncated", "slow-truncated", "complete"]) as (base, state):
            started = time.monotonic()
            with self.assertRaises(RuntimeError):
                complete_metrics(base, Path(directory) / "deadline-i-owned", timeout=0.3)
            self.assertLess(time.monotonic() - started, 1.2)
            self.assertLessEqual(state["gets"], 2)

    def test_http_and_metrics_semantic_failures_are_not_retried(self):
        for mode, error in [("http-error", RuntimeError), ("semantic-error", AssertionError)]:
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory, \
                    endpoint([mode, "complete"]) as (base, state):
                with self.assertRaises(error):
                    inference_case.request_count(base, Path(directory) / mode)
                self.assertEqual(state["gets"], 1)

    def test_complete_low_counter_is_returned_without_hiding_failed_distribution(self):
        with tempfile.TemporaryDirectory() as directory, endpoint(["complete"]) as (base, state):
            root = Path(directory)
            before = inference_case.request_count(base, root / "before")
            after = inference_case.request_count(base, root / "after")
            self.assertFalse(after > before)
            self.assertEqual(state["gets"], 2)

    def test_only_observed_connection_reset_is_retryable(self):
        self.assertTrue(retryable_transport_error(ConnectionResetError(errno.ECONNRESET, "reset")))
        self.assertTrue(retryable_transport_error(urllib.error.URLError(
            ConnectionResetError(errno.ECONNRESET, "reset"))))
        for error in [TimeoutError(), OSError(errno.ECONNREFUSED, "refused"),
                      urllib.error.URLError(ssl.SSLCertVerificationError("invalid certificate"))]:
            self.assertFalse(retryable_transport_error(error))


if __name__ == "__main__":
    unittest.main()
