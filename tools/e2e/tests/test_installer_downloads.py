"""Exercise real HTTP failures without installing GPU software."""
import http.server
from pathlib import Path
import re
import subprocess
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[3]
SCRIPTS = ['examples/vllm-inference/install-vllm-inference.sh',
           'examples/openclaw-vllm/install-openclaw-vllm.sh']


class InstallerDownloads(unittest.TestCase):
    def invoke(self, script, path, url):
        source = (ROOT / script).read_text()
        function = re.search(r'(?ms)^download_installer\(\) \{\n.*?^\}\n', source)
        self.assertIsNotNone(function)
        return subprocess.run(['bash', '-c', function[0] + '\ndownload_installer "$1" "$2"',
                               'installer-download-test', url, str(path)],
                              capture_output=True, timeout=20)

    def test_transient_http_failure_retries_and_promotes_only_complete_body(self):
        class Handler(http.server.BaseHTTPRequestHandler):
            calls = 0
            def do_GET(self):
                type(self).calls += 1
                body = b'busy' if self.calls == 1 else b'complete installer bytes'
                self.send_response(503 if self.calls == 1 else 200)
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            def log_message(self, *_):
                pass
        with tempfile.TemporaryDirectory() as tmp:
            for script in SCRIPTS:
                Handler.calls = 0
                server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
                thread = threading.Thread(target=server.serve_forever, daemon=True)
                thread.start()
                try:
                    destination = Path(tmp) / 'installer'
                    destination.unlink(missing_ok=True)
                    result = self.invoke(script, destination, f'http://127.0.0.1:{server.server_port}/')
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(destination.read_bytes(), b'complete installer bytes')
                    self.assertFalse(destination.with_suffix('.tmp').exists())
                    self.assertEqual(Handler.calls, 2)
                finally:
                    server.shutdown()
                    server.server_close()
                    thread.join()

    def test_truncated_response_is_not_cached_or_promoted(self):
        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                self.send_header('Content-Length', '999')
                self.end_headers()
                self.wfile.write(b'partial installer')
                self.close_connection = True
            def log_message(self, *_):
                pass
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as tmp:
                for script in SCRIPTS:
                    destination = Path(tmp) / 'installer'
                    result = self.invoke(script, destination, f'http://127.0.0.1:{server.server_port}/')
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(destination.exists())
                    self.assertFalse(destination.with_suffix('.tmp').exists())
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == '__main__':
    unittest.main()
