"""Stage an independent CPU build/deploy example with private E2E resources."""
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import sys

from render_case import ROOT, PINS, render_rekor_block, yaml_quote


def stage(example, target):
    env = os.environ
    source = ROOT / "examples" / example
    shutil.copytree(source, target, dirs_exist_ok=True)
    spec = target / (example + ".yaml")
    contents = spec.read_text().replace("../cli-agents/files/", str(ROOT / "examples/cli-agents/files") + "/")
    contents = contents.replace("source: /usr/bin/cai-pep", "source: " + yaml_quote(env["CA_PEP_BIN"]))
    for field, value in [("region", env["REGION"]), ("zone_id", env["ZONE_ID"]),
                         ("instance_type", env["INSTANCE_TYPE"]), ("disk_gb", int(env["DISK_GB"]))]:
        contents = re.sub(rf"(?m)^  {field}: .*", lambda _: f"  {field}: {json.dumps(value)}", contents)
    reference = env["REFERENCE_VALUES"]
    rekor = render_rekor_block(env)
    contents = re.sub(r"(?m)^  reference_values: .*\n(?:  rekor:\n(?:    .*\n)*)?",
                      lambda _: f"  reference_values: {reference}\n" + (rekor + "\n" if rekor else ""), contents)

    key = env["DASHSCOPE_API_KEY"]
    model = env.get("DASHSCOPE_MODEL", PINS["bailian_model"])
    url = env.get("DASHSCOPE_BASE_URL", "https://dashscope.aliyuncs.com/compatible-mode/v1")
    token = env.get("E2E_SERVICE_TOKEN")
    exports = {}
    files = {}
    if example == "openclaw":
        config = json.loads((source / "openclaw.json").read_text())
        provider = config["models"]["providers"]["bailian"]
        model = model.removeprefix("bailian/")
        provider.update(apiKey=key, baseUrl=url)
        provider["models"][0].update(id=model, name=model)
        config["agents"]["defaults"]["model"]["primary"] = "bailian/" + model
        config["gateway"]["auth"]["token"] = token
        config["tools"] = {"profile": "full"}
        config["channels"].pop("dingtalk")
        plugins = config["plugins"]
        plugins["allow"].remove("dingtalk")
        del plugins["entries"]["dingtalk"]
        no_pep = env.get("E2E_OPENCLAW_DISABLE_PEP", "0")
        if no_pep == "1":
            plugins["allow"].remove("cai-pep")
            del plugins["entries"]["cai-pep"]
            contents = re.sub(r"(?m)^    - source: [^\n]*\n      target: ([^\n]*)\n(?:      executable: true\n)?",
                              lambda entry: "" if "pep" in entry[1] or "tdx-remote-attestation" in entry[1] else entry[0],
                              contents)
        files["openclaw.json"] = json.dumps(config, indent=2)
        exports.update(OPENCLAW_VERSION=env.get("E2E_OPENCLAW_VERSION", PINS["openclaw"]),
                       OPENCLAW_NODE_VERSION=env.get("E2E_OPENCLAW_NODE_VERSION", PINS["node"]),
                       NPM_REGISTRY=env.get("E2E_NPM_REGISTRY", "https://registry.npmmirror.com/"),
                       CA_DISABLE_PEP=no_pep)
    elif example == "hermes-agent":
        exports.update(HERMES_BRANCH=env.get("HERMES_BRANCH", "main"), HERMES_COMMIT=env.get("HERMES_COMMIT", ""))
        files["secrets/hermes.env"] = (f"API_SERVER_ENABLED=true\nAPI_SERVER_HOST=0.0.0.0\nAPI_SERVER_PORT=8642\n"
            f"API_SERVER_KEY={token}\nDASHSCOPE_API_KEY={key}\nDASHSCOPE_BASE_URL={url}\nHERMES_HOME=/opt/data\nHERMES_MODEL={model}")
        files["secrets/config.yaml"] = f"model:\n  provider: alibaba\n  default: {yaml_quote(model)}\n  model: {yaml_quote(model)}"
    else:
        exports.update(CLI_AGENT_NODE_VERSION=env.get("E2E_CLI_AGENT_NODE_VERSION", PINS["node"]),
                       NPM_REGISTRY=env.get("E2E_CLI_AGENT_NPM_REGISTRY") or env.get("E2E_NPM_REGISTRY", "https://registry.npmmirror.com/"))
        if example == "codex":
            exports["CODEX_VERSION"] = env.get("E2E_CODEX_VERSION", PINS["codex"])
            files["secrets/config.toml"] = (f'model_provider = "Model_Studio"\nmodel = {json.dumps(model)}\n'
                f'[model_providers.Model_Studio]\nname = "Model_Studio"\nbase_url = {json.dumps(url)}\n'
                'env_key = "OPENAI_API_KEY"\nwire_api = "responses"')
            files["secrets/codex.env"] = f"OPENAI_API_KEY={shlex.quote(key)}\nCODEX_HOME=/root/.codex"
            files["secrets/app-server-token"] = token
        elif example == "claude-code":
            exports["CLAUDE_CODE_VERSION"] = env.get("E2E_CLAUDE_CODE_VERSION", PINS["claude-code"])
            settings = dict(ANTHROPIC_AUTH_TOKEN=key, ANTHROPIC_BASE_URL=env.get(
                "DASHSCOPE_ANTHROPIC_BASE_URL", "https://dashscope.aliyuncs.com/apps/anthropic"))
            for field in ("ANTHROPIC_MODEL", "ANTHROPIC_DEFAULT_HAIKU_MODEL", "ANTHROPIC_DEFAULT_SONNET_MODEL",
                          "ANTHROPIC_DEFAULT_OPUS_MODEL", "CLAUDE_CODE_SUBAGENT_MODEL"):
                settings[field] = model
            files["secrets/settings.json"] = json.dumps({"env": settings}, indent=2)
            files["secrets/claude.json"] = json.dumps({"hasCompletedOnboarding": True})
        else:
            raise ValueError("unknown independent CPU example: " + example)
    spec.write_text(contents)
    script = target / ("install-" + example + ".sh")
    original = script.read_text().splitlines(keepends=True)
    # Values must take effect in the image builder's isolated environment.
    assignments = [f"export {name}={shlex.quote(value)}\n" for name, value in exports.items()]
    script.write_text("".join(original[:1] + assignments + [
        line for line in original[1:] if not any(line.startswith(f"export {name}=") for name in exports)]))
    for name, content in files.items():
        path = target / name
        path.parent.mkdir(mode=0o700, exist_ok=True)
        path.touch(mode=0o600)
        path.chmod(0o600)
        path.write_text(content + "\n")


if __name__ == "__main__":
    os.umask(0o077)
    stage(sys.argv[1], Path(sys.argv[2]))
