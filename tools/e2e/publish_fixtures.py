#!/usr/bin/env python3.11
"""Build and publish the signed images owned by a full E2E run."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import time
from cloud_case import Case, ROOT, release_selection, run, write_json

# Inference has its own producer; all-in-one consumers need only this image.
REQUIRED_PUBLICATIONS = {("allinone-builder", "release")}
SIGNED_FIXTURE_DIRECTORIES = ("allinone",)


def publish_variant(case, service, spec, variant, directory, records):
    """Publish once and durably record ownership before exporting the envelope."""
    case.command("publish-"+service+"-"+variant, "image", "publish", service,
                 "--spec", spec, "--region", case.region, "--variant", variant, timeout=14400)
    state = case.local_state(service)
    entry = next(image for image in state["build"]["published"].values()
                 if image["region"] == case.region and image["variant"] == variant)
    records[:] = [record for record in records
                  if (record.get("region", case.region), record["service"], record["variant"])
                  != (case.region, service, variant)]
    records.append({"service": service, "variant": variant, "region": case.region, "image_id": entry["image_id"],
                    "release_dir": str(directory)})
    write_json(case.work / "published-images.json", records)
    record_publication_snapshots(case)
    directory.mkdir(mode=0o700, exist_ok=True)
    case.command("export-"+service+"-"+variant, "image", "export", service,
                 "--signing-key", case.work / "publisher.key",
                 "--output", directory / "image-release.json", "--region", case.region,
                 "--variant", variant)
    shutil.copyfile(case.work / "publisher.pem", directory / "publisher.pem")


def verify_resumable_build(case, service, spec, variants):
    directory = case.state / "services" / service
    if not (directory / "state.json").exists():
        return False
    state = case.local_state(service)
    if (state["service_id"] != service or state["phase"] not in ["built", "deleted"]
            or Path(state["spec"]["path"]).resolve() != spec.resolve()
            or state["spec"]["sha256"] != hashlib.sha256(spec.read_bytes()).hexdigest()):
        raise RuntimeError("publisher resume requires the original specification and completed local build")
    observed = ("terraform_dir", "instance_id", "instance_ids", "public_ip", "private_ip",
                "instance_public_ips", "instance_private_ips", "security_group_id", "slb_vip")
    if any((state.get("deploy") or {}).get(name) for name in observed):
        raise RuntimeError("publisher resume cannot take over a deployed service")
    manifest = json.loads((directory / "manifest.json").read_text())
    if manifest["service_id"] != service or manifest.get("image"):
        raise RuntimeError("publisher resume requires a local producer manifest")
    artifacts = []
    for variant in variants:
        selected = manifest.get("variants", {}).get(variant)
        if not selected:
            raise RuntimeError("publisher resume is missing a completed build variant: " + variant)
        artifact = Path(selected["build_result"]).with_name("artifact-manifest.json").resolve()
        if not artifact.is_relative_to(case.state.resolve()) or not artifact.is_file():
            raise RuntimeError("publisher resume artifact is missing or outside its owned state")
        artifacts.append((variant, artifact))
    # Rehash and reverify the actual disk, boot sidecars and security contract.
    # A recorded success alone is insufficient to resume a publication.
    for variant, artifact in artifacts:
        run([os.environ.get("CA_SHELTER_BIN", "shelter"), "--work-dir", manifest["shelter_work_dir"],
             "image", "verify", artifact, "--format", "json"],
            case.work / ("resume-verify-" + service + "-" + variant), timeout=7200)
    case.checks.setdefault("resumed_builds_verified", []).append(service)
    return True


def record_publication_snapshots(case):
    ledger = case.work / "cloud-resource-ledger.json"
    entries = json.loads(ledger.read_text()) if ledger.exists() else []
    for path in (case.state / "services").glob("*/state.json"):
        state = json.loads(path.read_text())
        for image in (state.get("build") or {}).get("published", {}).values():
            if not image.get("image_id"):
                continue
            entry = {"type": "alicloud_image_import", "id": image["image_id"],
                     "region": image["region"], "service": state["service_id"]}
            if entry not in entries:
                entries.append(entry)
    write_json(ledger, entries)
    run(["python3.11", ROOT / "tools/e2e/cleanup_snapshots.py", "record", ledger, case.region],
        case.work / "record-publication-snapshots", timeout=600)


def cleanup(case):
    ledger = case.work / "published-images.json"
    records = json.loads(ledger.read_text()) if ledger.exists() else []
    # An import/export can fail before its entry reaches the suite ledger.
    for path in (case.state / "services").glob("*/state.json"):
        state = json.loads(path.read_text())
        for image in (state.get("build") or {}).get("published", {}).values():
            record = {"service": state["service_id"], "variant": image["variant"], "region": image["region"]}
            if not any(item["service"] == record["service"] and item["variant"] == record["variant"]
                       and item.get("region", case.region) == record["region"] for item in records):
                records.append(record)
    errors = []
    try:
        # Preserve exact image/snapshot associations before image deletion makes
        # them unavailable. A failed inventory prevents destructive cleanup.
        record_publication_snapshots(case)
    except RuntimeError as error:
        errors.append(str(error))
    if not errors:
        for record in reversed(records):
            try:
                case.command("unpublish-"+record["service"]+"-"+record["variant"], "image", "unpublish",
                             record["service"], "--region", record.get("region", case.region), "--variant", record["variant"],
                             "--force", timeout=3600)
            except RuntimeError as error:
                errors.append(str(error))
        try:
            run(["python3.11", ROOT / "tools/e2e/cleanup_snapshots.py", "cleanup",
                 case.work / "cloud-resource-ledger.json", case.region],
                case.work / "cleanup-publication-snapshots", timeout=600)
        except RuntimeError as error:
            errors.append(str(error))
    result_path = case.work / "case-result.json"
    result = json.loads(result_path.read_text()) if result_path.exists() else {"passed": False}
    result.update(cleanup_complete=not errors, cleanup_deferred=False, cleanup_errors=errors)
    write_json(case.work / "case-result.json", result)
    if errors:
        raise RuntimeError("publication cleanup failed; private diagnostics retained")


def resume_producer(case):
    previous = json.loads((case.work / "case-result.json").read_text())
    if previous.get("passed") is not False:
        raise RuntimeError("only a failed producer attempt can be resumed")
    for name in ["publisher.key", "publisher.pem", "cosign.key"]:
        if not (case.work / name).is_file():
            raise RuntimeError("publisher resume is missing its original signing material")
    archive = case.work / "attempts" / str(time.time_ns())
    archive.mkdir(parents=True, mode=0o700)
    for path in case.work.iterdir():
        if path.is_file() and (path.suffix in [".out", ".err", ".log"]
                              or path.name.endswith(".timing.json")
                              or path.name in ["case-result.json", "published-images.json"]):
            shutil.copy2(path, archive / path.name)
    case.checks["prior_attempt_evidence"] = str(archive.relative_to(case.work))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    operation = parser.add_mutually_exclusive_group()
    operation.add_argument("--cleanup", action="store_true")
    operation.add_argument("--resume", action="store_true",
                           help="Resume a failed producer in its original work/state directory, rechecking retained artifacts")
    args = parser.parse_args()
    case = Case()
    if args.cleanup:
        if os.environ.get("E2E_PUBLISHED_FIXTURES"):
            raise RuntimeError("external signed-fixture consumers do not own publisher cleanup")
        cleanup(case)
        return
    external = os.environ.get("E2E_PUBLISHED_FIXTURES")
    if args.resume:
        if external:
            raise RuntimeError("external signed fixtures cannot resume a producer build")
        resume_producer(case)
    if external:
        # A clean runner imports only signed envelopes and public keys. The
        # originating producer retains ownership of images and their cleanup.
        with case:
            source = Path(external).resolve()
            for name in SIGNED_FIXTURE_DIRECTORIES:
                release_selection(source / name, case.region)
                destination = case.work / name
                destination.mkdir(mode=0o700)
                for filename in ["image-release.json", "publisher.pem"]:
                    shutil.copyfile(source / name / filename, destination / filename)
            publisher = case.state / "services/allinone-builder/state.json"
            publisher.parent.mkdir(parents=True)
            shutil.copyfile(source / "allinone-publisher-state.json", publisher)
            case.checks.update(external_signed_fixtures=True, producer_artifacts_copied=False)
        return
    case.defer_cleanup = True
    ledger = case.work / "published-images.json"
    records = json.loads(ledger.read_text()) if args.resume and ledger.exists() else []
    try:
        with case:
            examples = case.work / "examples"
            if not args.resume:
                shutil.copytree(ROOT / "examples", examples)
                run(["openssl", "genpkey", "-algorithm", "ED25519", "-out", case.work / "publisher.key"], case.work / "publisher-key")
                run(["openssl", "pkey", "-in", case.work / "publisher.key", "-pubout", "-out", case.work / "publisher.pem"], case.work / "publisher-public")
            run(["aliyun", "--region", case.region, "sts", "GetCallerIdentity"], case.work / "caller-identity")
            cosign = case.work / "cosign.key"
            if not args.resume:
                case.command("cosign-key", "key", "generate-cosign", "--output-key-prefix", cosign.with_suffix(""))
            # JSON specs can be supplied as AppSpec fixtures; normal examples stay YAML.
            allinone = examples / "allinone" / "build.yaml"
            if not args.resume:
                yaml = allinone.read_text().replace("region: cn-beijing", "region: "+case.region)
                yaml = yaml.replace("reference_values: sample", "reference_values: rekor\n  rekor:\n    cosign_key: "+str(cosign)+"\n    slsa_generator: /usr/libexec/shelter/slsa/slsa-generator")
                allinone.write_text(yaml)
            builds = [("allinone-builder", allinone, ["release"])]
            for service, spec, variants in builds:
                if not (args.resume and verify_resumable_build(case, service, spec, variants)):
                    case.command("build-"+service, "build", "--spec", spec, timeout=28800)
                for variant in variants:
                    publish_variant(case, service, spec, variant, case.work / "allinone", records)
            # Older producers may own inference publications too. Retain every
            # owned entry for cleanup when resuming, without rebuilding them.
            case.checks["signed_images_published"] = REQUIRED_PUBLICATIONS <= {
                (record["service"], record["variant"]) for record in records
            }
    except BaseException:
        try:
            cleanup(case)
        except Exception:
            pass
        raise


if __name__ == "__main__":
    main()
