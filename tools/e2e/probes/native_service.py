"""Check the native CPU services at the endpoint assigned by ca connect."""
import base64
from http.cookiejar import CookieJar
import json
from pathlib import Path
import urllib.error
import urllib.parse
import urllib.request
import uuid


def require_auth(request):
    try:
        urllib.request.urlopen(request, timeout=20).close()
    except urllib.error.HTTPError as error:
        assert error.code in (401, 403), f"unexpected unauthenticated response: {error.code}"
    else:
        raise AssertionError("service accepted an unauthenticated request")


def opencode(base, settings, expected):
    require_auth(base + "/global/health")
    credentials = base64.b64encode(("opencode:" + settings["password"]).encode()).decode()

    def request(path, body=None):
        req = urllib.request.Request(base + path,
            data=json.dumps(body).encode() if body is not None else None,
            headers={"Authorization": "Basic " + credentials, "Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=600) as response:
            return json.load(response)

    assert request("/global/health")["healthy"]
    session = request("/session", {})
    reply = request("/session/" + session["id"] + "/message",
                    {"parts": [{"type": "text", "text": settings["message"]}]})
    text = "\n".join(part["text"] for part in reply["parts"] if part["type"] == "text")
    assert expected in text, "OpenCode web response missing model marker"
    return text


def dsh(link, prompt, expected, workspace="/opt/data/allinone/workspace"):
    base = urllib.parse.urlsplit(link)._replace(path="", query="", fragment="").geturl()
    require_auth(base)
    client = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(CookieJar()))
    with client.open(link, timeout=30) as response:
        assert response.status == 200 and b"<html" in response.read().lower(), "DSH web login failed"

    def rpc(method, arguments):
        rpc_id = str(uuid.uuid4())
        body = {"type": "client-request", "rpcId": rpc_id, "method": method,
                "payload": {"args": {"request": arguments}}}
        req = urllib.request.Request(base + "/api/" + method, data=json.dumps(body).encode(),
                                     headers={"Content-Type": "application/json", "Origin": base})
        with client.open(req, timeout=600) as response:
            envelope = json.load(response)
        assert envelope["rpcId"] == rpc_id
        result = envelope["result"]
        assert result["ok"], f"DSH {method} failed: {result['error']['code']}"
        return result["value"]

    session = rpc("session/create", {"cwd": workspace})["sessionId"]
    receipt = rpc("session/prompt", {"sessionId": session, "requestId": str(uuid.uuid4()),
                  "mode": "queue", "content": [{"type": "text", "text": prompt}]})
    assert receipt["accepted"]
    reply = rpc("session/wait", {"sessionId": session, "timeoutMs": 540000})
    assert reply["reason"] == "idle"
    records = [record["event"]["data"] for record in reply["output"]["records"]
               if record["event"]["type"] == "assistant/message"]
    text = "\n".join(part["text"] for record in records for part in record["message"]["content"]
                     if part["type"] == "text")
    assert expected in text, "DSH web response missing model marker"
    return text


def connected(consumer, endpoint, run, label):
    settings = consumer.probe["native_service"]
    harness = settings["harness"]
    base = endpoint["http_base_url"]
    expected = consumer.probe["expect"]
    probes = Path(__file__).parent
    if harness == "codex":
        run(["python3.11", probes / "codex-app-server.py", "--port", str(endpoint["local_port"]),
             "--token-file", settings["token_file"], "--prompt", settings["message"], "--expect", expected],
            consumer.work / label, timeout=660)
    elif harness in ("openclaw", "hermes"):
        if harness == "openclaw":
            require_auth(urllib.request.Request(base + "/v1/responses", data=b"{}",
                         headers={"Content-Type": "application/json"}))
        else:
            require_auth(base + "/v1/models")
        args = ["node", probes / (harness + "-chat-probe.mjs"), "--url", base,
                "--token", settings["token"], "--message", settings["message"], "--expect", expected,
                "--timeout-ms", "600000"]
        if harness == "hermes":
            args += ["--model", settings["model"]]
        run(args, consumer.work / label, timeout=660)
    elif harness == "opencode":
        (consumer.work / (label + ".out")).write_text(opencode(base, settings, expected))
    elif harness == "dsh":
        port = endpoint["local_port"]
        link = consumer.ssh(label + "-dsh-web-login", f"cai-agent --web-url --port {port}").stdout.decode().strip()
        assert urllib.parse.urlsplit(link).netloc == urllib.parse.urlsplit(base).netloc
        (consumer.work / (label + ".out")).write_text(dsh(link, settings["message"], expected))
    else:
        raise ValueError("unknown native service: " + harness)
