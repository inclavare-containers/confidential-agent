"""Generate private native harness fixtures; credentials come only from the caller."""
import json
import os
from pathlib import Path
import secrets
import shlex
import shutil

HARNESSES = ["codex", "claude-code", "opencode", "openclaw", "hermes", "qodercli"]
PROMPT = "Reply with ALLINONE_, E2E_, and OK concatenated without spaces or other text."


def generate(directory, image, mode, region, prefix, *, harnesses=None):
    os.umask(0o077)
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=False)
    key = os.environ.get("DASHSCOPE_API_KEY") or os.environ.get("BAILIAN_API_KEY")
    if not key:
        raise RuntimeError("Bailian credential is required")
    url = os.environ.get("DASHSCOPE_BASE_URL", "https://dashscope.aliyuncs.com/compatible-mode/v1")
    model = os.environ.get("DASHSCOPE_MODEL", "qwen3.8-max")
    fixtures = []
    for harness in HARNESSES if harnesses is None else harnesses:
        if harness not in HARNESSES:
            raise ValueError("unknown harness: " + harness)
        target = directory / harness
        target.mkdir(mode=0o700)
        resources, environment = {}, {}
        def resource(name, content, guest, mutable=False):
            source = target / name
            source.write_text(content if isinstance(content, str) else json.dumps(content))
            resources[name.replace(".", "_")] = {"source": str(source), "target": guest,
                                                   "mode": "0600", "mutable": mutable}
        runtime = None
        if harness == "codex":
            environment["OPENAI_API_KEY"] = key
            resource("config.toml", f'model_provider = "Model_Studio"\nmodel = "{model}"\n[model_providers.Model_Studio]\nname = "Model_Studio"\nbase_url = "{url}"\nenv_key = "OPENAI_API_KEY"\nwire_api = "responses"\n', "/opt/data/allinone/home/.codex/config.toml")
            command = "cai-agent --ask-for-approval never exec --skip-git-repo-check --sandbox danger-full-access --json " + shlex.quote(PROMPT)
        elif harness == "claude-code":
            resource("settings.json", {"env": {"ANTHROPIC_AUTH_TOKEN": key,
                     "ANTHROPIC_BASE_URL": os.environ.get("DASHSCOPE_ANTHROPIC_BASE_URL", "https://dashscope.aliyuncs.com/apps/anthropic"),
                     "ANTHROPIC_MODEL": model, "ANTHROPIC_DEFAULT_HAIKU_MODEL": model,
                     "ANTHROPIC_DEFAULT_SONNET_MODEL": model, "ANTHROPIC_DEFAULT_OPUS_MODEL": model,
                     "CLAUDE_CODE_SUBAGENT_MODEL": model, "DISABLE_AUTOUPDATER": "1"}}, "/opt/data/allinone/home/.claude/settings.json")
            resource("claude.json", {"hasCompletedOnboarding": True}, "/opt/data/allinone/home/.claude.json")
            command = "cai-agent --print " + shlex.quote(PROMPT) + " --max-turns 2 --output-format text"
        elif harness == "opencode":
            resource("opencode.json", {"$schema": "https://opencode.ai/config.json", "autoupdate": False,
                     "model": "bailian/"+model, "provider": {"bailian": {"npm": "@ai-sdk/openai-compatible", "name": "Bailian",
                     "options": {"baseURL": url, "apiKey": key}, "models": {model: {"name": model}}}}}, "/opt/data/allinone/home/.config/opencode/opencode.json")
            command = "cai-agent run --format json " + shlex.quote(PROMPT)
        elif harness == "openclaw":
            resource("openclaw.json", {"models": {"mode": "merge", "providers": {"bailian": {"baseUrl": url, "apiKey": key,
                     "api": "openai-completions", "models": [{"id": model, "name": model, "reasoning": False,
                     "input": ["text"], "contextWindow": 1000000, "maxTokens": 131072}]}}},
                     "agents": {"defaults": {"model": {"primary": "bailian/"+model}, "workspace": "/opt/data/allinone/workspace"}},
                     "tools": {"profile": "full"},
                     "plugins": {"enabled": True, "allow": ["cai-pep", "cai-a2a"], "entries": {
                         "cai-pep": {"enabled": True, "config": {"socketPath": "/run/cai/pep.sock",
                             "pepRequired": True, "defaultWorkdir": "/opt/data/allinone/workspace"}},
                         "cai-a2a": {"enabled": True, "config": {"peers": {}}}}},
                     "gateway": {"mode": "local", "bind": "loopback", "port": 18789,
                     "auth": {"mode": "token", "token": secrets.token_hex(32)}}}, "/opt/data/allinone/home/.openclaw/openclaw.json")
            runtime = "cai-agent health --json"
            command = "cai-agent agent --agent main --message " + shlex.quote(PROMPT) + " --json"
        elif harness == "hermes":
            environment = {"DASHSCOPE_API_KEY": key, "DASHSCOPE_BASE_URL": url, "HERMES_MODEL": model,
                           "API_SERVER_ENABLED": "true", "API_SERVER_HOST": "0.0.0.0", "API_SERVER_PORT": "8642",
                           "API_SERVER_KEY": secrets.token_hex(32)}
            resource("config.yaml", f"model:\n  provider: alibaba\n  default: {model}\n  model: {model}\n", "/opt/data/allinone/hermes/config.yaml")
            runtime = "python3.11 -c 'import urllib.request; assert urllib.request.urlopen(\"http://127.0.0.1:8642/health\",timeout=20).status == 200'"
            command = "cai-agent chat -q " + shlex.quote(PROMPT)
        else:
            auth = Path(os.environ.get("E2E_QODER_NATIVE_CONFIG", "/missing"))
            if os.environ.get("QODER_PERSONAL_ACCESS_TOKEN"):
                environment["QODER_PERSONAL_ACCESS_TOKEN"] = os.environ["QODER_PERSONAL_ACCESS_TOKEN"]
            elif (auth / ".auth/user").is_file():
                for name in ["user", "machine_id"]:
                    source = target / ("qoder-"+name)
                    shutil.copyfile(auth / ".auth" / name, source)
                    resources["qoder_native_"+name] = {"source": str(source), "target": "/opt/data/allinone/home/.qoder/.auth/"+name,
                                                         "mode": "0600", "mutable": name == "user"}
            else:
                raise RuntimeError("Qoder account credential is required for a complete model probe")
            # BYOK entries belong to Qoder's account-specific wizard. Preserve
            # its credential fields and select the configured model explicitly.
            settings = json.loads((auth / "settings.json").read_text()) if (auth / "settings.json").is_file() else {}
            selected = os.environ.get("E2E_QODER_MODEL") or (settings.get("model") or {}).get("name")
            selected = selected.strip() if isinstance(selected, str) else selected
            if not isinstance(selected, str) or not selected.strip() or selected.lower() in {
                "auto", "default", "ultimate", "performance", "fast", "efficient", "lite"}:
                raise RuntimeError("Qoder requires a fixed model selected through /model or E2E_QODER_MODEL; automatic routing is not a model acceptance test")
            # Qoder 1.1.70 migrates this legacy field even for --version. Emit
            # its current schema before CA hashes the immutable resource.
            # An existing per-model preference takes precedence, as in Qoder.
            native_model = settings.get("model") or {}
            native_name = native_model.get("name")
            if isinstance(native_name, str) and native_name and "contextWindow" in native_model:
                context_window = native_model.pop("contextWindow")
                native_model.setdefault("preferences", {}).setdefault(native_name, {}).setdefault(
                    "contextWindow", context_window)
            settings.setdefault("general", {})["enableAutoUpdate"] = False
            resource("qoder-settings.json", settings, "/opt/data/allinone/home/.qoder/settings.json")
            command = "cai-agent --model " + shlex.quote(selected) + " -p " + shlex.quote(PROMPT)
        resource("runtime.json", {"harness": harness, "environment": environment}, "/opt/data/allinone/runtime.json")
        # Consumers authenticate Rekor metadata from the signed release; they
        # do not enable build-time signing or carry a producer's cosign key.
        spec = {"schema": "confidential-agent/v1", "service": {"id": prefix+"-"+harness, "ports": [], "ssh": True,
                "app_service": "cai-allinone.service"}, "deploy": {"provider": "aliyun", "region": region,
                "zone_id": os.environ.get("E2E_ZONE_ID", "cn-beijing-i"), "instance_type": os.environ.get("E2E_INSTANCE_TYPE", "ecs.g9i.xlarge"),
                "disk_gb": 120, "image": image}, "attestation": {"tee": "tdx", "mode": mode,
                "reference_values": "rekor", "rekor": {
                    "slsa_generator": "/usr/libexec/shelter/slsa/slsa-generator"}}, "resources": resources}
        if harness == "openclaw":
            spec["service"].update(ports=[18789], connect=[])
        elif harness == "hermes":
            spec["service"].update(ports=[8642], connect=[8642])
        (target / "appspec.json").write_text(json.dumps(spec, indent=2)+"\n")
        (target / "probe.json").write_text(json.dumps({"command": command, "expect": "ALLINONE_E2E_OK",
                 "runtime_command": runtime, "connect_http_path": "/health" if harness == "hermes" else None})+"\n")
        fixtures.append(target)
    return fixtures
