use super::*;
use std::collections::BTreeSet;

#[derive(Debug, Serialize)]
struct MarkdownDoc {
    topic: &'static str,
    title: &'static str,
    content: &'static str,
}

#[derive(Debug, Serialize)]
struct SpecValidationReport {
    ok: bool,
    spec: String,
    service_id: Option<String>,
    image_variant: Option<String>,
    checks: Vec<SpecValidationCheck>,
}

#[derive(Debug, Serialize)]
struct SpecValidationCheck {
    name: String,
    ok: bool,
    message: String,
}

pub(super) fn cmd_docs(args: &DocsArgs) -> Result<()> {
    let doc = docs_topic(args.topic);
    match args.format {
        OutputFormat::Markdown => println!("{}", doc.content.trim()),
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&doc)?),
    }
    Ok(())
}

pub(super) fn cmd_spec(args: &SpecArgs) -> Result<()> {
    match &args.command {
        SpecCommands::Schema { format } => match format {
            OutputFormat::Markdown => println!("{}", SPEC_SCHEMA_MARKDOWN.trim()),
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&spec_schema_json())?)
            }
        },
        SpecCommands::Validate { spec, format } => cmd_spec_validate(spec, *format)?,
    }
    Ok(())
}

fn cmd_spec_validate(spec_path: &Path, format: OutputFormat) -> Result<()> {
    let report = validate_spec_file(spec_path);
    match format {
        OutputFormat::Markdown => {
            println!(
                "Spec validation {}: {}",
                if report.ok { "passed" } else { "failed" },
                report.spec
            );
            if let Some(service_id) = &report.service_id {
                println!("service: {service_id}");
            }
            if let Some(image_variant) = &report.image_variant {
                println!("image_variant: {image_variant}");
            }
            for check in &report.checks {
                println!(
                    "{:<28} {:<5} {}",
                    check.name,
                    if check.ok { "ok" } else { "fail" },
                    check.message
                );
            }
        }
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
    }
    if report.ok {
        Ok(())
    } else {
        bail!("spec validation failed")
    }
}

fn validate_spec_file(spec_path: &Path) -> SpecValidationReport {
    let mut checks = Vec::new();
    let spec_dir = spec_path.parent().unwrap_or_else(|| Path::new("."));
    match AgentSpec::from_path(spec_path) {
        Ok(spec) => {
            push_check(&mut checks, "parse", true, "AppSpec parsed");
            if let Some(image) = &spec.deploy.image {
                push_path_check(&mut checks, "deploy.image.release", &image.release);
                if !image.skip_signature_verification {
                    push_path_check(&mut checks, "deploy.image.trusted_key", &image.trusted_key);
                }
            }
            match spec.ensure_mvp_supported() {
                Ok(()) => push_check(&mut checks, "supported", true, "uses supported features"),
                Err(err) => push_check(&mut checks, "supported", false, err.to_string()),
            }
            for (index, file) in spec.build.files.iter().enumerate() {
                push_path_check(
                    &mut checks,
                    format!("build.files[{index}].source"),
                    &file.source,
                );
            }
            for (index, script) in spec.build.scripts.iter().enumerate() {
                push_path_check(&mut checks, format!("build.scripts[{index}]"), script);
            }
            if let Some(container) = &spec.build.container {
                if let Some(archive) = &container.archive {
                    push_path_check(&mut checks, "build.container.archive", archive);
                }
            }
            push_script_package_checks(&mut checks, &spec);
            push_script_package_manager_checks(&mut checks, &spec);
            if let Some(debug) = &spec.build.variants.debug {
                if let Some(ssh_public_key) = &debug.ssh_public_key {
                    push_path_check(
                        &mut checks,
                        "build.variants.debug.ssh_public_key",
                        ssh_public_key,
                    );
                }
            }
            for (name, resource) in &spec.resources {
                push_path_check(
                    &mut checks,
                    format!("resources.{name}.source"),
                    &resource.source,
                );
            }
            if let Some(passphrase) = &spec.secrets.disk_passphrase {
                push_path_check(&mut checks, "secrets.disk_passphrase", passphrase);
            }
            if let Some(rekor) = &spec.attestation.rekor {
                if let Some(cosign_key) = &rekor.cosign_key {
                    push_path_check(&mut checks, "attestation.rekor.cosign_key", cosign_key);
                }
                push_host_tool_path_check(
                    &mut checks,
                    "attestation.rekor.slsa_generator",
                    &rekor.slsa_generator,
                    spec_dir,
                );
            }
            let ok = checks.iter().all(|check| check.ok);
            SpecValidationReport {
                ok,
                spec: spec_path.display().to_string(),
                service_id: Some(spec.service.id.clone()),
                image_variant: Some(spec.image_variant().to_string()),
                checks,
            }
        }
        Err(err) => {
            push_check(&mut checks, "parse", false, format_error_chain(&err));
            SpecValidationReport {
                ok: false,
                spec: spec_path.display().to_string(),
                service_id: None,
                image_variant: None,
                checks,
            }
        }
    }
}

fn format_error_chain(err: &anyhow::Error) -> String {
    let mut parts = err.chain().map(|cause| cause.to_string());
    let Some(first) = parts.next() else {
        return err.to_string();
    };
    let rest = parts.collect::<Vec<_>>();
    if rest.is_empty() {
        first
    } else {
        format!("{first}: {}", rest.join(": "))
    }
}

struct ScriptToolRequirement {
    command: &'static str,
    packages: &'static [&'static str],
    hint: &'static str,
}

const SCRIPT_TOOL_REQUIREMENTS: &[ScriptToolRequirement] = &[
    ScriptToolRequirement {
        command: "curl",
        packages: &["curl"],
        hint: "`curl`",
    },
    ScriptToolRequirement {
        command: "wget",
        packages: &["wget"],
        hint: "`wget`",
    },
    ScriptToolRequirement {
        command: "git",
        packages: &["git"],
        hint: "`git`",
    },
    ScriptToolRequirement {
        command: "tar",
        packages: &["tar"],
        hint: "`tar`",
    },
    ScriptToolRequirement {
        command: "gzip",
        packages: &["gzip"],
        hint: "`gzip`",
    },
    ScriptToolRequirement {
        command: "gunzip",
        packages: &["gzip"],
        hint: "`gzip`",
    },
    ScriptToolRequirement {
        command: "xz",
        packages: &["xz"],
        hint: "`xz`",
    },
    ScriptToolRequirement {
        command: "unxz",
        packages: &["xz"],
        hint: "`xz`",
    },
    ScriptToolRequirement {
        command: "unzip",
        packages: &["unzip"],
        hint: "`unzip`",
    },
    ScriptToolRequirement {
        command: "node",
        packages: &["nodejs", "npm"],
        hint: "`nodejs`",
    },
    ScriptToolRequirement {
        command: "npm",
        packages: &["npm"],
        hint: "`npm`",
    },
    ScriptToolRequirement {
        command: "npx",
        packages: &["npm"],
        hint: "`npm`",
    },
    ScriptToolRequirement {
        command: "corepack",
        packages: &["nodejs"],
        hint: "`nodejs`",
    },
    ScriptToolRequirement {
        command: "gcc",
        packages: &["gcc"],
        hint: "`gcc`",
    },
    ScriptToolRequirement {
        command: "g++",
        packages: &["gcc-c++"],
        hint: "`gcc-c++`",
    },
    ScriptToolRequirement {
        command: "make",
        packages: &["make"],
        hint: "`make`",
    },
    ScriptToolRequirement {
        command: "podman",
        packages: &["podman"],
        hint: "`podman`",
    },
];

fn push_script_package_checks(checks: &mut Vec<SpecValidationCheck>, spec: &AgentSpec) {
    if spec.build.scripts.is_empty() {
        return;
    }
    let packages = spec
        .build
        .packages
        .iter()
        .map(|package| package.trim().to_string())
        .collect::<BTreeSet<_>>();
    let mut issues = Vec::new();
    for script in &spec.build.scripts {
        let Ok(text) = fs::read_to_string(script) else {
            continue;
        };
        for requirement in SCRIPT_TOOL_REQUIREMENTS {
            if !script_mentions_command(&text, requirement.command) {
                continue;
            }
            if requirement
                .packages
                .iter()
                .any(|package| packages.contains(*package))
            {
                continue;
            }
            issues.push(format!(
                "{} uses `{}` but build.packages does not include {}",
                script.display(),
                requirement.command,
                requirement.hint
            ));
        }
    }
    if issues.is_empty() {
        push_check(
            checks,
            "build.scripts.packages",
            true,
            "common script commands are covered by build.packages",
        );
    } else {
        push_check(checks, "build.scripts.packages", false, issues.join("; "));
    }
}

fn push_script_package_manager_checks(checks: &mut Vec<SpecValidationCheck>, spec: &AgentSpec) {
    if spec.build.scripts.is_empty() {
        return;
    }
    let mut issues = Vec::new();
    for script in &spec.build.scripts {
        let Ok(text) = fs::read_to_string(script) else {
            continue;
        };
        let uses = script_os_package_manager_uses(&text);
        if !uses.is_empty() {
            issues.push(format!(
                "{} uses {}; put OS packages in build.packages instead",
                script.display(),
                uses.join(", ")
            ));
        }
    }
    if issues.is_empty() {
        push_check(
            checks,
            "build.scripts.no_os_pkg_manager",
            true,
            "build scripts do not install OS packages with package managers",
        );
    } else {
        push_check(
            checks,
            "build.scripts.no_os_pkg_manager",
            false,
            issues.join("; "),
        );
    }
}

fn script_os_package_manager_uses(script: &str) -> Vec<String> {
    let mut uses = BTreeSet::new();
    for line in script.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let words = script_words(line);
        for window in words.windows(2) {
            let command = window[0].as_str();
            let subcommand = window[1].as_str();
            let phrase = match (command, subcommand) {
                ("apt-get" | "apt", "install" | "update" | "upgrade") => {
                    Some(format!("{command} {subcommand}"))
                }
                ("yum" | "dnf" | "microdnf", "install" | "update" | "upgrade" | "module") => {
                    Some(format!("{command} {subcommand}"))
                }
                ("apk", "add" | "update" | "upgrade") => Some(format!("{command} {subcommand}")),
                _ => None,
            };
            if let Some(phrase) = phrase {
                uses.insert(phrase);
            }
        }
    }
    uses.into_iter().collect()
}

fn script_words(line: &str) -> Vec<String> {
    line.split(|ch: char| {
        !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '+'))
    })
    .filter(|token| !token.is_empty())
    .map(|token| token.rsplit('/').next().unwrap_or(token).to_string())
    .collect()
}

fn script_mentions_command(script: &str, command: &str) -> bool {
    script
        .split(|ch: char| {
            !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '+'))
        })
        .filter(|token| !token.is_empty())
        .map(|token| token.rsplit('/').next().unwrap_or(token))
        .any(|basename| basename == command)
}

fn push_check(
    checks: &mut Vec<SpecValidationCheck>,
    name: impl Into<String>,
    ok: bool,
    message: impl Into<String>,
) {
    checks.push(SpecValidationCheck {
        name: name.into(),
        ok,
        message: message.into(),
    });
}

fn push_path_check(checks: &mut Vec<SpecValidationCheck>, name: impl Into<String>, path: &Path) {
    let ok = path.exists();
    let message = if ok {
        format!("{} exists", path.display())
    } else {
        format!("{} does not exist", path.display())
    };
    push_check(checks, name, ok, message);
}

fn push_host_tool_path_check(
    checks: &mut Vec<SpecValidationCheck>,
    name: impl Into<String>,
    path: &Path,
    spec_dir: &Path,
) {
    if path.starts_with(spec_dir) {
        push_path_check(checks, name, path);
        return;
    }
    let message = if path.exists() {
        format!("{} exists", path.display())
    } else {
        format!(
            "{} is not present on this host; install the host tool before running a Rekor build",
            path.display()
        )
    };
    push_check(checks, name, true, message);
}

fn docs_topic(topic: DocsTopic) -> MarkdownDoc {
    match topic {
        DocsTopic::Overview => MarkdownDoc {
            topic: "overview",
            title: "Confidential Agent CLI",
            content: OVERVIEW_DOC,
        },
        DocsTopic::Workflow => MarkdownDoc {
            topic: "workflow",
            title: "Workflow",
            content: WORKFLOW_DOC,
        },
        DocsTopic::Appspec => MarkdownDoc {
            topic: "appspec",
            title: "AppSpec",
            content: APPSPEC_DOC,
        },
        DocsTopic::Ops => MarkdownDoc {
            topic: "ops",
            title: "Operations",
            content: OPS_DOC,
        },
    }
}

fn spec_schema_json() -> serde_json::Value {
    serde_json::json!({
        "schema": "confidential-agent/v1",
        "required_top_level": ["schema", "service", "deploy", "attestation", "resources"],
        "image_source": "provide build for a local image, or deploy.image for an authenticated published release",
        "service": {
            "id": "stable service name",
            "ports": "guest TCP ports exposed by the workload",
            "connect": "integer ports (rats-tls), or {port, encryption: rats-tls|ohttp}; ALB requires ohttp",
            "app_service": "systemd unit that indicates workload readiness",
            "ssh": "opt in to instance client/host keys delivered through CA; requires an image SSH service using /opt/data/cai-ssh"
        },
        "build": {
            "image_name": "base Shelter image name",
            "runtime_contract": "optional image-owned JSON contract path, captured during build and signed on export",
            "resize": "optional target disk size; rendered as Shelter disk.size",
            "container": "optional Shelter OCI workload payload; supports image/archive, mode, runtime, name, pull, command, env, working_dir, and network",
            "packages": "minimal OS packages installed by Shelter/mkosi",
            "files": "host files copied into the guest image",
            "scripts": "controller-local script paths executed inside the guest image build; use relative paths such as ./install-service.sh, not guest target paths such as /tmp/install.sh",
            "variants": "optional release/debug variant map"
        },
        "deploy": {
            "provider": "aliyun",
            "image_variant": "release or debug",
            "image": {"release": "signed image release JSON", "trusted_key": "trusted publisher Ed25519 SPKI PEM", "manifest": "optional sha256 version pin; otherwise read from authenticated release", "image_id": "optional existing shared/marketplace image override", "skip_signature_verification": "explicit release-signature opt-out; runtime attestation remains required"},
            "region": "Aliyun region",
            "zone_id": "Aliyun zone",
            "instance_type": "TDX-capable ECS instance type",
            "disk_gb": "root disk size",
            "replicas": "positive integer, default 1; multiple replicas require an enabled SLB",
            "slb": {"enabled": "requires replicas > 1 and a connect port", "slb_type": "nlb (default) or alb; alb requires OHTTP connect ports"}
        },
        "attestation": {
            "tee": "tdx",
            "mode": "challenge or trustee",
            "reference_values": "sample (local measured reference values) or rekor (published reference metadata)"
        },
        "resources": "runtime files injected after deployment",
        "secrets": "secret inputs, usually paths to local files"
    })
}

const SPEC_SCHEMA_MARKDOWN: &str = r#"
# AppSpec Essentials

Required top-level keys:

- `schema`: currently `confidential-agent/v1`.
- `service`: workload identity, ports, connect ports, and readiness systemd unit.
- `deploy`: cloud provider and Aliyun TDX ECS placement/shape.
- `attestation`: TDX mode and reference value policy.
- `resources`: files injected after deploy; use `{}` if no runtime files are needed.

Common optional keys:

- `build`: required for local builds; omit it when `deploy.image` selects a published image.
- `secrets`: secret inputs, usually local file paths.
- `a2a`: optional AgentCard publication settings. Network peerings are managed separately with `peering` commands.
- `build.runtime_contract`: image-owned runtime JSON, captured during build and signed on export.
- `deploy.replicas`: positive replica count, default 1; values above 1 require `deploy.slb.enabled`.
- `deploy.slb`: `{enabled, slb_type: nlb|alb}`; ALB requires OHTTP connect ports.
- `service.connect`: integer ports (RATS-TLS), or `{port, encryption: rats-tls|ohttp}`.
- `attestation.mode`: `challenge` or `trustee`; `reference_values`: `sample` or `rekor`. Sample uses measured reference values, not a bypass. Challenge currently falls back to local measured values if Rekor reference registration fails.

Use `confidential-agent spec validate --spec <path> --format json` before build/deploy. Validation checks local file paths, common install-script command/package mismatches, OS package-manager use inside build scripts, and parse error details. If omitted, common commands default to `confidential-agent.yaml` in the current directory.

CA builds the guest rootfs with mkosi. Use `build.container` for an OCI workload image or archive; it is rendered as Shelter `container:` and is not a replacement for the mkosi base image. Keep `build.packages` limited to Alinux/RHEL OS prerequisites and let the target's package manager install application dependencies.
`build.scripts` entries are local script paths from the controller work directory, usually the same file listed in `build.files[].source`; they are not the guest `build.files[].target` path.

To deploy without producer state or a local disk image, set `deploy.image.release` and `trusted_key`. The optional `manifest` field (a full `sha256:` digest) pins a specific version; by default CA reads the identity from the authenticated release and saves it in deployment state. Omit `build` and run `deploy` directly. CA authenticates the release through Shelter before provisioning or releasing resources; challenge/Trustee resource delivery remains unchanged. `image export <service> --signing-key <Ed25519 PKCS8 PEM> --output <release.json>` exports an available publication from the producer; --region and --image-id can select an existing shared/marketplace image without upload. deploy --skip-signature-verification explicitly skips the release signature and permits omitting trusted_key, but keeps manifest/runtime checks and attestation. Transfer only the release and publisher public key to consumers. Consumer `destroy` preserves the shared image.
"#;

const OVERVIEW_DOC: &str = r#"
# Confidential Agent CLI

This CLI builds, deploys, connects to, and operates agents in a TDX-backed Confidential Agent environment. It is intentionally concise; detailed migration instructions should live in the operator skill.

Useful self-description commands:

- `confidential-agent --version` or `confidential-agent version`
- `confidential-agent docs workflow`
- `confidential-agent docs appspec`
- `confidential-agent spec schema`
- `confidential-agent spec validate --spec <path> --format json` or just `confidential-agent spec validate` (defaults to `confidential-agent.yaml`)
"#;

const WORKFLOW_DOC: &str = r#"
# Workflow

1. Inspect the target agent and identify its runtime, service command, ports, config files, and health checks.
2. Write an AppSpec that installs the runtime into the guest image and enables one systemd service.
3. Validate the spec locally.
4. Build a release/debug image. Build does not use peerings or security group inputs.
5. Optionally run `image publish <service> --spec <path>` to import the built image as a reusable Aliyun custom image.
6. Add operator peering for the controller CIDR so deploy can render security group ingress.
7. Deploy the selected image variant to Aliyun TDX ECS. Deploy reuses an available published image only when provider, region, variant and build id match. It uses the saved publication identity and source digest without reading the local QCOW2.
8. Inject runtime resources and secrets.
9. Use `peering apply` after later peering changes to refresh active service security groups.
10. Connect, probe the service with normal tools, and collect logs/status for operations.
"#;

const APPSPEC_DOC: &str = r#"
# AppSpec

The AppSpec is the contract between the agent migration workflow and the Confidential Agent CLI. Keep it service-oriented: one workload, explicit ports, deterministic build scripts, and a systemd unit that can be checked after boot.

Prefer copied scripts and structured resource files over ad hoc shell snippets in prompts. Keep secrets out of the image; inject them as resources or secret files during deploy/ops.

CA builds the guest rootfs with mkosi. Use Alinux/RHEL package names, start minimal, and refine after package-manager errors instead of adding optional tools by default.
"#;

const OPS_DOC: &str = r#"
# Operations

Core commands:

- `up --spec <path>` builds if needed, deploys or resumes, and starts a protected connection. Without --spec, it initializes a project first.
- `cleanup SERVICE` destroys one recorded instance; `cleanup --all --dry-run` previews all recorded instances.
- `build --spec <path>` creates the confidential image; `build` defaults to `confidential-agent.yaml`.
- `key generate-cosign --output-key-prefix ./cosign` creates a Rekor signing key pair using native cosign.
- `image publish <service> --spec <path> [--region <region>] [--no-wait]` uploads the built image to OSS, imports an Aliyun custom image, and records published state. `--no-wait` returns after the import task is created; rerun without it to wait for the same image to become available.
- `deploy --spec <path>` provisions the cloud instance; `deploy` defaults to `confidential-agent.yaml`. If a matching available published image exists, deploy reuses its `image_id` and skips upload/import.
- `status --service <id> --live --json` checks local and guest state.
- `connect --render-only` prints the local forwarding config without starting the tunnel. The JSON includes `client_endpoints[]` with the exact host-side `127.0.0.1:<port>` URLs.
- `connect --service <id>` selects one active local service by exact id; without it, connect covers all active services that expose `service.connect` ports.
- `connect start --service <id> --ready-json connect-ready.json --wait-ready 120` starts the tunnel in the background, waits for local ports, and writes ready metadata. `connect stop --ready-json connect-ready.json` stops only that recorded tunnel.
- `connect` without `start` remains the foreground human-operator mode and runs until stopped.
- `destroy <service-id>` tears down provisioned resources.
- `image unpublish <service> [--image-id <id>] [--force]` deletes published custom images and residual OSS objects tracked in local state. Active/deployed services using the image require `--force`.
- `image prune --dry-run --all` audits published images eligible for cleanup; omit `--dry-run` to delete them. Without `--all`, prune targets old build ids and failed publish entries.

For probes, use standard tools such as `curl`, `nc`, the controller agent API, or the workload's native client. Keep probe logic in evaluation scripts or skills rather than expanding the CLI surface.
"#;

#[cfg(test)]
mod self_describe_tests {
    use super::*;

    #[test]
    fn docs_are_concise() {
        assert!(OVERVIEW_DOC.contains("spec validate"));
        assert!(!OVERVIEW_DOC.contains("Generic Python Agent"));
    }

    #[test]
    fn schema_json_names_required_sections() {
        let schema = spec_schema_json();
        let required = schema
            .get("required_top_level")
            .and_then(|value| value.as_array())
            .unwrap();
        assert!(required.iter().any(|value| value == "service"));
        assert!(!required.iter().any(|value| value == "build"));
        assert!(schema["deploy"]["image"]["manifest"].is_string());
        assert!(required.iter().any(|value| value == "resources"));
    }

    #[test]
    fn spec_validate_checks_all_local_path_fields() {
        let temp = tempfile::tempdir().unwrap();
        let spec_path = temp.path().join("agent.yaml");
        fs::write(
            &spec_path,
            r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
build:
  image_name: openclaw-agent
  container:
    archive: ./missing-container.oci.tar
  scripts: [./missing-install.sh]
  variants:
    release:
      enabled: true
    debug:
      enabled: true
      ssh_public_key: ./missing-debug.pub
deploy:
  provider: aliyun
  image_variant: debug
  instance_type: ecs.g9i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-i
attestation:
  tee: tdx
  mode: challenge
  reference_values: rekor
  rekor:
    cosign_key: ./missing-cosign.key
    slsa_generator: ./missing-slsa-generator
resources: {}
"#,
        )
        .unwrap();

        let report = validate_spec_file(&spec_path);
        let failed: Vec<&str> = report
            .checks
            .iter()
            .filter(|check| !check.ok)
            .map(|check| check.name.as_str())
            .collect();

        assert!(!report.ok);
        assert!(failed.contains(&"build.container.archive"));
        assert!(failed.contains(&"build.scripts[0]"));
        assert!(failed.contains(&"build.variants.debug.ssh_public_key"));
        assert!(failed.contains(&"attestation.rekor.cosign_key"));
        assert!(failed.contains(&"attestation.rekor.slsa_generator"));
    }

    #[test]
    fn spec_validate_parse_check_includes_source_chain() {
        let temp = tempfile::tempdir().unwrap();
        let spec_path = temp.path().join("agent.yaml");
        fs::write(
            &spec_path,
            r#"
schema: confidential-agent/v1
service:
  id: sample
  ports: [8080]
build:
  image_name: sample
  commands: []
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g9i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-i
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#,
        )
        .unwrap();

        let report = validate_spec_file(&spec_path);
        let parse_check = report
            .checks
            .iter()
            .find(|check| check.name == "parse")
            .unwrap();

        assert!(!report.ok);
        assert!(!parse_check.ok);
        assert!(parse_check.message.contains("failed to parse agent spec"));
        assert!(parse_check.message.contains("commands"));
    }

    #[test]
    fn spec_validate_rejects_missing_script_tool_packages() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("install.sh"),
            "#!/usr/bin/env bash\ncurl -fsSL https://example.invalid/app.tar.gz | tar -xz -C /opt\n",
        )
        .unwrap();
        let spec_path = temp.path().join("agent.yaml");
        fs::write(
            &spec_path,
            r#"
schema: confidential-agent/v1
service:
  id: sample
  ports: [8080]
  connect: [8080]
build:
  image_name: sample
  packages: [ca-certificates, curl]
  scripts: [./install.sh]
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g9i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-i
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#,
        )
        .unwrap();

        let report = validate_spec_file(&spec_path);
        let package_check = report
            .checks
            .iter()
            .find(|check| check.name == "build.scripts.packages")
            .unwrap();

        assert!(!report.ok);
        assert!(!package_check.ok);
        assert!(package_check.message.contains("`tar`"));
    }

    #[test]
    fn spec_validate_accepts_script_tool_packages() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("install.sh"),
            "#!/usr/bin/env bash\ncurl -fsSL https://example.invalid/app.tar.gz | tar -xz -C /opt\n",
        )
        .unwrap();
        let spec_path = temp.path().join("agent.yaml");
        fs::write(
            &spec_path,
            r#"
schema: confidential-agent/v1
service:
  id: sample
  ports: [8080]
  connect: [8080]
build:
  image_name: sample
  packages: [ca-certificates, curl, tar]
  scripts: [./install.sh]
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g9i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-i
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#,
        )
        .unwrap();

        let report = validate_spec_file(&spec_path);

        assert!(report.ok);
    }

    #[test]
    fn spec_validate_rejects_script_os_package_install() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("install.sh"),
            "#!/usr/bin/env bash\n# Do not run dnf install here.\ndnf install -y nodejs\n",
        )
        .unwrap();
        let spec_path = temp.path().join("agent.yaml");
        fs::write(
            &spec_path,
            r#"
schema: confidential-agent/v1
service:
  id: sample
  ports: [8080]
  connect: [8080]
build:
  image_name: sample
  packages: [ca-certificates]
  scripts: [./install.sh]
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g9i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-i
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#,
        )
        .unwrap();

        let report = validate_spec_file(&spec_path);
        let package_manager_check = report
            .checks
            .iter()
            .find(|check| check.name == "build.scripts.no_os_pkg_manager")
            .unwrap();

        assert!(!report.ok);
        assert!(!package_manager_check.ok);
        assert!(package_manager_check.message.contains("dnf install"));
        assert!(!package_manager_check.message.contains("# Do not run"));
    }
}
