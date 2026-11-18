use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};
use tempfile::TempDir;

const REGION: &str = "cn-beijing";
const ACCOUNT: &str = "123456";
const MANIFEST: &str = concat!(
    "sha256:",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
);
const OCI: &str = concat!(
    "registry.example/example/shelter-artifact@sha256:",
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
);

struct Fixture {
    temp: TempDir,
    release_digest: String,
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        for tool in ["shelter", "aliyun", "ossutil64"] {
            let path = temp.path().join(tool);
            fs::write(&path, TOOLS).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::create_dir(temp.path().join("artifact")).unwrap();
        let disk = b"portable image disk";
        let release = serde_json::to_vec(&json!({
            "manifest_digest": MANIFEST,
            "artifact_alias": "portable-image",
            "disk_digest": digest(disk),
            "disk_size": disk.len(),
            "runtime_metadata": {"image_variant": "release"},
        }))
        .unwrap();
        fs::write(temp.path().join("image-release.json"), &release).unwrap();
        let fixture = Self {
            temp,
            release_digest: digest(&release),
        };
        fixture.write_artifact(MANIFEST, disk);
        fixture
    }

    fn install_registry_tools(&self) {
        for tool in ["curl", "oras"] {
            let path = self.temp.path().join(tool);
            fs::write(&path, TOOLS).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn write_artifact(&self, manifest: &str, disk: &[u8]) {
        let path = self.temp.path().join("artifact");
        fs::write(path.join("disk.qcow2"), disk).unwrap();
        fs::write(
            path.join("artifact-manifest.json"),
            json!({
                "manifest_digest": manifest,
                "outputs": [{
                    "role": "disk",
                    "size": disk.len(),
                    "digest": digest(disk),
                    "path": "disk.qcow2",
                }],
            })
            .to_string(),
        )
        .unwrap();
    }

    fn command(&self, account: &str) -> Command {
        let root = self.temp.path();
        let mut command = Command::new(env!("CARGO_BIN_EXE_confidential-agent"));
        command
            .current_dir(root)
            .arg("--state-dir")
            .arg(root.join("state"))
            .arg("--tools-dir")
            .arg(root)
            .arg("--shelter-bin")
            .arg(root.join("shelter"))
            .env(
                "PATH",
                std::env::join_paths(
                    std::iter::once(root.to_owned())
                        .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
                )
                .unwrap(),
            )
            .env("ALICLOUD_ACCESS_KEY", account)
            .env("ALICLOUD_SECRET_KEY", "fixture-secret")
            .env("CA_IMAGE_IMPORT_TIMEOUT_SEC", "5")
            .env_remove("CA_IMAGE_OSS_ENDPOINT");
        command
    }

    fn publish(&self, source: &str, account: &str, region: &str) -> Command {
        let mut command = self.command(account);
        command.args([
            "image",
            "publish",
            "--artifact",
            source,
            "--image-release",
            "image-release.json",
            "--region",
            region,
        ]);
        command
    }

    fn publish_image(&self, source: &str, account: &str, region: &str) -> Value {
        let output = self.temp.path().join("publication.json");
        assert_success(
            self.publish(source, account, region)
                .args(["--release-digest", &self.release_digest, "--output"])
                .arg(&output)
                .output()
                .unwrap(),
        );
        serde_json::from_slice(&fs::read(output).unwrap()).unwrap()
    }

    fn unpublish(&self, account: &str, region: &str, filters: &[&str]) -> Output {
        self.command(account)
            .args(["image", "unpublish", MANIFEST, "--region", region])
            .args(filters)
            .output()
            .unwrap()
    }

    fn image_list(&self) -> Vec<Value> {
        let output = self
            .command(ACCOUNT)
            .args(["image", "list", "--json"])
            .output()
            .unwrap();
        let entries = serde_json::from_slice(&output.stdout);
        assert_success(output);
        entries.unwrap()
    }

    fn calls(&self, operation: &str) -> Vec<String> {
        let log = fs::read_to_string(self.temp.path().join("calls")).unwrap_or_default();
        log.lines()
            .filter_map(|line| line.strip_prefix(operation)?.strip_prefix(' '))
            .map(str::to_owned)
            .collect()
    }
}

fn assert_success(output: Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_failure(output: Output, message: &str) {
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(message), "{stderr}");
}

#[test]
fn portable_artifact_reuses_publication_and_retires_only_selected_scope() {
    let fixture = Fixture::new();
    fixture.install_registry_tools();
    let first = fixture.publish_image(OCI, ACCOUNT, REGION);
    assert_eq!(first["status"], "available");
    assert_eq!(first["account_id"], ACCOUNT);
    assert_eq!(first["manifest_digest"], MANIFEST);
    assert_eq!(first["image_id"], "m-123456-cn-beijing");
    assert_eq!(first["oss_cleaned"], true);
    assert_eq!(
        fixture.publish_image(OCI, ACCOUNT, REGION)["image_id"],
        first["image_id"]
    );
    assert_eq!(fixture.calls("oras-default"), [OCI]);
    assert_eq!(fixture.calls("pull-layout"), [OCI]);
    assert!(fixture.calls("blob-offset").is_empty());
    assert_eq!(fixture.calls("cp"), ["disk.qcow2"]);
    assert_eq!(fixture.calls("ImportImage"), ["m-123456-cn-beijing"]);

    let other_account = fixture.publish_image("artifact", "654321", REGION);
    let other_region = fixture.publish_image("artifact", ACCOUNT, "cn-shanghai");
    assert_eq!(other_account["account_id"], "654321");
    assert_eq!(other_account["image_id"], "m-654321-cn-beijing");
    assert_eq!(other_region["image_id"], "m-123456-cn-shanghai");
    assert_ne!(first["bucket"], other_account["bucket"]);

    let entries = fixture.image_list();
    assert_eq!(entries.len(), 3);
    for receipt in [&first, &other_account, &other_region] {
        assert!(entries.iter().any(|entry| {
            let publication = &entry["publications"][0];
            entry["service_id"] == MANIFEST
                && publication["manifest_digest"] == MANIFEST
                && publication["account_id"] == receipt["account_id"]
                && publication["region"] == receipt["region"]
                && publication["image_id"] == receipt["image_id"]
        }));
    }

    for filter in [
        ["--image-id", "m-654321-cn-beijing"],
        ["--variant", "debug"],
    ] {
        assert_failure(
            fixture.unpublish(ACCOUNT, REGION, &filter),
            "no publication matches",
        );
    }
    assert!(fixture.calls("DeleteImage").is_empty());
    assert_success(fixture.unpublish(
        ACCOUNT,
        REGION,
        &["--variant", "release", "--image-id", "m-123456-cn-beijing"],
    ));
    assert_eq!(fixture.calls("DeleteImage"), ["m-123456-cn-beijing"]);
    assert_eq!(fixture.calls("DeleteSnapshot"), ["s-123456-cn-beijing"]);
    assert_failure(
        fixture.unpublish(ACCOUNT, REGION, &[]),
        "no publication for this artifact",
    );
    assert_eq!(
        fixture.publish_image("artifact", "654321", REGION)["image_id"],
        other_account["image_id"]
    );
    assert_eq!(
        fixture.publish_image("artifact", ACCOUNT, "cn-shanghai")["image_id"],
        other_region["image_id"]
    );
    assert_eq!(fixture.calls("cp").len(), 3);
    assert_eq!(fixture.calls("rm").len(), 3);
}

#[test]
fn portable_artifact_requires_matching_release_manifest_and_disk() {
    let fixture = Fixture::new();
    let publish = || {
        fixture
            .publish("artifact", ACCOUNT, REGION)
            .args(["--release-digest", &fixture.release_digest])
            .output()
            .unwrap()
    };
    let wrong_digest = digest(b"different release");
    assert_failure(
        fixture
            .publish("artifact", ACCOUNT, REGION)
            .args(["--release-digest", &wrong_digest])
            .output()
            .unwrap(),
        "release digest mismatch",
    );
    fixture.write_artifact(&digest(b"different manifest"), b"portable image disk");
    assert_failure(publish(), "artifact does not match the release manifest");
    fixture.write_artifact(MANIFEST, b"different disk");
    assert_failure(publish(), "artifact disk does not match release");
    assert!(fixture.calls("cp").is_empty());
    assert!(fixture.calls("ImportImage").is_empty());
}

#[test]
fn portable_artifact_requires_release_authentication_and_existing_source() {
    let fixture = Fixture::new();
    assert_failure(
        fixture
            .publish("artifact", ACCOUNT, REGION)
            .output()
            .unwrap(),
        "--release-digest",
    );
    assert_failure(
        fixture
            .publish("absent/artifact-manifest.json", ACCOUNT, REGION)
            .args(["--release-digest", &fixture.release_digest])
            .output()
            .unwrap(),
        "absent/artifact-manifest.json",
    );
    assert_failure(
        fixture
            .publish("ghcr.io/example/shelter-artifact:latest", ACCOUNT, REGION)
            .args(["--release-digest", &fixture.release_digest])
            .output()
            .unwrap(),
        "a remote OCI reference must end in @sha256:",
    );
    assert!(fixture.calls("pull").is_empty());
    assert!(fixture.calls("ImportImage").is_empty());
}

const PUBLIC_OCI: &str = concat!(
    "ghcr.io/example/shelter-artifact:candidate@sha256:",
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
);

#[test]
fn public_artifact_resumes_interrupted_disk_and_passes_layout_to_shelter() {
    let fixture = Fixture::new();
    fixture.install_registry_tools();
    let output = fixture.publish_image(PUBLIC_OCI, ACCOUNT, REGION);
    assert_eq!(output["status"], "available");
    assert_eq!(fixture.calls("blob-offset"), ["0", "8"]);
    assert_eq!(fixture.calls("oras-layout"), [PUBLIC_OCI]);
    assert_eq!(fixture.calls("pull-layout"), [PUBLIC_OCI]);
    assert_eq!(fixture.calls("ImportImage"), ["m-123456-cn-beijing"]);
}

#[test]
fn anonymous_registry_refusal_uses_default_oras_credentials() {
    for step in ["token", "manifest"] {
        let fixture = Fixture::new();
        fixture.install_registry_tools();
        let output = fixture
            .publish(PUBLIC_OCI, ACCOUNT, REGION)
            .env("FIXTURE_REFUSE_ANONYMOUS", step)
            .args(["--release-digest", &fixture.release_digest])
            .output()
            .unwrap();
        assert_success(output);
        assert_eq!(fixture.calls("anonymous-denied"), [step]);
        assert_eq!(fixture.calls("oras-default"), [PUBLIC_OCI]);
        assert_eq!(fixture.calls("pull-layout"), [PUBLIC_OCI]);
        assert!(fixture.calls("blob-offset").is_empty());
        assert!(fixture.calls("oras-layout").is_empty());
    }
}

const TOOLS: &str = r#"#!/usr/bin/env python3.11
import hashlib
import json
import os
import sys
from pathlib import Path

tool = Path(sys.argv[0]).name
args = sys.argv[1:]

def arg(flag):
    return args[args.index(flag) + 1]

def log(operation, value):
    with open("calls", "a", encoding="utf-8") as calls:
        print(operation, value, file=calls)

def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()

def shel():
    operation, source = args[1:3]
    if operation == "inspect-release":
        data = Path(source).read_bytes()
        if arg("--release-digest") != digest(data):
            sys.exit("release digest mismatch")
        return json.loads(data)
    if operation == "pull":
        if "--oci-layout" in args:
            layout = Path(arg("--oci-layout"))
            assert (layout / "index.json").is_file()
            assert Path(arg("--oras")).resolve() == Path("oras").resolve()
            log("pull-layout", source)
        else:
            log(operation, source)
        return {"record_path": "artifact"}
    if operation == "verify":
        path = Path(source)
        manifest = json.loads(path.read_text())
        disk, = manifest["outputs"]
        data = (path.parent / disk["path"]).read_bytes()
        assert disk["digest"] == digest(data)
        assert disk["size"] == len(data)
        return {"manifest_digest": manifest["manifest_digest"]}
    sys.exit("unexpected Shelter command: " + repr(args))

def ecs():
    account = os.environ["ALICLOUD_ACCESS_KEY"]
    operation = args[3]
    suffix = account + "-" + arg("--region")
    image_id = "m-" + suffix
    snapshot_id = "s-" + suffix
    if operation == "GetCallerIdentity":
        return {"AccountId": account}
    if operation == "DescribeRegions":
        return {}
    if operation == "ImportImage":
        log(operation, image_id)
        Path(image_id).touch()
        return {"ImageId": image_id}
    if operation == "DescribeImages":
        images = []
        if "--ImageId" in args and arg("--ImageId") == image_id and Path(image_id).exists():
            images.append({
                "ImageId": image_id,
                "Status": "Available",
                "DiskDeviceMappings": {"DiskDeviceMapping": [{"SnapshotId": snapshot_id}]},
            })
        return {"Images": {"Image": images}}
    if operation == "DeleteImage":
        assert arg("--ImageId") == image_id
        Path(image_id).unlink()
        log(operation, image_id)
        return {}
    if operation == "DeleteSnapshot":
        assert arg("--SnapshotId") == snapshot_id
        log(operation, snapshot_id)
        return {}
    sys.exit("unexpected Aliyun command: " + repr(args))

def curl():
    url = next(a for a in args if a.startswith("https://"))
    disk = Path("artifact/disk.qcow2").read_bytes()
    if url.endswith("/token"):
        assert arg("--data-urlencode") == "service=ghcr.io"
        assert "scope=repository:example/shelter-artifact:pull" in args
        if os.environ.get("FIXTURE_REFUSE_ANONYMOUS") == "token":
            log("anonymous-denied", "token")
            sys.stdout.write("\n403")
        else:
            sys.stdout.write(json.dumps({"token":"test-token"}) + "\n200")
        return
    if "--header" not in args or "@-" not in args:
        assert "/v2/example/shelter-artifact/manifests/sha256:" in url
        print('www-authenticate: Bearer realm="https://ghcr.io/token",service="ghcr.io"', file=sys.stderr)
        sys.stdout.write("\n401")
        return
    assert sys.stdin.read() == "Authorization: Bearer test-token\n"
    if "/manifests/" in url:
        if os.environ.get("FIXTURE_REFUSE_ANONYMOUS") == "manifest":
            log("anonymous-denied", "manifest")
            sys.stdout.write("\n403")
            return
        manifest = {
            "artifactType":"application/vnd.shelter.artifact.v1",
            "layers":[{"digest":digest(disk),"size":len(disk)}],
        }
        sys.stdout.write(json.dumps(manifest) + "\n200")
        return
    assert url.endswith("/blobs/" + digest(disk))
    assert arg("--continue-at") == "-"
    part = Path(arg("--output"))
    offset = part.stat().st_size if part.exists() else 0
    log("blob-offset", offset)
    with part.open("ab") as out:
        out.write(disk[offset:] if offset else disk[:8])
    if not offset:
        sys.stdout.write("\n200")
        sys.exit(18)
    sys.stdout.write("\n206")

def oras():
    assert args[:3] == ["cp", "--recursive", "--to-oci-layout"]
    flags = args[3:-2]
    source, destination = args[-2:]
    path, tag = destination.rsplit(":", 1)
    assert tag == "release"
    layout = Path(path)
    disk = Path("artifact/disk.qcow2").read_bytes()
    oci_digest = source.rsplit("@sha256:", 1)[1]
    assert layout.parent.name == oci_digest
    seeded_disk = layout / "blobs/sha256" / digest(disk).split(":")[1]
    if flags:
        assert flags[0] == "--from-registry-config" and len(flags) == 2
        assert json.loads(Path(flags[1]).read_text()) == {"auths":{}}
        assert seeded_disk.read_bytes() == disk
        assert not (layout.parent / "disk.part").exists()
        log("oras-layout", source)
    else:
        assert not seeded_disk.exists()
        log("oras-default", source)
    layout.mkdir(parents=True, exist_ok=True)
    (layout / "index.json").write_text("{}")

if tool == "shelter":
    print(json.dumps(shel()))
elif tool == "aliyun":
    print(json.dumps(ecs()))
elif tool == "curl":
    curl()
elif tool == "oras":
    oras()
elif tool == "ossutil64":
    operation, source = args[4:6]
    if operation == "cp":
        assert Path(source).read_bytes() == b"portable image disk"
        log(operation, Path(source).name)
    elif operation == "rm":
        log(operation, source)
    else:
        assert operation == "stat"
"#;
