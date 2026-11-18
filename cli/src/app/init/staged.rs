//! Application-centric shared CPU image and dedicated GPU initialization.
use super::*;
use serde_json::{json, Value};

pub(super) fn resolve_plan_and_interactive(args: &mut InitArgs) -> Result<InitPlan> {
    if args.non_interactive {
        return match (args.target, args.stage) {
            (None, Some(InitStage::Build)) => Ok(InitPlan::SharedCpuBuild),
            (None, _) => bail!(
                "init --non-interactive requires a target (for example `init openclaw`) or `--stage build`"
            ),
            (Some(target), Some(InitStage::Build)) if !target.is_gpu() => {
                Ok(InitPlan::SharedCpuBuild)
            }
            (Some(target), None | Some(InitStage::Deploy)) if !target.is_gpu() => {
                args.stage = Some(InitStage::Deploy);
                Ok(InitPlan::SharedCpuDeploy(target))
            }
            (Some(target), None) => Ok(InitPlan::GpuDirect(target)),
            (Some(target), Some(stage)) => Ok(InitPlan::GpuStaged(target, stage)),
        };
    }

    let plan = match (args.target, args.stage) {
        (None, Some(InitStage::Build)) => InitPlan::SharedCpuBuild,
        (None, Some(InitStage::Deploy)) => {
            let target = prompt_deploy_target()?;
            args.target = Some(target);
            if target.is_gpu() {
                InitPlan::GpuStaged(target, InitStage::Deploy)
            } else {
                InitPlan::SharedCpuDeploy(target)
            }
        }
        (None, None) => match prompt_interactive_choice()? {
            None => {
                args.stage = Some(InitStage::Build);
                InitPlan::SharedCpuBuild
            }
            Some(target) if target.is_gpu() => {
                args.target = Some(target);
                InitPlan::GpuDirect(target)
            }
            Some(target) => {
                args.target = Some(target);
                args.stage = Some(InitStage::Deploy);
                InitPlan::SharedCpuDeploy(target)
            }
        },
        (Some(target), Some(InitStage::Build)) if !target.is_gpu() => InitPlan::SharedCpuBuild,
        (Some(target), None | Some(InitStage::Deploy)) if !target.is_gpu() => {
            args.stage = Some(InitStage::Deploy);
            InitPlan::SharedCpuDeploy(target)
        }
        (Some(target), None) => InitPlan::GpuDirect(target),
        (Some(target), Some(stage)) => InitPlan::GpuStaged(target, stage),
    };

    if let InitPlan::SharedCpuDeploy(target) = plan {
        if args.harness_mode.is_none() {
            let (modes, default, explanation) = cpu_mode_choices(target);
            if modes.len() == 1 {
                args.harness_mode = Some(default.into());
            } else {
                println!("{explanation}");
                loop {
                    let selected = prompt_default(
                        &format!("Mode for {} ({})", target.command_name(), modes.join("/")),
                        default,
                    )?;
                    if modes.contains(&selected.as_str()) {
                        args.harness_mode = Some(selected);
                        break;
                    }
                    println!("Choose {}.", modes.join(" or "));
                }
            }
        }
    }

    let is_consumer = matches!(
        plan,
        InitPlan::SharedCpuDeploy(_) | InitPlan::GpuStaged(_, InitStage::Deploy)
    );
    if is_consumer {
        if matches!(
            plan,
            InitPlan::GpuStaged(InitTarget::VllmInference, InitStage::Deploy)
        ) {
            if args.oss_bucket.is_none() {
                args.oss_bucket = Some(prompt_value("Encrypted model OSS bucket[:/prefix]")?);
            }
            if args.model_key_file.is_none() {
                args.model_key_file = Some(prompt_value("Local model decryption key file")?.into());
            }
        }
        if args.service_id.is_none() {
            let default_id = project_name(args, plan);
            args.service_id = Some(prompt_default("Unique instance/service ID", &default_id)?);
        }
        if args.image_release.is_none() {
            args.image_release = Some(
                if Path::new("image-release.json").is_file() {
                    prompt_default("Image release path", "./image-release.json")?
                } else {
                    prompt_value("Image release path (image-release.json)")?
                }
                .into(),
            );
        }
        if args.release_digest.is_none() {
            args.release_digest = Some(prompt_value("Trusted release digest (sha256:...)")?);
        }
        if let InitPlan::SharedCpuDeploy(target) = plan {
            if args.model.is_none() && target != InitTarget::Qodercli {
                args.model = Some(prompt_default(
                    "Model name",
                    &confidential_agent_core::software::version("bailian_model"),
                )?);
            }
        }
    }

    if matches!(
        plan,
        InitPlan::SharedCpuBuild | InitPlan::SharedCpuDeploy(_) | InitPlan::GpuStaged(_, _)
    ) {
        if args.region.is_none() {
            args.region = Some(prompt_default("Region", "cn-beijing")?);
        }
        let region = args.region.as_deref().unwrap();
        if args.zone_id.is_none() {
            args.zone_id = Some(prompt_default("Zone", default_zone_id(region))?);
        }
        if args.instance_type.is_none() {
            args.instance_type = Some(prompt_default(
                "Instance type",
                default_instance_type(region, plan),
            )?);
        }
        if args.disk_gb.is_none() {
            args.disk_gb = Some(
                prompt_default("Disk GB", &default_disk(plan).to_string())?
                    .parse()
                    .context("disk size must be an integer")?,
            );
        }
        let supports_replicas = match plan {
            InitPlan::SharedCpuDeploy(target) => cpu_target_mode(args, target)?.1.is_some(),
            InitPlan::GpuStaged(_, InitStage::Deploy) => true,
            _ => false,
        };
        if is_consumer && args.replicas == 1 && supports_replicas {
            args.replicas = prompt_default("Replicas in this service (CLI: 1)", "1")?
                .parse()
                .context("replicas must be an integer")?;
        }
        args.output_dir = prompt_default(
            "Output parent directory",
            &args.output_dir.display().to_string(),
        )?
        .into();
    }
    Ok(plan)
}

fn prompt_interactive_choice() -> Result<Option<InitTarget>> {
    println!("Select deployment target (or shared CPU image build):");
    println!("  1) openclaw        (gateway, port 18789)");
    println!("  2) hermes          (gateway, port 8642)");
    println!("  3) codex           (app-server, port 4500)");
    println!("  4) opencode        (web, port 4096)");
    println!("  5) dsh             (web, port 3080)");
    println!("  6) claude-code     (CLI over SSH)");
    println!("  7) qodercli        (CLI over SSH)");
    println!("  8) pi              (CLI over SSH)");
    println!("  9) openclaw-vllm   (GPU direct build/deploy)");
    println!(" 10) vllm-inference  (GPU direct build/deploy)");
    println!(" 11) build           (shared CPU image builder)");
    loop {
        let value = prompt_default("Selection", "1")?;
        match value.as_str() {
            "1" | "openclaw" => return Ok(Some(InitTarget::Openclaw)),
            "2" | "hermes" | "hermes-agent" => return Ok(Some(InitTarget::Hermes)),
            "3" | "codex" => return Ok(Some(InitTarget::Codex)),
            "4" | "opencode" => return Ok(Some(InitTarget::Opencode)),
            "5" | "dsh" => return Ok(Some(InitTarget::Dsh)),
            "6" | "claude-code" | "claudecode" => return Ok(Some(InitTarget::Claudecode)),
            "7" | "qodercli" => return Ok(Some(InitTarget::Qodercli)),
            "8" | "pi" => return Ok(Some(InitTarget::Pi)),
            "9" | "openclaw-vllm" => return Ok(Some(InitTarget::OpenclawVllm)),
            "10" | "vllm-inference" => return Ok(Some(InitTarget::VllmInference)),
            "11" | "build" => return Ok(None),
            _ => println!("Enter 1-11, a target name, or `build`."),
        }
    }
}

fn prompt_deploy_target() -> Result<InitTarget> {
    println!("Select deployment target:");
    println!("  1) openclaw");
    println!("  2) hermes");
    println!("  3) codex");
    println!("  4) opencode");
    println!("  5) dsh");
    println!("  6) claude-code");
    println!("  7) qodercli");
    println!("  8) pi");
    println!("  9) openclaw-vllm");
    println!(" 10) vllm-inference");
    loop {
        let value = prompt_default("Target", "1")?;
        match value.as_str() {
            "1" | "openclaw" => return Ok(InitTarget::Openclaw),
            "2" | "hermes" | "hermes-agent" => return Ok(InitTarget::Hermes),
            "3" | "codex" => return Ok(InitTarget::Codex),
            "4" | "opencode" => return Ok(InitTarget::Opencode),
            "5" | "dsh" => return Ok(InitTarget::Dsh),
            "6" | "claude-code" | "claudecode" => return Ok(InitTarget::Claudecode),
            "7" | "qodercli" => return Ok(InitTarget::Qodercli),
            "8" | "pi" => return Ok(InitTarget::Pi),
            "9" | "openclaw-vllm" => return Ok(InitTarget::OpenclawVllm),
            "10" | "vllm-inference" => return Ok(InitTarget::VllmInference),
            _ => println!("Enter 1-10 or a target name."),
        }
    }
}

fn prompt_default(label: &str, default: &str) -> Result<String> {
    print!("{label} [{default}]: ");
    std::io::stdout().flush()?;
    let input = read_stdin_line()?;
    Ok(if input.trim().is_empty() {
        default.into()
    } else {
        input.trim().into()
    })
}

pub(super) fn default_disk(plan: InitPlan) -> u32 {
    match plan {
        InitPlan::SharedCpuBuild | InitPlan::SharedCpuDeploy(_) => 120,
        InitPlan::GpuDirect(_) | InitPlan::GpuStaged(_, _) => 512,
    }
}

pub(super) fn project_name(args: &InitArgs, plan: InitPlan) -> String {
    args.service_id.clone().unwrap_or_else(|| match plan {
        InitPlan::SharedCpuBuild => "allinone-builder".into(),
        InitPlan::SharedCpuDeploy(target) | InitPlan::GpuDirect(target) => {
            target.service_id().into()
        }
        InitPlan::GpuStaged(target, InitStage::Build) => format!("{}-builder", target.service_id()),
        InitPlan::GpuStaged(target, InitStage::Deploy) => target.service_id().into(),
    })
}

fn validate_sha256_digest(flag: &str, value: &str) -> Result<()> {
    let digest = value.strip_prefix("sha256:").unwrap_or_default();
    anyhow::ensure!(
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "{flag} must be a full sha256:<64-hex> digest"
    );
    Ok(())
}

pub(super) fn validate_args(args: &InitArgs, plan: InitPlan) -> Result<()> {
    let name = project_name(args, plan);
    anyhow::ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "--service-id may contain only letters, numbers, underscores and hyphens"
    );
    anyhow::ensure!(args.replicas > 0, "--replicas must be greater than zero");

    match plan {
        InitPlan::GpuDirect(_) => {
            anyhow::ensure!(
                args.harness_mode.is_none()
                    && args.image_release.is_none()
                    && args.release_digest.is_none()
                    && args.manifest.is_none()
                    && args.image_id.is_none(),
                "GPU direct build/deploy does not take published image options; pass --stage deploy to reuse an image"
            );
            return Ok(());
        }
        InitPlan::SharedCpuBuild => {
            anyhow::ensure!(
                args.harness_mode.is_none()
                    && args.image_release.is_none()
                    && args.release_digest.is_none()
                    && args.manifest.is_none()
                    && args.image_id.is_none()
                    && !args.enable_dingtalk
                    && !args.disable_pep,
                "shared CPU image build does not take instance release, mode, DingTalk, or PEP flags; configure those per deployed instance"
            );
            let disk = args.disk_gb.unwrap_or_else(|| default_disk(plan));
            anyhow::ensure!(
                disk >= 120,
                "shared CPU image requires at least 120 GB including the encrypted writable layer"
            );
            return Ok(());
        }
        InitPlan::GpuStaged(target, InitStage::Build) => {
            anyhow::ensure!(
                args.harness_mode.is_none()
                    && args.image_release.is_none()
                    && args.release_digest.is_none()
                    && args.manifest.is_none()
                    && args.image_id.is_none(),
                "build initialization does not accept published release options; use --stage deploy"
            );
            anyhow::ensure!(
                args.vllm_build_variants == "release",
                "shared-image initialization currently supports a single release variant"
            );
            let base_disk = if target == InitTarget::OpenclawVllm {
                160
            } else {
                80
            };
            anyhow::ensure!(
                args.disk_gb.unwrap_or_else(|| default_disk(plan)) > base_disk,
                "deployment disk must be larger than the {base_disk} GB base image to leave encrypted writable space"
            );
            return Ok(());
        }
        InitPlan::SharedCpuDeploy(target) => {
            if target != InitTarget::Openclaw {
                anyhow::ensure!(!args.disable_pep, "--disable-pep applies only to openclaw");
                anyhow::ensure!(
                    !args.enable_dingtalk,
                    "--enable-dingtalk applies only to openclaw"
                );
            }
            let disk = args.disk_gb.unwrap_or_else(|| default_disk(plan));
            anyhow::ensure!(
                disk >= 120,
                "shared CPU image requires at least 120 GB including the encrypted writable layer"
            );
            let (_, port) = cpu_target_mode(args, target)?;
            anyhow::ensure!(
                args.replicas == 1 || port.is_some(),
                "CLI applications have no load-balanced port; create independent instances with distinct --service-id values"
            );
        }
        InitPlan::GpuStaged(target, InitStage::Deploy) => {
            anyhow::ensure!(
                args.harness_mode.is_none(),
                "--harness-mode applies only to CPU applications"
            );
            anyhow::ensure!(
                args.vllm_build_variants == "release",
                "shared-image initialization currently supports a single release variant"
            );
            let base_disk = if target == InitTarget::OpenclawVllm {
                160
            } else {
                80
            };
            anyhow::ensure!(
                args.disk_gb.unwrap_or_else(|| default_disk(plan)) > base_disk,
                "deployment disk must be larger than the {base_disk} GB base image to leave encrypted writable space"
            );
        }
    }

    // Consumer validation (SharedCpuDeploy and GpuStaged(_, Deploy)).
    let release = args
        .image_release
        .as_ref()
        .context("instance initialization requires --image-release")?;
    anyhow::ensure!(
        release.is_file(),
        "--image-release must be an existing file"
    );
    let release_digest = args
        .release_digest
        .as_deref()
        .context("instance initialization requires --release-digest sha256:<digest>")?;
    validate_sha256_digest("--release-digest", release_digest)?;
    if let Some(pin) = &args.manifest {
        validate_sha256_digest("--manifest", pin)?;
    }
    let output = args.output_dir.join(&name);
    if args.force && output.exists() {
        let output = output.canonicalize()?;
        anyhow::ensure!(
            !release.canonicalize()?.starts_with(&output),
            "--image-release must be outside the output directory before --force replaces it"
        );
    }
    Ok(())
}

pub(super) fn pep_binary(args: &InitArgs) -> Result<PathBuf> {
    if args.stage == Some(InitStage::Deploy) {
        Ok("/usr/bin/cai-pep".into())
    } else {
        find_cai_pep_binary_for_init()
    }
}

fn cpu_mode_choices(target: InitTarget) -> (&'static [&'static str], &'static str, &'static str) {
    match target {
        InitTarget::Openclaw | InitTarget::Hermes => (
            &["gateway", "cli"],
            "gateway",
            "gateway: run the authenticated network service over TNG; cli: terminal over SSH.",
        ),
        InitTarget::Codex => (
            &["app-server", "cli"],
            "app-server",
            "app-server: run authenticated Codex app-server over TNG; cli: terminal over SSH.",
        ),
        InitTarget::Opencode | InitTarget::Dsh => (
            &["web", "cli"],
            "web",
            "web: expose authenticated Web UI via loopback TNG; cli: terminal over SSH.",
        ),
        InitTarget::Claudecode | InitTarget::Qodercli | InitTarget::Pi => (
            &["cli"],
            "cli",
            "This application runs interactively over SSH (cli).",
        ),
        InitTarget::OpenclawVllm | InitTarget::VllmInference => unreachable!("GPU target"),
    }
}

fn cpu_target_mode(args: &InitArgs, target: InitTarget) -> Result<(&'static str, Option<u16>)> {
    let (modes, default, _) = cpu_mode_choices(target);
    let requested = args.harness_mode.as_deref().unwrap_or(default);
    let &mode = modes
        .iter()
        .find(|&&m| m == requested)
        .with_context(|| format!("unsupported mode {requested} for {}", target.command_name()))?;
    let port = match (target, mode) {
        (_, "cli") => None,
        (InitTarget::Openclaw, "gateway") => Some(18789),
        (InitTarget::Hermes, "gateway") => Some(8642),
        (InitTarget::Codex, "app-server") => Some(4500),
        (InitTarget::Opencode, "web") => Some(4096),
        (InitTarget::Dsh, "web") => Some(3080),
        _ => bail!("unsupported mode {mode} for {}", target.command_name()),
    };
    Ok((mode, port))
}

pub(super) fn write_shared_cpu_build(args: &InitArgs, project: &InitProject) -> Result<()> {
    let assets = find_assets_root()?;
    let base = assets.join("examples/allinone");
    let mut spec: Value = serde_yaml::from_str(&fs::read_to_string(base.join("build.yaml"))?)?;
    spec["service"]["id"] = json!(project.service_id);
    spec["deploy"]["region"] = json!(project.region);
    spec["deploy"]["zone_id"] = json!(project.zone_id);
    spec["deploy"]["instance_type"] = json!(project.instance_type);
    spec["deploy"]["disk_gb"] = json!(project.disk_gb);
    spec["deploy"]["image_variant"] = json!("release");
    spec["build"]["variants"] = json!({"release": {"enabled": true}});
    spec["resources"] = json!({});
    spec["attestation"]["reference_values"] =
        json!(reference_values_name(project.reference_values));

    copy_dir(&base.join("files"), &project.dir.join("files"))?;
    copy_dir(
        &assets.join("examples/openclaw/files"),
        &project.dir.join("files/openclaw"),
    )?;
    for name in ["install-allinone.sh", "runtime-contract.json"] {
        fs::copy(base.join(name), project.dir.join(name))?;
    }
    set_mode(&project.dir.join("install-allinone.sh"), 0o755)?;
    for (source, name) in [
        (
            "examples/cli-agents/files/tdx-remote-attestation.SKILL.md",
            "tdx-remote-attestation.SKILL.md",
        ),
        (
            "examples/cli-agents/files/install-cli-agent-runtime.sh",
            "install-cli-agent-runtime.sh",
        ),
        (
            "examples/hermes-agent/files/install-hermes-agent-runtime.sh",
            "install-hermes-agent-runtime.sh",
        ),
    ] {
        let dst = project.dir.join("files").join(name);
        fs::copy(assets.join(source), &dst)?;
        if name.ends_with(".sh") {
            set_mode(&dst, 0o755)?;
        }
    }
    for file in spec["build"]["files"]
        .as_array_mut()
        .context("allinone build files missing")?
    {
        let source = file["source"].as_str().context("missing build source")?;
        if file["target"] == "/usr/local/bin/cai-pep" {
            file["source"] = json!(pep_binary(args)?);
        } else if source.starts_with("../") {
            let name = Path::new(source)
                .file_name()
                .context("missing source filename")?
                .to_string_lossy();
            file["source"] = if source.starts_with("../openclaw/") {
                json!(format!("./files/openclaw/{name}"))
            } else {
                json!(format!("./files/{name}"))
            };
        }
    }
    if project.reference_values == InitReferenceValues::Rekor {
        spec["attestation"]["rekor"] = json!({
            "cosign_key": project.cosign_key,
            "slsa_generator": project.slsa_generator,
            "required": true
        });
    } else {
        spec["attestation"].as_object_mut().unwrap().remove("rekor");
    }
    write_file(&project.spec_path, &serde_yaml::to_string(&spec)?, 0o644)
}

pub(super) fn write_shared_cpu_deploy(
    args: &InitArgs,
    project: &InitProject,
    target: InitTarget,
) -> Result<()> {
    let assets = find_assets_root()?;
    let base = assets.join("examples/allinone");
    let mut spec: Value = serde_yaml::from_str(&fs::read_to_string(base.join("instance.yaml"))?)?;
    spec.as_object_mut().unwrap().remove("build");
    spec["service"]["id"] = json!(project.service_id);
    spec["service"]["ssh"] = json!(true);
    spec["deploy"]["region"] = json!(project.region);
    spec["deploy"]["zone_id"] = json!(project.zone_id);
    spec["deploy"]["instance_type"] = json!(project.instance_type);
    spec["deploy"]["disk_gb"] = json!(project.disk_gb);
    spec["deploy"]["image_variant"] = json!("release");
    spec["deploy"]["replicas"] = json!(args.replicas);
    if args.replicas > 1 {
        spec["deploy"]["slb"] = json!({"enabled": true, "slb_type": "nlb"});
    } else {
        spec["deploy"].as_object_mut().unwrap().remove("slb");
    }
    spec["attestation"]["reference_values"] =
        json!(reference_values_name(project.reference_values));
    spec["attestation"].as_object_mut().unwrap().remove("rekor");

    let release_src = args
        .image_release
        .as_ref()
        .context("missing --image-release")?;
    let release_digest = args
        .release_digest
        .as_ref()
        .context("missing --release-digest")?;
    fs::copy(release_src, project.dir.join("image-release.json"))?;
    let mut image = json!({
        "release": "./image-release.json",
        "release_digest": release_digest,
    });
    if let Some(pin) = &args.manifest {
        image["manifest"] = json!(pin);
    }
    if let Some(id) = &args.image_id {
        image["image_id"] = json!(id);
    }
    spec["deploy"]["image"] = image;

    spec["resources"] = json!({});
    let harness = target.service_id();
    let (mode, port) = cpu_target_mode(args, target)?;
    let mut runtime = json!({"harness": harness, "mode": mode, "environment": {}});
    spec["service"]["ports"] = json!(port.into_iter().collect::<Vec<_>>());
    spec["service"]["connect"] = spec["service"]["ports"].clone();
    if let Some(port) = port {
        runtime["port"] = json!(port);
    }

    let key = project.dashscope_key.as_deref().unwrap_or("");
    let home = "/opt/data/allinone/home";
    match target {
        InitTarget::Codex => {
            runtime["environment"]["OPENAI_API_KEY"] = json!(key);
            let config = format!(
                "model_provider = \"Model_Studio\"\nmodel = {}\n[model_providers.Model_Studio]\nname = \"Model_Studio\"\nbase_url = {}\nenv_key = \"OPENAI_API_KEY\"\nwire_api = \"responses\"\n",
                json!(project.model),
                json!(project.dashscope_base_url)
            );
            resource(
                project,
                &mut spec,
                "codex_config",
                "config.toml",
                &format!("{home}/.codex/config.toml"),
                &config,
            )?;
            if mode == "app-server" {
                let token = args
                    .codex_app_server_token
                    .clone()
                    .or_else(|| args.gateway_token.clone())
                    .unwrap_or_else(|| random_base64(32));
                resource(
                    project,
                    &mut spec,
                    "codex_app_server_token",
                    "app-server-token",
                    &format!("{home}/.codex/app-server-token"),
                    &format!("{token}\n"),
                )?;
            }
        }
        InitTarget::Hermes => {
            runtime["environment"] = json!({"HERMES_MODEL": project.model});
            if mode == "gateway" {
                runtime["environment"]["API_SERVER_KEY"] = json!(args
                    .hermes_api_server_key
                    .clone()
                    .or_else(|| args.gateway_token.clone())
                    .unwrap_or_else(|| random_hex(32)));
            }
            let config = serde_yaml::to_string(&json!({
                "model": {
                    "provider": "custom",
                    "base_url": project.dashscope_base_url,
                    "api_key": key,
                    "default": project.model,
                    "model": project.model
                }
            }))?;
            resource(
                project,
                &mut spec,
                "hermes_config",
                "config.yaml",
                "/opt/data/allinone/hermes/config.yaml",
                &config,
            )?;
        }
        InitTarget::Openclaw => {
            if project.disable_pep {
                runtime["disable_pep"] = json!(true);
            }
            let bind = if mode == "gateway" { "lan" } else { "loopback" };
            let config = super::openclaw_json(
                project,
                false,
                Some("/opt/data/allinone/workspace"),
                bind,
                port.unwrap_or(18789),
            )?;
            resource(
                project,
                &mut spec,
                "openclaw_config",
                "openclaw.json",
                &format!("{home}/.openclaw/openclaw.json"),
                &serde_json::to_string_pretty(&config)?,
            )?;
            if mode == "gateway" {
                spec["a2a"] = json!({
                    "enabled": project.reference_values == InitReferenceValues::Rekor,
                    "id": project.service_id,
                    "name": project.service_id,
                    "version": "1.0.0",
                    "description": "OpenClaw confidential agent",
                    "skills": [{
                        "id": "chat",
                        "name": "Chat",
                        "description": "OpenClaw gateway chat"
                    }]
                });
            }
        }
        InitTarget::Claudecode => {
            runtime["environment"] = json!({
                "ANTHROPIC_AUTH_TOKEN": key,
                "ANTHROPIC_BASE_URL": project.dashscope_anthropic_base_url,
                "ANTHROPIC_MODEL": project.model,
                "ANTHROPIC_DEFAULT_HAIKU_MODEL": project.model,
                "ANTHROPIC_DEFAULT_SONNET_MODEL": project.model,
                "ANTHROPIC_DEFAULT_OPUS_MODEL": project.model,
                "CLAUDE_CODE_SUBAGENT_MODEL": project.model,
                "DISABLE_AUTOUPDATER": "1"
            });
            resource(
                project,
                &mut spec,
                "claude_onboarding",
                "claude.json",
                &format!("{home}/.claude.json"),
                "{\"hasCompletedOnboarding\":true}\n",
            )?;
        }
        InitTarget::Opencode => {
            if mode == "web" {
                let password = project
                    .gateway_token
                    .as_deref()
                    .context("OpenCode server password was not resolved")?;
                runtime["environment"]["OPENCODE_SERVER_PASSWORD"] = json!(password);
            }
            let config = json!({
                "$schema": "https://opencode.ai/config.json",
                "autoupdate": false,
                "provider": {
                    "bailian": {
                        "npm": "@ai-sdk/openai-compatible",
                        "name": "Bailian",
                        "options": {
                            "baseURL": project.dashscope_base_url,
                            "apiKey": key
                        },
                        "models": {
                            project.model.clone(): {"name": project.model}
                        }
                    }
                },
                "model": format!("bailian/{}", project.model)
            });
            resource(
                project,
                &mut spec,
                "opencode_config",
                "opencode.json",
                &format!("{home}/.config/opencode/opencode.json"),
                &serde_json::to_string_pretty(&config)?,
            )?;
        }
        InitTarget::Pi => {
            runtime["environment"]["CA_INFERENCE_API_KEY"] = json!(key);
            let models = json!({
                "providers": {
                    "inference": {
                        "baseUrl": project.dashscope_base_url,
                        "api": "openai-completions",
                        "apiKey": "$CA_INFERENCE_API_KEY",
                        "models": [{"id": project.model}]
                    }
                }
            });
            resource(
                project,
                &mut spec,
                "pi_models",
                "pi-models.json",
                &format!("{home}/.pi/agent/models.json"),
                &serde_json::to_string_pretty(&models)?,
            )?;
            let settings = json!({"defaultProvider": "inference", "defaultModel": project.model});
            resource(
                project,
                &mut spec,
                "pi_settings",
                "pi-settings.json",
                &format!("{home}/.pi/agent/settings.json"),
                &serde_json::to_string_pretty(&settings)?,
            )?;
        }
        InitTarget::Dsh => {
            runtime["environment"]["CA_INFERENCE_API_KEY"] = json!(key);
            let patches = json!([
                {
                    "id": "llm-pi-ai",
                    "config": {
                        "providers": {
                            "inference": {
                                "api": "openai-completions",
                                "baseURL": project.dashscope_base_url,
                                "apiKeyEnv": "CA_INFERENCE_API_KEY",
                                "models": [{"id": project.model}]
                            }
                        }
                    }
                },
                {
                    "id": "agent-default-model",
                    "config": {"provider": "inference", "model": project.model}
                }
            ]);
            resource(
                project,
                &mut spec,
                "dsh_config",
                "cordis.patch.yml",
                &format!("{home}/.dsh/cordis.patch.yml"),
                &serde_yaml::to_string(&patches)?,
            )?;
        }
        InitTarget::Qodercli => {
            // Qoder account login is performed in the guest, not inferred from a model API key.
        }
        InitTarget::OpenclawVllm | InitTarget::VllmInference => unreachable!("GPU target"),
    }
    resource(
        project,
        &mut spec,
        "runtime",
        "runtime.json",
        "/opt/data/allinone/runtime.json",
        &serde_json::to_string_pretty(&runtime)?,
    )?;
    write_file(&project.spec_path, &serde_yaml::to_string(&spec)?, 0o644)
}

fn resource(
    project: &InitProject,
    spec: &mut Value,
    name: &str,
    file: &str,
    target: &str,
    content: &str,
) -> Result<()> {
    let secrets = project.dir.join("secrets");
    fs::create_dir_all(&secrets)?;
    set_mode(&secrets, 0o700)?;
    write_secret_file(&secrets.join(file), content)?;
    spec["resources"][name] = json!({"source": format!("./secrets/{file}"), "target": target, "mode": "0600", "required": true});
    Ok(())
}

pub(super) struct GpuRelease {
    bytes: Vec<u8>,
    mode: u32,
    ssh: bool,
}

pub(super) fn read_gpu_release(args: &InitArgs) -> Result<GpuRelease> {
    let release_digest = args.release_digest.as_ref().unwrap();
    let mut file = File::open(args.image_release.as_ref().unwrap())?;
    let mode = file.metadata()?.permissions().mode() & 0o777;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let actual_digest = format!("sha256:{}", hex_encode(&Sha256::digest(&bytes)));
    anyhow::ensure!(
        actual_digest.eq_ignore_ascii_case(release_digest),
        "release digest mismatch: expected {release_digest}, found {actual_digest}"
    );
    let mut release: Value = serde_json::from_slice(&bytes)?;
    if release["payloadType"] == "application/vnd.shelter.cloud-image-release.v1+json" {
        let payload = release["payload"]
            .as_str()
            .context("missing release payload")?;
        release = serde_json::from_slice(&BASE64_STANDARD.decode(payload)?)?;
    }
    let ssh = release["runtime_metadata"]["capabilities"]
        .as_array()
        .is_some_and(|values| values.iter().any(|value| value == "instance-ssh-v1"));
    Ok(GpuRelease { bytes, mode, ssh })
}

pub(super) fn finish_gpu(
    args: &InitArgs,
    project: &InitProject,
    release: Option<&GpuRelease>,
) -> Result<()> {
    let InitPlan::GpuStaged(_, _) = project.plan else {
        return Ok(());
    };
    let mut spec: Value = serde_yaml::from_str(&fs::read_to_string(&project.spec_path)?)?;
    spec["service"]["id"] = json!(project.service_id);
    if spec["a2a"].is_object() {
        spec["a2a"]["id"] = spec["service"]["id"].clone();
        spec["a2a"]["name"] = spec["service"]["id"].clone();
        if project.reference_values == InitReferenceValues::Sample {
            spec["a2a"]["enabled"] = json!(false);
        }
    }
    spec["deploy"]["image_variant"] = json!("release");
    if let Some(release) = release {
        spec.as_object_mut().unwrap().remove("build");
        spec["deploy"]["replicas"] = json!(args.replicas);
        if args.replicas > 1 && spec["deploy"]["slb"].is_null() {
            spec["deploy"]["slb"] = json!({"enabled": true, "slb_type": "nlb"});
        }
        spec["service"]["ssh"] = json!(release.ssh);
        write_private_file(
            &project.dir.join("image-release.json"),
            &release.bytes,
            release.mode,
        )?;
        let mut image = json!({
            "release": "./image-release.json",
            "release_digest": args.release_digest.as_ref().unwrap(),
        });
        if let Some(pin) = &args.manifest {
            image["manifest"] = json!(pin);
        }
        if let Some(id) = &args.image_id {
            image["image_id"] = json!(id);
        }
        spec["deploy"]["image"] = image;
    } else {
        spec["service"]["ssh"] = json!(true);
        add_management_ssh(project, &mut spec)?;
        spec["build"]["variants"] = json!({"release": {"enabled": true}});
        spec["resources"] = json!({});
        spec["deploy"].as_object_mut().unwrap().remove("replicas");
        spec["deploy"].as_object_mut().unwrap().remove("slb");
    }
    write_file(&project.spec_path, &serde_yaml::to_string(&spec)?, 0o644)
}

fn add_management_ssh(project: &InitProject, spec: &mut Value) -> Result<()> {
    let source = find_assets_root()?.join("examples/allinone/files");
    let directory = project.dir.join("management-ssh");
    fs::create_dir_all(&directory)?;
    for (name, target) in [
        ("cai-ssh.service", "/etc/systemd/system/cai-ssh.service"),
        ("cai-ssh.path", "/etc/systemd/system/cai-ssh.path"),
        (
            "sshd_config",
            "/usr/local/libexec/confidential-agent/ssh/sshd_config",
        ),
    ] {
        let content = fs::read_to_string(source.join(name))?
            .replace("/allinone/sshd_config", "/ssh/sshd_config");
        write_file(&directory.join(name), &content, 0o644)?;
        spec["build"]["files"]
            .as_array_mut()
            .context("missing build files")?
            .push(json!({"source": format!("./management-ssh/{name}"), "target": target}));
    }
    let packages = spec["build"]["packages"]
        .as_array_mut()
        .context("missing packages")?;
    if !packages.iter().any(|p| p == "openssh-server") {
        packages.push(json!("openssh-server"));
    }
    let script = r#"#!/bin/bash
set -euo pipefail
install -d -m 0755 /etc/systemd/system-preset
cat > /etc/systemd/system-preset/00-cai-management-ssh.preset <<'PRESET'
disable sshd.service
disable sshd.socket
enable cai-ssh.path
PRESET
for unit in sshd.service sshd.socket; do
    if [[ -f /usr/lib/systemd/system/$unit || -f /etc/systemd/system/$unit ]]; then
        systemctl disable "$unit"
    fi
done
rm -f /etc/ssh/ssh_host_* /root/.ssh/authorized_keys
systemctl enable cai-ssh.path
"#;
    write_file(&directory.join("install.sh"), script, 0o755)?;
    spec["build"]["scripts"]
        .as_array_mut()
        .context("missing scripts")?
        .push(json!("./management-ssh/install.sh"));
    Ok(())
}

pub(super) fn write_next_steps(
    cli: &Cli,
    args: &InitArgs,
    project: &InitProject,
    ssh: bool,
) -> Result<()> {
    let name = &project.service_id;
    let steps = if matches!(
        project.plan,
        InitPlan::SharedCpuBuild | InitPlan::GpuStaged(_, InitStage::Build)
    ) {
        let (consumer_example, reuse_note) = match project.plan {
            InitPlan::GpuStaged(target, _) => (
                format!("confidential-agent init {} --stage deploy", target.command_name()),
                format!("Use this dedicated GPU release for `{}` instances.", target.command_name()),
            ),
            _ => (
                "confidential-agent init openclaw".into(),
                "Reuse this same shared CPU release across openclaw, hermes, codex, claude-code, qodercli, opencode, pi, and dsh without rebuilding.".into(),
            ),
        };
        format!(
            r#"# Build and publish {name}

Run these commands from this directory. No model credentials belong in this image.
Review build.yaml, then configure cloud credentials with /usr/libexec/cagent/bin/aliyun configure.

```bash
confidential-agent spec validate --spec build.yaml
confidential-agent build --spec build.yaml
confidential-agent image publish {name} --spec build.yaml
confidential-agent image export {name} --output image-release.json
```

`image export` prints `release_digest=sha256:...` for this release document.
For an already available shared/marketplace image, add --region REGION --image-id IMAGE_ID to image export; no upload or import is performed.
Initialize instances with `{consumer_example} --image-release ./image-release.json --release-digest sha256:...` (or a digest from the versioned official Release index).
Optional --manifest pins a specific manifest digest; --service-id sets a distinct instance name when running multiple instances.
{reuse_note}
Changing software versions requires a new build and release. Changing model/provider configuration does not require rebuilding.
Destroying a consumer preserves the shared image.
"#
        )
    } else {
        let access = match project.plan {
            InitPlan::SharedCpuDeploy(target) => cpu_access_steps(args, target, name)?,
            _ => {
                let mut access = format!(
                    r#"Start the TNG tunnel and read the mapped local port.

```bash
confidential-agent connect start --service {name} --ready-json connect-ready.json
LOCAL_PORT="$(jq -r '.client_endpoints[0].local_port' connect-ready.json)"
```
"#
                );
                if let InitPlan::GpuStaged(InitTarget::OpenclawVllm, _) = project.plan {
                    access.push_str(
                        r#"
Open the displayed URL and use the gateway token in `openclaw-vllm.json`.

```bash
printf '%s\n' "http://127.0.0.1:$LOCAL_PORT/openclaw"
openclaw tui --url "ws://127.0.0.1:$LOCAL_PORT" --token "$(jq -r '.gateway.auth.token' openclaw-vllm.json)"
```
"#,
                    );
                } else {
                    access.push_str("\nQuery the vLLM model list through the OHTTP tunnel.\n\n```bash\ncurl -fsS \"http://127.0.0.1:$LOCAL_PORT/v1/models\"\n```\n");
                }
                if ssh {
                    access.push_str(&format!(
                        "\n```bash\nconfidential-agent ssh {name} --tty\n```\n"
                    ));
                }
                access
            }
        };
        let ssh_note = if ssh {
            "Management SSH uses credentials delivered after attestation.\n\n"
        } else {
            ""
        };
        format!(
            r#"# Deploy {name} from a published image

Run these commands from this directory. Review instance.yaml and the local resource files it lists.
CA authenticates image-release.json against deploy.image.release_digest and verifies runtime TDX attestation.
Use the published image to deploy this project.

```bash
confidential-agent peering add --role operator --cidr YOUR_PUBLIC_IP/32 --label operator
confidential-agent spec validate --spec instance.yaml
confidential-agent deploy --spec instance.yaml
confidential-agent status --live
confidential-agent report --json
```

{ssh_note}{access}
Send a model request to check the configured provider after the application is ready.
Run init again with a different --service-id to create an independent instance from the same release.
Network services support --replicas N for load-balanced replicas; CLI applications use distinct --service-id values instead.
Use `confidential-agent inject --spec instance.yaml --target-ip INSTANCE_IP` to update resources on an existing instance;
port/mode changes affect deployment topology, so create a new instance for those changes.

```bash
confidential-agent destroy {name}
```
"#
        )
    };
    write_project_next_steps(cli, project, &steps)
}

fn cpu_access_steps(args: &InitArgs, target: InitTarget, name: &str) -> Result<String> {
    let (mode, port) = cpu_target_mode(args, target)?;
    let mut steps = format!("```bash\nconfidential-agent ssh {name} -- cai-agent --check\n```\n");
    if port.is_some() {
        steps.push_str(&format!(
            "\nStart the tunnel and set `LOCAL_PORT` to the mapped port it reports.\n\n```bash\nconfidential-agent connect start --service {name}\n```\n"
        ));
    }
    steps.push_str(&match (target, mode) {
        (InitTarget::Openclaw, "gateway") => r#"
Open `http://127.0.0.1:LOCAL_PORT/openclaw` and use the gateway token from `secrets/openclaw.json`.

```bash
openclaw tui --url "ws://127.0.0.1:$LOCAL_PORT" --token "$(jq -r .gateway.auth.token secrets/openclaw.json)"
```
"#.into(),
        (InitTarget::Hermes, "gateway") => "\nSend requests to `http://127.0.0.1:LOCAL_PORT/v1/chat/completions` with the `API_SERVER_KEY` from `secrets/runtime.json` as a Bearer token.\n".into(),
        (InitTarget::Codex, "app-server") => r#"
```bash
CODEX_REMOTE_TOKEN="$(cat secrets/app-server-token)" codex --remote "ws://127.0.0.1:$LOCAL_PORT" --remote-auth-token-env CODEX_REMOTE_TOKEN
```
"#.into(),
        (InitTarget::Opencode, "web") => r#"
Open `http://127.0.0.1:LOCAL_PORT` and sign in as `opencode` with `OPENCODE_SERVER_PASSWORD` from `secrets/runtime.json`, or attach from a terminal.

```bash
OPENCODE_SERVER_USERNAME=opencode OPENCODE_SERVER_PASSWORD="$(jq -r .environment.OPENCODE_SERVER_PASSWORD secrets/runtime.json)" opencode attach "http://127.0.0.1:$LOCAL_PORT"
```
"#.into(),
        (InitTarget::Dsh, "web") => format!(
            "\nOpen the login URL returned by this SSH command.\n\n```bash\nconfidential-agent ssh {name} -- cai-agent --web-url --port \"$LOCAL_PORT\"\n```\n"
        ),
        (InitTarget::Dsh, "cli") => format!(
            "\n```bash\nconfidential-agent ssh {name} -- cai-agent headless 'Your task'\n```\n"
        ),
        _ => format!("\n```bash\nconfidential-agent ssh {name} --tty -- cai-agent\n```\n"),
    });
    if target == InitTarget::Qodercli {
        steps.push_str("\nSign in to Qoder and complete its `/model` wizard in the guest.\n");
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn run_args(temp: &Path, args_slice: &[&str]) -> Result<PathBuf> {
        let output = temp.join("projects");
        let mut argv = vec![
            "cagent",
            "init",
            "--non-interactive",
            "--reference-values",
            "sample",
            "--output-dir",
            output.to_str().unwrap(),
        ];
        argv.extend(args_slice);
        let cli = Cli::try_parse_from(argv)?;
        let Commands::Init(args) = &cli.command else {
            unreachable!()
        };
        let mut resolved = args.clone();
        let plan = resolve_plan_and_interactive(&mut resolved)?;
        cmd_init(&cli, args)?;
        Ok(output.join(project_name(&resolved, plan)))
    }

    #[test]
    fn cpu_mode_choices_match_standard_application_modes() {
        assert_eq!(cpu_mode_choices(InitTarget::Openclaw).1, "gateway");
        assert_eq!(cpu_mode_choices(InitTarget::Hermes).1, "gateway");
        assert_eq!(cpu_mode_choices(InitTarget::Codex).1, "app-server");
        assert_eq!(cpu_mode_choices(InitTarget::Opencode).1, "web");
        assert_eq!(cpu_mode_choices(InitTarget::Dsh).1, "web");
        assert_eq!(cpu_mode_choices(InitTarget::Claudecode).0, &["cli"]);
        assert_eq!(cpu_mode_choices(InitTarget::Qodercli).0, &["cli"]);
        assert_eq!(cpu_mode_choices(InitTarget::Pi).0, &["cli"]);
    }

    #[test]
    fn release_digest_init_writes_authenticated_digest_and_preserves_state_dir_in_next_steps() {
        let temp = tempfile::tempdir().unwrap();
        let release = temp.path().join("release.json");
        fs::write(&release, "fixture").unwrap();
        let digest = format!("sha256:{}", "b".repeat(64));
        let state = temp.path().join("state with spaces and 'quote");
        let output = run_args(
            temp.path(),
            &[
                "codex",
                "--service-id",
                "portable-consumer",
                "--dashscope-api-key",
                "test-model-key",
                "--image-release",
                release.to_str().unwrap(),
                "--release-digest",
                &digest,
                "--image-id",
                "m-marketplace",
                "--state-dir",
                state.to_str().unwrap(),
            ],
        )
        .unwrap();
        let spec = AgentSpec::from_path(&output.join("instance.yaml")).unwrap();
        let image = spec.deploy.image.unwrap();
        assert_eq!(image.release_digest.as_deref(), Some(digest.as_str()));
        assert!(!image.skip_signature_verification);
        assert!(image.manifest.is_none());
        assert!(image.trusted_key.as_os_str().is_empty());
        assert_eq!(image.image_id.as_deref(), Some("m-marketplace"));
        assert_eq!(spec.service.ports, vec![4500]);
        assert!(!output.join("publisher.pem").exists());
        let yaml = fs::read_to_string(output.join("instance.yaml")).unwrap();
        assert!(!yaml.contains("trusted_key"));
        assert!(!yaml.contains("skip_signature_verification"));
        assert!(!yaml.contains("account_id"));
        let steps = fs::read_to_string(output.join("NEXT_STEPS.md")).unwrap();
        assert!(steps.contains(&format!("cd {}", shell_path(&output))));
        assert!(steps.contains(&format!(
            "confidential-agent --state-dir {} deploy --spec instance.yaml",
            shell_path(&state)
        )));
    }

    #[test]
    fn gpu_consumers_use_rekor_and_only_request_advertised_instance_ssh() {
        let temp = tempfile::tempdir().unwrap();
        let release = temp.path().join("image-release.json");
        let model_key = temp.path().join("model.key");
        fs::write(&model_key, "test-model-key\n").unwrap();

        for (target, access, ssh) in [
            ("openclaw-vllm", "/openclaw", false),
            ("vllm-inference", "/v1/models", false),
            ("openclaw-vllm", "/openclaw", true),
            ("vllm-inference", "/v1/models", true),
        ] {
            let capabilities = if ssh { vec!["instance-ssh-v1"] } else { vec![] };
            let mut document = json!({"runtime_metadata": {"capabilities": capabilities}});
            if ssh {
                document = json!({
                    "payloadType": "application/vnd.shelter.cloud-image-release.v1+json",
                    "payload": BASE64_STANDARD.encode(serde_json::to_vec(&document).unwrap()),
                    "signatures": []
                });
            }
            fs::write(&release, serde_json::to_vec(&document).unwrap()).unwrap();
            let digest = format!("sha256:{}", sha256_file(&release).unwrap());
            let output = temp.path().join(if ssh { "managed" } else { "direct" });
            let mut argv = vec![
                "cagent",
                "init",
                target,
                "--stage",
                "deploy",
                "--non-interactive",
                "--output-dir",
                output.to_str().unwrap(),
                "--image-release",
                release.to_str().unwrap(),
                "--release-digest",
                &digest,
            ];
            if target == "vllm-inference" {
                argv.extend([
                    "--oss-bucket",
                    "test-models:/qwen",
                    "--model-key-file",
                    model_key.to_str().unwrap(),
                ]);
            }
            let cli = Cli::try_parse_from(argv).unwrap();
            let Commands::Init(args) = &cli.command else {
                unreachable!()
            };
            cmd_init(&cli, args).unwrap();

            let project = output.join(target);
            let spec = AgentSpec::from_path(&project.join("instance.yaml")).unwrap();
            assert_eq!(spec.attestation.reference_values, ReferenceValueMode::Rekor);
            assert!(spec.attestation.rekor.is_none());
            assert!(!project.join("cosign.key").exists());
            assert_eq!(spec.service.ssh, ssh, "{target}");
            let image = spec.deploy.image.unwrap();
            assert_eq!(image.release_digest.as_deref(), Some(digest.as_str()));
            assert_eq!(
                format!("sha256:{}", sha256_file(&image.release).unwrap()),
                digest
            );
            let steps = fs::read_to_string(project.join("NEXT_STEPS.md")).unwrap();
            assert!(steps.contains("connect start"));
            assert!(steps.contains(".client_endpoints[0].local_port"));
            assert!(steps.contains(access));
            assert_eq!(steps.contains("--tty"), ssh, "{target}");

            if target == "openclaw-vllm" && !ssh {
                let marker = project.join("operator-notes.txt");
                fs::write(&marker, "retain me").unwrap();
                let mut invalid = args.clone();
                invalid.force = true;
                invalid.release_digest = Some(format!("sha256:{}", "0".repeat(64)));
                let error = cmd_init(&cli, &invalid).unwrap_err();
                assert!(error.to_string().contains("release digest mismatch"));
                assert_eq!(fs::read_to_string(marker).unwrap(), "retain me");
                invalid.service_id = Some("invalid-release".into());
                assert!(cmd_init(&cli, &invalid).is_err());
                assert!(!output.join("invalid-release").exists());
            }
        }
    }

    #[test]
    fn untargeted_and_targeted_cpu_builders_generate_the_same_shared_image_builder() {
        let _guard = super::super::tests::ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let pep = temp.path().join("pep");
        fs::write(&pep, "fake guest asset").unwrap();
        let prev = std::env::var_os("CA_PEP_BIN");
        std::env::set_var("CA_PEP_BIN", &pep);

        let untargeted = run_args(temp.path(), &["--stage", "build"]).unwrap();
        assert_eq!(untargeted.file_name().unwrap(), "allinone-builder");
        let spec = AgentSpec::from_path(&untargeted.join("build.yaml")).unwrap();
        assert_eq!(spec.service.id, "allinone-builder");
        assert!(spec.resources.is_empty());
        assert!(spec.service.ssh);
        assert!(spec.deploy.image.is_none());
        assert!(!untargeted.join("secrets").exists());
        assert_eq!(spec.image_variant(), "release");
        assert!(spec.build.variants.debug.is_none());
        let steps = fs::read_to_string(untargeted.join("NEXT_STEPS.md")).unwrap();
        assert!(steps.contains("shared CPU release across openclaw, hermes, codex, claude-code, qodercli, opencode, pi, and dsh"));
        for home in [
            "home/.agents",
            "home/.claude",
            "home/.qoder",
            "home/.config/opencode",
            "home/.pi/agent",
            "home/.dsh",
            "home/.openclaw",
            "hermes",
        ] {
            let dest = format!("/opt/data/allinone/{home}/skills/tdx-remote-attestation/SKILL.md");
            assert!(spec.build.files.iter().any(|f| f.target == dest), "{home}");
        }
        for name in [
            "install-openclaw-runtime.sh",
            "patch-openclaw-cai-pep.js",
            "cai-pep-plugin",
            "cai-a2a-plugin",
            "cai-pep-default-policy.json",
        ] {
            assert!(
                spec.build.files.iter().any(|f| f.target.ends_with(name)),
                "missing {name}"
            );
        }
        for file in &spec.build.files {
            assert!(file.source.exists(), "{}", file.source.display());
        }
        for file in &spec.build.scripts {
            assert!(file.exists(), "{}", file.display());
        }
        assert!(spec.build.runtime_contract.as_ref().unwrap().is_file());

        // Targeted CPU `--stage build` selects the exact same shared build.
        let targeted = run_args(
            temp.path(),
            &[
                "openclaw",
                "--stage",
                "build",
                "--service-id",
                "shared-cpu-builder",
            ],
        )
        .unwrap();
        let targeted_spec = AgentSpec::from_path(&targeted.join("build.yaml")).unwrap();
        assert_eq!(targeted_spec.build.image_name, "allinone");
        assert_eq!(targeted_spec.service.id, "shared-cpu-builder");

        // GPU staged builders remain dedicated to their GPU images.
        for gpu in ["openclaw-vllm", "vllm-inference"] {
            let gpu_dir = run_args(temp.path(), &[gpu, "--stage", "build"]).unwrap();
            let gpu_spec = AgentSpec::from_path(&gpu_dir.join("build.yaml")).unwrap();
            assert_ne!(gpu_spec.build.image_name, "allinone");
            assert!(gpu_spec.resources.is_empty());
            let steps = fs::read_to_string(gpu_dir.join("NEXT_STEPS.md")).unwrap();
            assert!(steps.contains(&format!("dedicated GPU release for `{gpu}` instances")));
        }

        assert!(run_args(temp.path(), &["--stage", "build", "--disk-gb", "80"]).is_err());

        if let Some(prev) = prev {
            std::env::set_var("CA_PEP_BIN", prev);
        } else {
            std::env::remove_var("CA_PEP_BIN");
        }
    }

    #[test]
    fn cpu_applications_default_to_app_service_ids_and_standard_modes() {
        let temp = tempfile::tempdir().unwrap();
        let release = temp.path().join("release.json");
        fs::write(&release, "release-bytes").unwrap();
        let release_digest = format!("sha256:{}", "c".repeat(64));
        let manifest_digest = format!("sha256:{}", "a".repeat(64));

        let expected: &[(&str, &str, Option<u16>)] = &[
            ("openclaw", "gateway", Some(18789)),
            ("hermes", "gateway", Some(8642)),
            ("codex", "app-server", Some(4500)),
            ("opencode", "web", Some(4096)),
            ("dsh", "web", Some(3080)),
            ("claude-code", "cli", None),
            ("qodercli", "cli", None),
            ("pi", "cli", None),
        ];

        for &(target, expected_mode, expected_port) in expected {
            let dir = run_args(
                temp.path(),
                &[
                    target,
                    "--image-release",
                    release.to_str().unwrap(),
                    "--release-digest",
                    &release_digest,
                    "--manifest",
                    &manifest_digest,
                    "--dashscope-api-key",
                    "test-private-model-key",
                    "--gateway-token",
                    "0123456789abcdef0123456789abcdef",
                ],
            )
            .unwrap();
            assert_eq!(dir.file_name().unwrap(), target);
            let spec = AgentSpec::from_path(&dir.join("instance.yaml")).unwrap();
            assert_eq!(spec.service.id, target);
            assert!(spec.build.image_name.is_empty());
            assert!(!dir.join("files").exists());
            assert!(spec.service.ssh);
            assert_eq!(
                spec.deploy
                    .image
                    .as_ref()
                    .unwrap()
                    .release_digest
                    .as_deref(),
                Some(release_digest.as_str())
            );
            assert_eq!(
                spec.service.ports,
                expected_port.into_iter().collect::<Vec<_>>(),
                "{target}"
            );
            let runtime: Value =
                serde_json::from_slice(&fs::read(&spec.resources["runtime"].source).unwrap())
                    .unwrap();
            assert_eq!(runtime["harness"], target);
            assert_eq!(runtime["mode"], expected_mode);

            if target == "openclaw" {
                let config: Value = serde_json::from_slice(
                    &fs::read(&spec.resources["openclaw_config"].source).unwrap(),
                )
                .unwrap();
                assert_eq!(config["gateway"]["controlUi"]["basePath"], "/openclaw");
                assert_eq!(config["gateway"]["bind"], "lan");
                assert_eq!(
                    config["plugins"]["entries"]["cai-pep"]["config"]["pepRequired"],
                    true
                );
            }
            if target == "opencode" {
                assert_eq!(
                    runtime["environment"]["OPENCODE_SERVER_PASSWORD"],
                    "0123456789abcdef0123456789abcdef"
                );
            }
            if target == "dsh" {
                let steps = fs::read_to_string(dir.join("NEXT_STEPS.md")).unwrap();
                assert!(steps.contains("cai-agent --web-url"));
            }
        }

        // OpenClaw default reference_values is Rekor and enables A2A AgentCard; --disable-pep works per-instance.
        let rekor_dir = {
            let output = temp.path().join("rekor-projects");
            let cli = Cli::try_parse_from([
                "cagent",
                "init",
                "openclaw",
                "--stage",
                "deploy",
                "--non-interactive",
                "--output-dir",
                output.to_str().unwrap(),
                "--service-id",
                "claw-no-pep",
                "--disable-pep",
                "--enable-dingtalk",
                "--dingtalk-client-id",
                "dt-id",
                "--dingtalk-client-secret",
                "dt-secret",
                "--image-release",
                release.to_str().unwrap(),
                "--release-digest",
                &release_digest,
                "--dashscope-api-key",
                "sk-test",
            ])
            .unwrap();
            let Commands::Init(args) = &cli.command else {
                unreachable!()
            };
            cmd_init(&cli, args).unwrap();
            output.join("claw-no-pep")
        };
        let rekor_spec = AgentSpec::from_path(&rekor_dir.join("instance.yaml")).unwrap();
        let a2a = rekor_spec.a2a.expect("OpenClaw gateway enables A2A");
        assert!(a2a.enabled);
        assert_eq!(a2a.id, "claw-no-pep");
        let runtime: Value =
            serde_json::from_slice(&fs::read(&rekor_spec.resources["runtime"].source).unwrap())
                .unwrap();
        assert_eq!(runtime["disable_pep"], true);
        let claw_cfg: Value = serde_json::from_slice(
            &fs::read(&rekor_spec.resources["openclaw_config"].source).unwrap(),
        )
        .unwrap();
        assert!(claw_cfg["plugins"]["entries"].get("cai-pep").is_none());
        assert_eq!(claw_cfg["channels"]["dingtalk"]["clientId"], "dt-id");

        // Reject --disable-pep on non-OpenClaw targets and replicas > 1 on CLI mode.
        assert!(run_args(
            temp.path(),
            &[
                "codex",
                "--disable-pep",
                "--image-release",
                release.to_str().unwrap(),
                "--release-digest",
                &release_digest,
                "--dashscope-api-key",
                "sk-test",
            ],
        )
        .is_err());
        assert!(run_args(
            temp.path(),
            &[
                "claude-code",
                "--replicas",
                "2",
                "--image-release",
                release.to_str().unwrap(),
                "--release-digest",
                &release_digest,
                "--dashscope-api-key",
                "sk-test",
            ],
        )
        .is_err());
        assert!(run_args(
            temp.path(),
            &[
                "codex",
                "--service-id",
                "../escape",
                "--image-release",
                release.to_str().unwrap(),
                "--release-digest",
                &release_digest,
                "--dashscope-api-key",
                "sk-test",
            ],
        )
        .is_err());
    }
}
