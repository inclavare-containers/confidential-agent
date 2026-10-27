#!/usr/bin/env python3.11
"""Render E2E case fixtures from checked-in templates."""

import argparse
import base64
import json
import os
import re
import shutil
from pathlib import Path
from urllib.parse import urlsplit
from rekor_fixture import rekor_url


ROOT = Path(__file__).resolve().parents[2]
CASES = ROOT / "tools" / "e2e" / "cases"
PINS = json.loads((ROOT / "software-versions.json").read_text())


def yaml_quote(value: str) -> str:
    if "\n" in value or "\r" in value:
        raise SystemExit("YAML scalar values must not contain newlines")
    return "'" + value.replace("'", "''") + "'"


def render_rekor_block(context):
    if context["REFERENCE_VALUES"] != "rekor":
        return ""
    cosign_key = context.get("COSIGN_KEY", "")
    if not cosign_key:
        raise SystemExit("COSIGN_KEY is required when REFERENCE_VALUES=rekor")
    return "\n".join(
        [
            "  rekor:",
            f"    rekor_url: {yaml_quote(rekor_url(context.get('E2E_REKOR_URL')))}",
            f"    cosign_key: {yaml_quote(cosign_key)}",
            f"    slsa_generator: {yaml_quote(context['SLSA_GENERATOR'])}",
            "    required: true",
        ]
    )




def render_matrix_rekor_block(context):
    if context["MATRIX_REAL_REFERENCE_VALUES"] != "rekor":
        return ""
    cosign_key = context.get("MATRIX_REAL_COSIGN_KEY", "")
    if not cosign_key:
        raise SystemExit("MATRIX_REAL_COSIGN_KEY is required when MATRIX_REAL_REFERENCE_VALUES=rekor")
    return "\n".join(
        [
            "  rekor:",
            f"    rekor_url: {yaml_quote(rekor_url(context.get('E2E_REKOR_URL')))}",
            f"    cosign_key: {yaml_quote(cosign_key)}",
            f"    slsa_generator: {yaml_quote(context['MATRIX_REAL_SLSA_GENERATOR'])}",
            "    required: true",
        ]
    )


def matrix_repositories(context):
    mirror = context.get("E2E_MATRIX_REPO_MIRROR", "").rstrip("/")
    if not mirror:
        return []
    parsed = urlsplit(mirror)
    if (parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password
            or parsed.query or parsed.fragment or parsed.port == 0
            or any(character.isspace() for character in mirror)):
        raise ValueError("E2E_MATRIX_REPO_MIRROR must be an HTTPS repository base without credentials or query parameters")
    return [{"name": "alinux-" + name,
             "baseurl": mirror + "/3/" + name + "/$basearch/",
             "gpgkey": mirror + "/3/RPM-GPG-KEY-ALINUX-3"}
            for name in ("os", "updates", "plus", "module", "powertools")]


def repository_yaml(repository, indent=4):
    prefix = " " * indent
    return (prefix + "- name: " + repository["name"] + "\n"
            + prefix + "  baseurl: " + yaml_quote(repository["baseurl"]) + "\n"
            + prefix + "  gpgkey: " + yaml_quote(repository["gpgkey"]) + "\n")


def render_matrix_repositories(context):
    repositories = matrix_repositories(context)
    if not repositories:
        return ""
    lines = ["  repos:"]
    lines.extend(repository_yaml(repository).rstrip("\n") for repository in repositories)
    return "\n".join(lines)


def configure_repositories(path):
    """Apply the signed HTTPS mirror to canonical repositories, preserving extras."""
    repositories = matrix_repositories(os.environ)
    if not repositories:
        return
    path = Path(path)
    original = path.read_text()
    canonical = {repository["name"]: repository for repository in repositories}
    if original.lstrip().startswith("{"):
        document = json.loads(original)
        build = document.setdefault("build", {})
        if not isinstance(build, dict) or not isinstance(build.get("repos", []), list):
            raise ValueError("generated builder requires a build mapping and repository list")
        merged, seen = [], set()
        for repository in build.get("repos", []):
            if not isinstance(repository, dict):
                raise ValueError("generated builder repositories must be mappings")
            name = repository.get("name")
            if name in canonical:
                if name in seen:
                    continue
                repository = canonical[name]
                seen.add(name)
            merged.append(repository)
        merged.extend(repository for name, repository in canonical.items() if name not in seen)
        build["repos"] = merged
        contents = json.dumps(document, indent=2) + "\n"
    else:
        builds = list(re.finditer(r"(?m)^build:[ \t]*(?:#[^\n]*)?\n", original))
        if len(builds) != 1:
            raise ValueError("generated builder requires one block-style build mapping")
        start = builds[0].end()
        boundary = re.search(r"(?m)^[A-Za-z_][^\n]*:", original[start:])
        end = start + boundary.start() if boundary else len(original)
        body = original[start:end]
        headers = list(re.finditer(r"(?m)^  repos:([^\n]*)(?:\n|$)", body))
        if len(headers) > 1:
            raise ValueError("generated builder has duplicate build.repos mappings")
        if not headers:
            body = body.rstrip("\n") + "\n  repos:\n" + "".join(repository_yaml(repo) for repo in repositories)
        else:
            header = headers[0]
            value, _, comment = header.group(1).partition("#")
            if value.strip() not in ("", "[]", "null", "~"):
                raise ValueError("generated builder requires a block-style repository list")
            boundary = re.search(r"(?m)^  [A-Za-z_][^\n]*:", body[header.end():])
            stop = header.end() + boundary.start() if boundary else len(body)
            entries = body[header.end():stop]
            starts = list(re.finditer(r"(?m)^( +)-[ \t]+", entries))
            indent = len(starts[0].group(1)) if starts else 4
            starts = [entry for entry in starts if len(entry.group(1)) == indent]
            merged = entries[:starts[0].start()] if starts else entries
            seen = set()
            for index, entry in enumerate(starts):
                next_start = starts[index + 1].start() if index + 1 < len(starts) else len(entries)
                block = entries[entry.start():next_start]
                name = re.search(r"(?m)^ *-? *name: *(['\"]?)([A-Za-z0-9_.-]+)\1 *(?:#[^\n]*)?$", block)
                name = name.group(2) if name else None
                if name in canonical:
                    if name in seen:
                        continue
                    block = repository_yaml(canonical[name], indent)
                    seen.add(name)
                merged += block
            if merged and not merged.endswith("\n"):
                merged += "\n"
            merged += "".join(repository_yaml(repo, indent) for name, repo in canonical.items() if name not in seen)
            heading = "  repos:" + (" #" + comment if comment else "") + "\n"
            body = body[:header.start()] + heading + merged + body[stop:]
        contents = original[:start] + body + original[end:]
    if contents != original:
        path.write_text(contents)


def decode_jwt_payload(token: str) -> dict:
    parts = token.split(".")
    if len(parts) < 2:
        raise SystemExit("CA_A2A_SIGSTORE_IDENTITY_TOKEN must be a JWT when used to derive signer pins")
    payload = parts[1]
    payload += "=" * (-len(payload) % 4)
    try:
        raw = base64.urlsafe_b64decode(payload.encode("ascii"))
        return json.loads(raw.decode("utf-8"))
    except Exception as exc:
        raise SystemExit(f"failed to decode CA_A2A_SIGSTORE_IDENTITY_TOKEN JWT payload: {exc}") from exc


def apply_a2a_signer_defaults(context):
    if context.get("E2E_A2A_SIGNER_ISSUER") and not context.get("A2A_SIGNER_ISSUER"):
        context["A2A_SIGNER_ISSUER"] = context["E2E_A2A_SIGNER_ISSUER"]
    if context.get("E2E_A2A_SIGNER_SUBJECT") and not context.get("A2A_SIGNER_SUBJECT"):
        context["A2A_SIGNER_SUBJECT"] = context["E2E_A2A_SIGNER_SUBJECT"]

    token = context.get("CA_A2A_SIGSTORE_IDENTITY_TOKEN", "").strip()
    if token and (not context.get("A2A_SIGNER_ISSUER") or not context.get("A2A_SIGNER_SUBJECT")):
        payload = decode_jwt_payload(token)
        if not context.get("A2A_SIGNER_ISSUER"):
            context["A2A_SIGNER_ISSUER"] = str(payload.get("iss") or "")
        if not context.get("A2A_SIGNER_SUBJECT"):
            context["A2A_SIGNER_SUBJECT"] = str(payload.get("sub") or "")

    context.setdefault("A2A_SIGNER_ISSUER", "")
    context.setdefault("A2A_SIGNER_SUBJECT", "")


def a2a_signing_enabled(context):
    raw = str(context.get("E2E_A2A_SIGNING") or context.get("A2A_SIGNING") or "").strip().lower()
    return raw in {"1", "true", "yes", "y", "on", "signed", "sigstore", "keyless"}


def render_a2a_signing_block(context):
    if not a2a_signing_enabled(context):
        return ""
    apply_a2a_signer_defaults(context)
    issuer = context.get("A2A_SIGNER_ISSUER", "").strip()
    subject = context.get("A2A_SIGNER_SUBJECT", "").strip()
    if not issuer or not subject:
        raise SystemExit(
            "E2E_A2A_SIGNING=1 requires A2A_SIGNER_ISSUER/A2A_SIGNER_SUBJECT, "
            "E2E_A2A_SIGNER_*, or a JWT CA_A2A_SIGSTORE_IDENTITY_TOKEN with iss/sub claims"
        )
    return "\n".join(
        [
            "  signing:",
            "    mode: sigstore-keyless",
            "    required: true",
            f"    expected_issuer: {yaml_quote(issuer)}",
            f"    expected_subject: {yaml_quote(subject)}",
        ]
    )


def context_from_env(work_dir):
    context = {key: value for key, value in os.environ.items()}
    def default(name, value):
        if not context.get(name):
            context[name] = value

    def default_zone_id(region):
        if region == "cn-hongkong":
            return "cn-hongkong-d"
        return "cn-beijing-i"

    def default_instance_type(region):
        if region == "cn-hongkong":
            return "ecs.g8i.xlarge"
        if region == "cn-beijing":
            return "ecs.g9i.xlarge"
        return "ecs.g8i.xlarge"

    default("ROOT_DIR", str(ROOT))
    context["WORK_DIR"] = str(work_dir)
    default("REFERENCE_VALUES", "rekor")
    default("SLSA_GENERATOR", "/usr/libexec/shelter/slsa/slsa-generator")
    default("DASHSCOPE_BASE_URL", "https://dashscope.aliyuncs.com/compatible-mode/v1")
    default("DASHSCOPE_ANTHROPIC_BASE_URL", "https://dashscope.aliyuncs.com/apps/anthropic")
    default("DASHSCOPE_MODEL", PINS["bailian_model"])
    default("CLI_AGENT_NODE_VERSION", context.get("E2E_CLI_AGENT_NODE_VERSION", PINS["node"]))
    default(
        "CLI_AGENT_NPM_REGISTRY",
        context.get("E2E_CLI_AGENT_NPM_REGISTRY")
        or context.get("E2E_NPM_REGISTRY")
        or "https://registry.npmmirror.com/",
    )
    default("NPM_REGISTRY", context.get("E2E_NPM_REGISTRY", "https://registry.npmmirror.com/"))
    default("CLAUDE_CODE_VERSION", context.get("E2E_CLAUDE_CODE_VERSION", PINS["claude-code"]))
    default("CODEX_VERSION", context.get("E2E_CODEX_VERSION", PINS["codex"]))
    default("CODEX_REMOTE_PORT", context.get("E2E_CODEX_REMOTE_PORT", "4500"))
    default("HERMES_IMAGE", context.get("E2E_HERMES_IMAGE", "nousresearch/hermes-agent:v2026.6.5"))
    default("HERMES_CONTAINER_RUNTIME", context.get("E2E_HERMES_CONTAINER_RUNTIME", "podman"))
    default("HERMES_API_SERVER_KEY", context.get("E2E_HERMES_API_SERVER_KEY", ""))
    default("REGION", "cn-beijing")
    default("ZONE_ID", default_zone_id(context["REGION"]))
    default("INSTANCE_TYPE", default_instance_type(context["REGION"]))
    default("DISK_GB", "200")
    default("MATRIX_REAL_SERVICE_ID", "matrix")
    default("MATRIX_REAL_REFERENCE_VALUES", context["REFERENCE_VALUES"])
    default("MATRIX_REAL_COSIGN_KEY", context.get("COSIGN_KEY", ""))
    default("MATRIX_REAL_SLSA_GENERATOR", context["SLSA_GENERATOR"])
    default("MATRIX_REAL_REGION", context["REGION"])
    default("MATRIX_REAL_ZONE_ID", context["ZONE_ID"])
    default("MATRIX_REAL_INSTANCE_TYPE", context["INSTANCE_TYPE"])
    default("MATRIX_REAL_DISK_GB", context["DISK_GB"])
    default("MATRIX_REAL_INSTALL_SCRIPT", str(work_dir / "rendered" / "matrix" / "install-matrix-cloud.sh"))
    default("CAI_PEP", context.get("CA_PEP_BIN") or str(ROOT / "target" / "debug" / "cai-pep"))
    context["MATRIX_REAL_REKOR_BLOCK"] = render_matrix_rekor_block(context)
    context["MATRIX_REAL_REPOS_BLOCK"] = render_matrix_repositories(context)
    context["REKOR_BLOCK"] = render_rekor_block(context)
    context["DASHSCOPE_KEY"] = (
        context.get("DASHSCOPE_KEY")
        or context.get("DASHSCOPE_API_KEY")
        or context.get("BAILIAN_API_KEY", "")
    )
    context["HERMES_MODEL"] = context.get("HERMES_MODEL") or context["DASHSCOPE_MODEL"]
    context["A2A_SIGNING_BLOCK"] = ""
    return context


TOKEN_RE = re.compile(r"\{\{(raw|yaml|json):([A-Z0-9_]+)\}\}")


def render_template(text, context):
    def replace(match):
        kind, name = match.groups()
        value = context.get(name, "")
        if kind == "raw":
            return value
        if kind == "yaml":
            return yaml_quote(value)
        if kind == "json":
            return json.dumps(value, ensure_ascii=False)
        raise AssertionError(kind)

    return TOKEN_RE.sub(replace, text)


def copy_asset(root, work_dir, entry):
    source = (root / entry["source"]).resolve()
    target = work_dir / entry["target"]
    if target.exists():
        if target.is_dir():
            shutil.rmtree(target)
        else:
            target.unlink()
    target.parent.mkdir(parents=True, exist_ok=True)
    if source.is_dir():
        shutil.copytree(source, target)
    else:
        shutil.copy2(source, target)


def render(case, work_dir):
    case_dir = CASES / case
    config_path = case_dir / "case.json"
    if not config_path.exists():
        raise SystemExit(f"unknown E2E case: {case}")
    config = json.loads(config_path.read_text(encoding="utf-8"))
    context = context_from_env(work_dir)
    context.update({key: str(value) for key, value in config.get("context", {}).items()})
    context["A2A_SIGNING_BLOCK"] = render_a2a_signing_block(context)

    work_dir.mkdir(parents=True, exist_ok=True)
    for asset in config.get("assets", []):
        copy_asset(ROOT, work_dir, asset)

    for item in config.get("templates", []):
        source = case_dir / item["source"]
        target = work_dir / item["target"]
        target.parent.mkdir(parents=True, exist_ok=True)
        rendered = render_template(source.read_text(encoding="utf-8"), context)
        target.write_text(rendered, encoding="utf-8")
        if item.get("mode"):
            target.chmod(int(item["mode"], 8))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--case", required=True)
    parser.add_argument("--work-dir", required=True, type=Path)
    args = parser.parse_args()
    render(args.case, args.work_dir)


if __name__ == "__main__":
    main()
