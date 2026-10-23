#!/usr/bin/env python3.11
"""Prepare both inference variants once; distribute only signed release images."""
import argparse
import json
import os
from pathlib import Path
import shutil

from cloud_case import Case, release_selection, run, write_json
from publish_fixtures import cleanup as cleanup_publications, publish_variant, resume_producer, verify_resumable_build

MODEL = "Qwen3.8-27B"


def external_fixtures():
    # Preserve the former combined export directory for existing clean runners.
    return os.environ.get("E2E_INFERENCE_FIXTURES") or os.environ.get("E2E_PUBLISHED_FIXTURES")


def cleanup(case):
    try:
        cleanup_publications(case)
        state_path = case.state / "services/vllm-inference/state.json"
        if state_path.exists():
            state = json.loads(state_path.read_text())
            if state["phase"] not in ["built", "deleted"]:
                raise RuntimeError("debug deployment must be cleaned before its local build is removed")
            case.command("remove-local-images", "image", "rm", "vllm-inference", "--force", timeout=600)
    except BaseException:
        path = case.work / "case-result.json"
        result = json.loads(path.read_text()) if path.exists() else {"passed": False}
        result.update(cleanup_complete=False)
        write_json(path, result)
        raise


def debug_build(case):
    """Only the local debug case can deploy the producer's original keyed build."""
    producer = case.suite / "prepare-inference-images"
    if external_fixtures():
        raise RuntimeError("debug validation requires its original local producer, not exported release fixtures")
    case.state = producer / "state"
    spec = producer / "init/vllm-inference/vllm-inference.yaml"
    if not verify_resumable_build(case, "vllm-inference", spec, ["debug"]):
        raise RuntimeError("prepared debug build is missing")
    return "vllm-inference", spec


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    operation = parser.add_mutually_exclusive_group()
    operation.add_argument("--cleanup", action="store_true")
    operation.add_argument("--resume", action="store_true", help="Reverify retained builds and retry publication without rebuilding")
    args = parser.parse_args()
    case = Case()
    external = external_fixtures()
    if args.resume:
        if external:
            raise RuntimeError("external signed fixtures cannot resume a producer build")
        resume_producer(case)
    if args.cleanup:
        if external:
            raise RuntimeError("external consumers do not own inference publication cleanup")
        cleanup(case)
        return
    if external:
        with case:
            source = Path(external) / "inference-release"
            release_selection(source, case.region)
            destination = case.work / "inference-release"
            destination.mkdir(mode=0o700)
            for filename in ["image-release.json", "publisher.pem"]:
                shutil.copyfile(source / filename, destination / filename)
            case.checks.update(external_signed_fixtures=True, producer_artifacts_copied=False)
        return
    case.defer_cleanup = True
    try:
        with case:
            key, bucket = os.environ.get("E2E_MODEL_KEY_FILE"), os.environ.get("E2E_MODEL_OSS_BUCKET")
            if not key or not bucket:
                raise RuntimeError("encrypted model fixture and key are required")
            for arguments, label in ([] if args.resume else [
                (["genpkey", "-algorithm", "ED25519", "-out", case.work / "publisher.key"], "publisher-key"),
                (["pkey", "-in", case.work / "publisher.key", "-pubout", "-out", case.work / "publisher.pem"], "publisher-public"),
            ]):
                run(["openssl", *arguments], case.work / label)
            cosign = case.work / "cosign.key"
            if not args.resume:
                case.command("cosign-key", "key", "generate-cosign", "--output-key-prefix", cosign.with_suffix(""))
                case.command("init", "init", "vllm-inference", "--non-interactive", "--output-dir", case.work / "init",
                             "--region", case.region, "--zone-id", os.environ.get("E2E_GPU_ZONE_ID", "cn-beijing-l"),
                             "--vllm-build-variants", "release,debug", "--model-key-file", key,
                             "--oss-bucket", bucket, "--reference-values", "rekor", "--cosign-key", cosign,
                             "--vllm-served-model-name", MODEL)
            spec = case.work / "init/vllm-inference/vllm-inference.yaml"
            if not (args.resume and verify_resumable_build(case, "vllm-inference", spec, ["release", "debug"])):
                case.command("build", "build", "--spec", spec, timeout=28800)
                if not verify_resumable_build(case, "vllm-inference", spec, ["release", "debug"]):
                    raise RuntimeError("inference build did not produce both variants")
            run(["aliyun", "--region", case.region, "sts", "GetCallerIdentity"],
                case.work / "caller-identity")
            ledger = case.work / "published-images.json"
            records = json.loads(ledger.read_text()) if args.resume and ledger.exists() else []
            publish_variant(case, "vllm-inference", spec, "release", case.work / "inference-release",
                            records)
            case.checks.update(both_variants_verified=True, signed_release_published=True,
                               debug_credentials_distributed=False)
    except BaseException:
        try:
            # Publication/network failure should not discard the two expensive
            # local builds. Explicit --resume rechecks them before any reuse.
            cleanup_publications(case)
        except Exception:
            pass
        raise


if __name__ == "__main__":
    main()
