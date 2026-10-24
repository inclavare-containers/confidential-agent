"""Retain complete read-only metrics probes under one wall-clock budget."""
import errno
import hashlib
import http.client
import json
from pathlib import Path
import subprocess
import sys
import time
import urllib.error
import urllib.request


def retryable_transport_error(error):
    if isinstance(error, http.client.IncompleteRead):
        return True
    reason = error.reason if isinstance(error, urllib.error.URLError) else error
    return isinstance(reason, OSError) and reason.errno == errno.ECONNRESET


def fetch_once(base, deadline):
    """Child process: the parent enforces the deadline even on a trickling body."""
    body = bytearray()
    result = {"method": "GET", "path": "/metrics"}
    code = 1
    try:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("metrics deadline reached before request")
        request = urllib.request.Request(base.rstrip("/") + "/metrics",
            headers={"Content-Type": "application/json", "Connection": "close"})
        with urllib.request.urlopen(request, timeout=remaining) as response:
            result.update(http_status=response.status,
                          content_length=response.headers.get("Content-Length"),
                          content_type=response.headers.get("Content-Type"))
            expected = result["content_length"]
            expected = int(expected) if expected is not None else None
            while True:
                chunk = response.read1(65536)
                if not chunk:
                    break
                body.extend(chunk)
                sys.stdout.buffer.write(chunk)
                sys.stdout.buffer.flush()
            if expected is not None and len(body) != expected:
                raise http.client.IncompleteRead(b"", max(0, expected - len(body)))
        if time.monotonic() >= deadline:
            raise TimeoutError("metrics deadline reached while reading response")
        result.update(complete=True, retryable=False)
        code = 0
    except Exception as error:
        if isinstance(error, urllib.error.HTTPError):
            result["http_status"] = error.code
        if isinstance(error, http.client.IncompleteRead):
            body.extend(error.partial)
            sys.stdout.buffer.write(error.partial)
            sys.stdout.buffer.flush()
            result["expected_additional_bytes"] = error.expected
        result.update(complete=False, retryable=retryable_transport_error(error),
                      error_type=type(error).__name__, error=str(error))
        code = 2 if result["retryable"] else 1
    result.update(bytes=len(body), sha256=hashlib.sha256(body).hexdigest())
    sys.stderr.write(json.dumps(result) + "\n")
    return code


def complete_metrics(base, evidence, timeout=180):
    """Retry only truncation/reset, at most three GETs within the same deadline."""
    evidence = Path(evidence)
    evidence.parent.mkdir(parents=True, exist_ok=True)
    deadline = time.monotonic() + timeout
    last_error = "metrics deadline exhausted"
    for attempt in range(1, 4):
        if time.monotonic() >= deadline:
            break
        started = time.monotonic()
        timed_out = False
        command = [sys.executable, str(Path(__file__).resolve()), "--fetch", base, str(deadline)]
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            stdout, stderr = process.communicate(timeout=max(0, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            timed_out = True
            process.kill()
            stdout, stderr = process.communicate()
        except BaseException:
            if process.poll() is None:
                process.kill()
            process.communicate()
            raise
        prefix = evidence.with_name(evidence.name + f"-attempt-{attempt}")
        for suffix, data in [(".body", stdout), (".stderr", stderr)]:
            with Path(str(prefix) + suffix).open("xb") as output:
                output.write(data)
        result = {"attempt": attempt, "endpoint": base, "evidence_label": evidence.name,
                  "elapsed_seconds": time.monotonic() - started, "exit_code": process.returncode,
                  "timed_out": timed_out, "bytes": len(stdout),
                  "body_sha256": hashlib.sha256(stdout).hexdigest(),
                  "remaining_budget_seconds": max(0, deadline - time.monotonic())}
        if not timed_out:
            try:
                result["response"] = json.loads(stderr)
            except (ValueError, UnicodeDecodeError):
                result["response"] = {"complete": False, "retryable": False,
                                      "error": "metrics worker returned invalid diagnostics"}
        with Path(str(prefix) + ".result.json").open("x") as output:
            json.dump(result, output, indent=2)
            output.write("\n")
        response = result.get("response", {})
        if (not timed_out and process.returncode == 0 and response.get("complete") is True
                and response.get("sha256") == result["body_sha256"]
                and response.get("bytes") == len(stdout) and time.monotonic() < deadline):
            return stdout
        last_error = response.get("error", "metrics deadline exhausted")
        if timed_out or process.returncode != 2 or response.get("retryable") is not True:
            break
    raise RuntimeError(f"complete metrics response unavailable: {last_error}; evidence: {evidence}")


if __name__ == "__main__":
    if len(sys.argv) != 4 or sys.argv[1] != "--fetch":
        raise SystemExit("usage: metrics_probe.py --fetch BASE_URL MONOTONIC_DEADLINE")
    raise SystemExit(fetch_once(sys.argv[2], float(sys.argv[3])))
