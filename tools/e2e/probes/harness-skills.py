#!/usr/bin/env python3.11
"""Check native skill discovery without issuing a model request."""
import json
from pathlib import Path
import queue
import subprocess
import threading

NAME = "tdx-remote-attestation"


def claude_skills():
    # Claude's SDK initialize response lists available slash commands/skills.
    server = subprocess.Popen(
        ["cai-agent", "-p", "--input-format", "stream-json",
         "--output-format", "stream-json", "--verbose"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        text=True, bufsize=1,
    )
    messages = queue.Queue()

    def read():
        for line in server.stdout:
            try:
                messages.put(json.loads(line))
            except ValueError:
                continue
        messages.put(None)

    threading.Thread(target=read, daemon=True).start()
    try:
        server.stdin.write(json.dumps({
            "type": "control_request", "request_id": "tdx-skill-probe",
            "request": {"subtype": "initialize"},
        }) + "\n")
        server.stdin.flush()
        while True:
            response = messages.get(timeout=30)
            if response is None:
                raise RuntimeError("Claude exited before skill discovery")
            if response.get("type") == "control_response":
                assert NAME in json.dumps(response), "Claude did not discover its TDX skill"
                return
    finally:
        server.terminate()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait()


def main():
    harness = json.loads(Path("/opt/data/allinone/runtime.json").read_text())["harness"]
    homes = {
        "claude-code": "home/.claude", "qodercli": "home/.qoder",
        "opencode": "home/.config/opencode", "hermes": "hermes", "openclaw": "home/.openclaw",
        "pi": "home/.pi/agent", "dsh": "home/.dsh",
    }
    skill = Path("/opt/data/allinone") / homes[harness] / "skills" / NAME / "SKILL.md"
    assert skill.is_file(), "TDX skill is absent from the selected harness home"
    if harness == "claude-code":
        claude_skills()
    elif harness == "pi":
        result = subprocess.run(["cai-agent", "--mode", "rpc", "--no-session"],
            input=json.dumps({"id": "skills", "type": "get_commands"}) + "\n",
            capture_output=True, text=True, check=True, timeout=60)
        response = next(json.loads(line) for line in result.stdout.splitlines()
                        if json.loads(line).get("id") == "skills")
        assert response["success"] and str(skill) in json.dumps(response["data"]), \
            "Pi did not discover its TDX skill"
    elif harness == "dsh":
        # Use the filesystem provider shipped with the pinned DSH package.
        script = """
import { createRequire } from 'node:module';
import { execFileSync } from 'node:child_process';
const root = execFileSync('npm', ['root', '-g'], {encoding:'utf8'}).trim();
const require = createRequire(root + '/@deepseek-ai/dsh/package.json');
const {Context} = await import(require.resolve('@deepseek-ai/cordis'));
const {default: skills} = await import(require.resolve('@deepseek-ai/dsh-skill'));
const filesystem = await import(require.resolve('@deepseek-ai/dsh-skill-filesystem'));
const ctx = new Context();
await ctx.plugin(skills).await();
await ctx.plugin(filesystem, {
    dshHome:'/opt/data/allinone/home/.dsh',
    agentsHome:'/opt/data/allinone/home/.agents'
}).await();
console.log(JSON.stringify(await ctx.skills.list({cwd:'/opt/data/allinone/workspace'})));
await ctx.fiber.dispose();
"""
        result = subprocess.run(["node", "--input-type=module", "-e", script],
                                capture_output=True, text=True, check=True, timeout=60)
        assert any(row["name"] == NAME and row["path"] == str(skill)
                   for row in json.loads(result.stdout)), "DSH did not discover its TDX skill"
    else:
        args = {
            "qodercli": ["skills", "list"], "opencode": ["debug", "skill"],
            "hermes": ["skills", "list"], "openclaw": ["skills", "list", "--json"],
        }[harness]
        result = subprocess.run(["cai-agent", *args], capture_output=True, text=True, timeout=60)
        assert result.returncode == 0, "Native skill discovery failed"
        assert NAME in result.stdout, "Harness did not discover its TDX skill"
    if harness == "openclaw":
        subprocess.run(["systemctl", "is-active", "--quiet", "cai-pep.service"], check=True)
        config = json.loads((skill.parents[2] / "openclaw.json").read_text())
        assert config["plugins"]["entries"]["cai-pep"]["config"]["pepRequired"] is True
    print(json.dumps({"harness": harness, "tdx_skill_discovered": True, "path": str(skill)}))


if __name__ == "__main__":
    main()
