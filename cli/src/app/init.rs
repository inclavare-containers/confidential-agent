use super::*;

mod staged;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InitPlan {
    SharedCpuBuild,
    SharedCpuDeploy(InitTarget),
    GpuDirect(InitTarget),
    GpuStaged(InitTarget, InitStage),
}

#[derive(Debug)]
struct InitProject {
    plan: InitPlan,
    service_id: String,
    dir: PathBuf,
    spec_path: PathBuf,
    region: String,
    zone_id: String,
    instance_type: String,
    disk_gb: u32,
    replicas: u32,
    reference_values: InitReferenceValues,
    cosign_key: Option<PathBuf>,
    slsa_generator: PathBuf,
    dashscope_key: Option<String>,
    dashscope_base_url: String,
    dashscope_anthropic_base_url: String,
    model: String,
    gateway_token: Option<String>,
    disable_pep: bool,
    enable_dingtalk: bool,
    dingtalk_client_id: Option<String>,
    dingtalk_client_secret: Option<String>,
    vllm_model_id: String,
    vllm_model_dir: String,
    vllm_served_model_name: String,
    vllm_port: u16,
    vllm_version: String,
    vllm_build_variants: VllmBuildVariants,
    oss_bucket: String,
    oss_endpoint: String,
    model_key_file: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VllmBuildVariants {
    release: bool,
    debug: bool,
}

impl Default for VllmBuildVariants {
    fn default() -> Self {
        Self {
            release: true,
            debug: false,
        }
    }
}

impl VllmBuildVariants {
    fn deploy_variant(self) -> &'static str {
        if self.debug {
            "debug"
        } else {
            "release"
        }
    }
}

pub(super) fn cmd_init(cli: &Cli, args: &InitArgs) -> Result<()> {
    let mut resolved_args = args.clone();
    let plan = staged::resolve_plan_and_interactive(&mut resolved_args)?;
    let args = &resolved_args;

    staged::validate_args(args, plan)?;
    let mut project = resolve_project(args, plan)?;
    guard_output_dir(&project.dir, args.force)?;
    fs::create_dir_all(&project.dir)
        .with_context(|| format!("failed to create '{}'", project.dir.display()))?;
    set_mode(&project.dir, 0o755)?;
    if matches!(
        plan,
        InitPlan::SharedCpuBuild
            | InitPlan::GpuDirect(_)
            | InitPlan::GpuStaged(_, InitStage::Build)
    ) && project.reference_values == InitReferenceValues::Rekor
    {
        project.cosign_key = Some(resolve_or_generate_cosign_key(cli, args, &project)?);
    }

    match plan {
        InitPlan::SharedCpuBuild => staged::write_shared_cpu_build(args, &project)?,
        InitPlan::SharedCpuDeploy(target) => {
            staged::write_shared_cpu_deploy(args, &project, target)?
        }
        InitPlan::GpuDirect(InitTarget::OpenclawVllm)
        | InitPlan::GpuStaged(InitTarget::OpenclawVllm, _) => {
            write_openclaw_vllm_project(cli, args, &project)?;
            staged::finish_gpu(args, &project)?;
        }
        InitPlan::GpuDirect(InitTarget::VllmInference)
        | InitPlan::GpuStaged(InitTarget::VllmInference, _) => {
            write_vllm_inference_project(cli, args, &project)?;
            staged::finish_gpu(args, &project)?;
        }
        _ => unreachable!("unexpected GPU target in plan"),
    }

    AgentSpec::from_path(&project.spec_path).with_context(|| {
        format!(
            "generated AppSpec '{}' did not parse",
            project.spec_path.display()
        )
    })?;
    match plan {
        InitPlan::GpuDirect(_) => write_next_steps(cli, &project)?,
        _ => staged::write_next_steps(cli, args, &project)?,
    }
    println!("[ca] init generated {}", project.dir.display());
    println!("[ca] AppSpec: {}", project.spec_path.display());
    println!(
        "[ca] Next steps: {}",
        project.dir.join("NEXT_STEPS.md").display()
    );
    Ok(())
}

fn resolve_project(args: &InitArgs, plan: InitPlan) -> Result<InitProject> {
    if matches!(
        plan,
        InitPlan::GpuDirect(InitTarget::OpenclawVllm)
            | InitPlan::GpuStaged(InitTarget::OpenclawVllm, _)
    ) && args.disable_pep
    {
        bail!("init openclaw-vllm does not support --disable-pep because the TDX attestation skill uses cai-pep attest");
    }
    let service_id = staged::project_name(args, plan);
    let dir = args.output_dir.join(&service_id);
    let spec_path = dir.join(match plan {
        InitPlan::SharedCpuBuild | InitPlan::GpuStaged(_, InitStage::Build) => "build.yaml",
        InitPlan::SharedCpuDeploy(_) | InitPlan::GpuStaged(_, InitStage::Deploy) => "instance.yaml",
        InitPlan::GpuDirect(target) => target.spec_name(),
    });
    let region = args
        .region
        .clone()
        .unwrap_or_else(|| "cn-beijing".to_string());
    let zone_id = args
        .zone_id
        .clone()
        .unwrap_or_else(|| default_zone_id(&region).to_string());
    let instance_type = args
        .instance_type
        .clone()
        .unwrap_or_else(|| default_instance_type(&region, plan).to_string());
    let disk_gb = args.disk_gb.unwrap_or_else(|| staged::default_disk(plan));
    let reference_values = args.reference_values.unwrap_or(InitReferenceValues::Rekor);
    let dashscope_key = resolve_dashscope_key(args, plan)?;
    let model = args
        .model
        .clone()
        .unwrap_or_else(|| confidential_agent_core::software::version("bailian_model"));
    if let Some(token) = args.gateway_token.as_deref() {
        if token.len() < 32 {
            bail!("--gateway-token must be at least 32 characters");
        }
    }
    let gateway_token = match plan {
        InitPlan::SharedCpuDeploy(InitTarget::Openclaw)
        | InitPlan::SharedCpuDeploy(InitTarget::Opencode)
        | InitPlan::GpuDirect(InitTarget::OpenclawVllm)
        | InitPlan::GpuStaged(InitTarget::OpenclawVllm, InitStage::Deploy) => {
            Some(args.gateway_token.clone().unwrap_or_else(|| random_hex(20)))
        }
        _ => args.gateway_token.clone(),
    };
    let (enable_dingtalk, dingtalk_client_id, dingtalk_client_secret) = match plan {
        InitPlan::SharedCpuDeploy(InitTarget::Openclaw)
        | InitPlan::GpuDirect(InitTarget::OpenclawVllm)
        | InitPlan::GpuStaged(InitTarget::OpenclawVllm, InitStage::Deploy) => {
            resolve_dingtalk_credentials(args)?
        }
        _ => (false, None, None),
    };
    let is_gpu = matches!(plan, InitPlan::GpuDirect(_) | InitPlan::GpuStaged(_, _));
    let is_vllm_inference = matches!(
        plan,
        InitPlan::GpuDirect(InitTarget::VllmInference)
            | InitPlan::GpuStaged(InitTarget::VllmInference, _)
    );
    let vllm_build_variants = if is_gpu {
        parse_vllm_build_variants(&args.vllm_build_variants)?
    } else {
        VllmBuildVariants::default()
    };
    let vllm_model_dir = if is_vllm_inference {
        "/tmp/model".to_string()
    } else {
        args.vllm_model_dir.clone()
    };
    let vllm_port = args
        .vllm_port
        .unwrap_or(if is_vllm_inference { 8080 } else { 8090 });
    anyhow::ensure!(vllm_port > 0, "--vllm-port must be greater than zero");
    let (oss_bucket, model_key_file) = if matches!(
        plan,
        InitPlan::GpuDirect(InitTarget::VllmInference)
            | InitPlan::GpuStaged(InitTarget::VllmInference, InitStage::Deploy)
    ) {
        (
            args.oss_bucket
                .clone()
                .context("vllm-inference requires --oss-bucket")?,
            args.model_key_file
                .clone()
                .context("vllm-inference requires --model-key-file")?,
        )
    } else {
        (String::new(), PathBuf::new())
    };

    Ok(InitProject {
        plan,
        service_id,
        dir,
        spec_path,
        region,
        zone_id,
        instance_type,
        disk_gb,
        replicas: args.replicas,
        reference_values,
        cosign_key: args.cosign_key.clone(),
        slsa_generator: args.slsa_generator.clone(),
        dashscope_key,
        dashscope_base_url: args.dashscope_base_url.clone(),
        dashscope_anthropic_base_url: args.dashscope_anthropic_base_url.clone(),
        model,
        gateway_token,
        disable_pep: args.disable_pep,
        enable_dingtalk,
        dingtalk_client_id,
        dingtalk_client_secret,
        vllm_model_id: args.vllm_model_id.clone(),
        vllm_model_dir,
        vllm_served_model_name: args.vllm_served_model_name.clone(),
        vllm_port,
        vllm_version: args.vllm_version.clone(),
        vllm_build_variants,
        oss_bucket,
        oss_endpoint: args.oss_endpoint.clone(),
        model_key_file,
    })
}

fn guard_output_dir(dir: &Path, force: bool) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    if !force {
        bail!(
            "init output directory already exists: {}. Pass --force to replace it.",
            dir.display()
        );
    }
    let metadata =
        fs::symlink_metadata(dir).with_context(|| format!("failed to stat '{}'", dir.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "refusing to replace symlink output directory '{}'",
            dir.display()
        );
    }
    if metadata.is_dir() {
        fs::remove_dir_all(dir).with_context(|| format!("failed to remove '{}'", dir.display()))?;
    } else {
        fs::remove_file(dir).with_context(|| format!("failed to remove '{}'", dir.display()))?;
    }
    Ok(())
}

fn resolve_dashscope_key(args: &InitArgs, plan: InitPlan) -> Result<Option<String>> {
    let InitPlan::SharedCpuDeploy(target) = plan else {
        return Ok(None);
    };
    if target == InitTarget::Qodercli {
        return Ok(None);
    }
    let key = args
        .dashscope_api_key
        .clone()
        .or_else(|| std::env::var("BAILIAN_API_KEY").ok())
        .or_else(|| std::env::var("DASHSCOPE_API_KEY").ok());
    if key.as_deref().is_some_and(|value| !value.trim().is_empty()) {
        return Ok(key);
    }
    if args.non_interactive {
        bail!(
            "init {} --non-interactive requires --dashscope-api-key, DASHSCOPE_API_KEY, or BAILIAN_API_KEY",
            target.command_name()
        );
    }
    Ok(Some(prompt_secret("DashScope/Bailian API key")?))
}

fn resolve_dingtalk_credentials(args: &InitArgs) -> Result<(bool, Option<String>, Option<String>)> {
    if !args.enable_dingtalk {
        return Ok((false, None, None));
    }
    let client_id = resolve_dingtalk_value(
        args.dingtalk_client_id.clone(),
        args.non_interactive,
        "DingTalk bot client ID",
        "--dingtalk-client-id",
    )?;
    let client_secret = resolve_dingtalk_value(
        args.dingtalk_client_secret.clone(),
        args.non_interactive,
        "DingTalk bot client secret",
        "--dingtalk-client-secret",
    )?;
    Ok((true, Some(client_id), Some(client_secret)))
}

fn resolve_dingtalk_value(
    value: Option<String>,
    non_interactive: bool,
    prompt: &str,
    flag: &str,
) -> Result<String> {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        return Ok(value);
    }
    if non_interactive {
        bail!(
            "init --enable-dingtalk --non-interactive requires {flag} or its environment variable"
        );
    }
    if prompt.ends_with("secret") {
        prompt_secret(prompt)
    } else {
        prompt_value(prompt)
    }
}

fn parse_vllm_build_variants(value: &str) -> Result<VllmBuildVariants> {
    let mut variants = VllmBuildVariants {
        release: false,
        debug: false,
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("--vllm-build-variants cannot be empty");
    }
    for token in trimmed.split(',').map(str::trim) {
        match token {
            "release" => variants.release = true,
            "debug" => variants.debug = true,
            "" => bail!("--vllm-build-variants contains an empty entry"),
            other => bail!(
                "--vllm-build-variants only supports release, debug, or release,debug; got {other}"
            ),
        }
    }
    if !variants.release && !variants.debug {
        bail!("--vllm-build-variants must enable at least one variant");
    }
    Ok(variants)
}

fn resolve_or_generate_cosign_key(
    cli: &Cli,
    args: &InitArgs,
    project: &InitProject,
) -> Result<PathBuf> {
    if let Some(path) = args.cosign_key.as_ref() {
        if !path.exists() {
            bail!("cosign key does not exist: {}", path.display());
        }
        return path
            .canonicalize()
            .context("failed to resolve cosign key path");
    }

    fs::create_dir_all(&project.dir)
        .with_context(|| format!("failed to create '{}'", project.dir.display()))?;
    let prefix = project.dir.join("cosign");
    let key = project.dir.join("cosign.key");
    let pub_key = project.dir.join("cosign.pub");
    if !key.exists() || !pub_key.exists() {
        println!(
            "[ca] generating local cosign key pair for Rekor reference values at {}",
            prefix.display()
        );
        run_host_tool(
            cli,
            "cosign",
            vec![
                OsString::from("generate-key-pair"),
                OsString::from("--output-key-prefix"),
                prefix.as_os_str().to_os_string(),
            ],
            vec![("COSIGN_PASSWORD".to_string(), String::new())],
            true,
        )?;
    }
    set_mode(&key, 0o600)?;
    if pub_key.exists() {
        set_mode(&pub_key, 0o644)?;
    }
    Ok(PathBuf::from("./cosign.key"))
}

fn prompt_secret(prompt: &str) -> Result<String> {
    print!("{prompt}: ");
    std::io::stdout().flush().ok();
    let _ = Command::new("sh")
        .arg("-c")
        .arg("stty -echo 2>/dev/null || true")
        .status();
    let value = read_stdin_line();
    let _ = Command::new("sh")
        .arg("-c")
        .arg("stty echo 2>/dev/null || true")
        .status();
    eprintln!();
    let value = value?;
    let value = value.trim().to_string();
    if value.is_empty() {
        bail!("{prompt} cannot be empty");
    }
    Ok(value)
}

fn prompt_value(prompt: &str) -> Result<String> {
    print!("{prompt}: ");
    std::io::stdout().flush().ok();
    let value = read_stdin_line()?.trim().to_string();
    if value.is_empty() {
        bail!("{prompt} cannot be empty");
    }
    Ok(value)
}

fn read_stdin_line() -> Result<String> {
    let mut value = String::new();
    std::io::stdin()
        .read_line(&mut value)
        .context("failed to read stdin")?;
    Ok(value)
}

fn write_vllm_inference_project(cli: &Cli, args: &InitArgs, project: &InitProject) -> Result<()> {
    let _ = cli;
    if args.stage != Some(InitStage::Deploy) {
        let repo = find_assets_root()?;
        // NVIDIA stack reused verbatim from openclaw-vllm
        for file in [
            "cai-nvidia-cc-stack-install.sh",
            "nvidia-persistenced.service",
        ] {
            fs::copy(
                repo.join("examples/openclaw-vllm").join(file),
                project.dir.join(file),
            )
            .with_context(|| format!("failed to copy vllm-inference support file '{file}'"))?;
        }
        set_mode(&project.dir.join("cai-nvidia-cc-stack-install.sh"), 0o755)?;

        write_install_script_with_exports(
            &repo.join("examples/vllm-inference/install-vllm-inference.sh"),
            &project.dir.join("install-vllm-inference.sh"),
            &[("OPENCLAW_VLLM_VERSION", &project.vllm_version)],
        )?;
    }
    if args.stage != Some(InitStage::Build) {
        // cai-model.env resource (non-secret: public-read ciphertext bucket)
        let env = format!(
            "MODEL_OSS_BUCKET={}\nMODEL_OSS_ENDPOINT={}\nMODEL_MOUNT_POINT={}\nVLLM_PORT={}\nVLLM_SERVED_MODEL_NAME={}\n",
            project.oss_bucket,
            project.oss_endpoint,
            project.vllm_model_dir,
            project.vllm_port,
            project.vllm_served_model_name,
        );
        write_secret_file(&project.dir.join("cai-model.env"), &env)?;

        // model-decryption-key resource (secret: copied from operator-provided key file)
        fs::copy(
            &project.model_key_file,
            project.dir.join("model-decryption-key"),
        )
        .with_context(|| "failed to copy model-decryption-key")?;
        set_mode(&project.dir.join("model-decryption-key"), 0o600)?;
    }
    let spec = vllm_inference_yaml(project)?;
    write_file(&project.spec_path, &spec, 0o644)?;
    Ok(())
}

fn write_openclaw_vllm_project(_cli: &Cli, args: &InitArgs, project: &InitProject) -> Result<()> {
    if args.stage != Some(InitStage::Deploy) {
        let repo = find_assets_root()?;
        copy_dir(
            &repo.join("examples/openclaw/files"),
            &project.dir.join("files"),
        )?;
        for file in [
            "cai-nvidia-cc-stack-install.sh",
            "nvidia-persistenced.service",
        ] {
            fs::copy(
                repo.join("examples/openclaw-vllm").join(file),
                project.dir.join(file),
            )
            .with_context(|| format!("failed to copy OpenClaw vLLM support file '{file}'"))?;
        }
        set_mode(&project.dir.join("cai-nvidia-cc-stack-install.sh"), 0o755)?;
        let vllm_port = project.vllm_port.to_string();
        write_install_script_with_exports(
            &repo.join("examples/openclaw-vllm/install-openclaw-vllm.sh"),
            &project.dir.join("install-openclaw-vllm.sh"),
            &[
                ("OPENCLAW_VERSION", &args.openclaw_version),
                ("OPENCLAW_NODE_VERSION", &args.node_version),
                ("NPM_REGISTRY", &args.npm_registry),
                ("OPENCLAW_VLLM_MODEL_ID", &project.vllm_model_id),
                ("OPENCLAW_VLLM_MODEL_DIR", &project.vllm_model_dir),
                (
                    "OPENCLAW_VLLM_SERVED_MODEL_NAME",
                    &project.vllm_served_model_name,
                ),
                ("OPENCLAW_VLLM_PORT", &vllm_port),
                ("OPENCLAW_VLLM_VERSION", &project.vllm_version),
            ],
        )?;
    }
    if args.stage != Some(InitStage::Build) {
        let config = openclaw_json(project, true, None, "lan", 18789)?;
        write_secret_file(
            &project.dir.join("openclaw-vllm.json"),
            &serde_json::to_string_pretty(&config)?,
        )?;
    }
    let pep_bin = staged::pep_binary(args)?;
    let spec = openclaw_vllm_yaml(project, &pep_bin)?;
    write_file(&project.spec_path, &spec, 0o644)?;
    Ok(())
}

fn write_install_script_with_exports(
    src: &Path,
    dst: &Path,
    exports: &[(&str, &str)],
) -> Result<()> {
    let text =
        fs::read_to_string(src).with_context(|| format!("failed to read '{}'", src.display()))?;
    let mut lines: Vec<String> = text.lines().map(|line| format!("{line}\n")).collect();
    let mut missing = Vec::new();
    for (key, value) in exports {
        let prefix = format!("export {key}=");
        let mut replaced = false;
        for line in &mut lines {
            if line.starts_with(&prefix) {
                *line = format!("export {key}={}\n", shell_single_quote(value));
                replaced = true;
            }
        }
        if !replaced {
            missing.push(format!("export {key}={}\n", shell_single_quote(value)));
        }
    }

    let mut rendered = lines.concat();
    if !missing.is_empty() {
        let marker = "set -euo pipefail\n";
        let index = rendered
            .find(marker)
            .with_context(|| format!("{}: marker '{marker}' not found", src.display()))?
            + marker.len();
        rendered.insert_str(index, &missing.concat());
    }
    write_file(dst, &rendered, 0o755)
}

fn openclaw_json(
    project: &InitProject,
    local_vllm: bool,
    workspace: Option<&str>,
    bind: &str,
    port: u16,
) -> Result<serde_json::Value> {
    let token = project
        .gateway_token
        .as_deref()
        .context("OpenClaw gateway token was not resolved")?;
    let (provider_name, provider, primary_model) = if local_vllm {
        let served = project.vllm_served_model_name.as_str();
        let base_url = format!("http://127.0.0.1:{}/v1", project.vllm_port);
        (
            "local-vllm",
            serde_json::json!({
                "baseUrl": base_url,
                "apiKey": "local-unused",
                "api": "openai-completions",
                "models": [{
                    "id": served,
                    "name": served,
                    "reasoning": true,
                    "input": ["text", "image"],
                    "contextWindow": 262144,
                    "maxTokens": 16384
                }]
            }),
            format!("local-vllm/{served}"),
        )
    } else {
        let key = project
            .dashscope_key
            .as_deref()
            .context("OpenClaw requires DashScope API key")?;
        let model = project.model.trim_start_matches("bailian/");
        let mut models = vec![serde_json::json!({
            "id": model,
            "name": model,
            "reasoning": false,
            "input": ["text"],
            "contextWindow": 262144,
            "maxTokens": 65536
        })];
        if model != "qwen3-coder-plus" {
            models.push(serde_json::json!({
                "id": "qwen3-coder-plus",
                "name": "qwen3-coder-plus",
                "reasoning": false,
                "input": ["text"],
                "contextWindow": 131072,
                "maxTokens": 32768
            }));
        }
        (
            "bailian",
            serde_json::json!({
                "baseUrl": project.dashscope_base_url,
                "apiKey": key,
                "api": "openai-completions",
                "models": models
            }),
            format!("bailian/{model}"),
        )
    };

    let pep_workdir = workspace.unwrap_or("/workspace");
    let mut allow = vec!["cai-a2a"];
    let mut entries = serde_json::Map::new();
    if !project.disable_pep {
        allow.insert(0, "cai-pep");
        entries.insert(
            "cai-pep".to_string(),
            serde_json::json!({
                "enabled": true,
                "config": {
                    "socketPath": "/run/cai/pep.sock",
                    "pepRequired": true,
                    "defaultWorkdir": pep_workdir
                }
            }),
        );
    }
    entries.insert(
        "cai-a2a".to_string(),
        serde_json::json!({"enabled": true, "config": {"peers": {}}}),
    );
    let mut channels = serde_json::Map::new();
    if project.enable_dingtalk {
        let client_id = project
            .dingtalk_client_id
            .as_deref()
            .context("DingTalk client ID was not resolved")?;
        let client_secret = project
            .dingtalk_client_secret
            .as_deref()
            .context("DingTalk client secret was not resolved")?;
        allow.insert(0, "dingtalk");
        entries.insert(
            "dingtalk".to_string(),
            serde_json::json!({
                "enabled": true,
                "hooks": {"allowConversationAccess": true}
            }),
        );
        channels.insert(
            "dingtalk".to_string(),
            serde_json::json!({
                "enabled": true,
                "clientId": client_id,
                "clientSecret": client_secret,
                "dmPolicy": "open",
                "allowFrom": ["*"],
                "groupPolicy": "open",
                "debug": false,
                "messageType": "markdown"
            }),
        );
    }

    let mut agent_defaults = serde_json::json!({
        "model": {"primary": primary_model}
    });
    if let Some(ws) = workspace {
        agent_defaults["workspace"] = serde_json::json!(ws);
    }

    Ok(serde_json::json!({
        "models": {
            "mode": "merge",
            "providers": {
                provider_name: provider
            }
        },
        "agents": {"defaults": agent_defaults},
        "tools": {"profile": "full"},
        "plugins": {
            "enabled": true,
            "allow": allow,
            "entries": entries
        },
        "channels": channels,
        "gateway": {
            "mode": "local",
            "bind": bind,
            "port": port,
            "auth": {"mode": "token", "token": token},
            "http": {"endpoints": {"responses": {"enabled": true}}},
            "controlUi": {
                "enabled": true,
                "basePath": "/openclaw",
                "dangerouslyAllowHostHeaderOriginFallback": true,
                "dangerouslyDisableDeviceAuth": true
            }
        }
    }))
}

fn openclaw_vllm_yaml(project: &InitProject, pep_bin: &Path) -> Result<String> {
    let files = openclaw_vllm_files_yaml(pep_bin)?;
    let variants = openclaw_vllm_variants_yaml(project.vllm_build_variants);
    let deploy_variant = project.vllm_build_variants.deploy_variant();
    Ok(format!(
        r#"schema: confidential-agent/v1

service:
  id: {service_id}
  ports: [18789]
  connect: [18789]
  app_service: cai-openclaw-gateway.service

build:
  image_name: openclaw-vllm-agent
  kernel_cmdline_append: swiotlb=4194304,any rd.driver.blacklist=nouveau modprobe.blacklist=nouveau nouveau.modeset=0
  resize: 160G
  cleanup:
    remove_static_libs: false
  repos:
    - name: epel
      baseurl: https://mirrors.aliyun.com/epel/8/Everything/$basearch/
      gpgkey: https://mirrors.aliyun.com/epel/RPM-GPG-KEY-EPEL-8
  packages: [binutils, ca-certificates, curl, elfutils-libelf-devel, findutils, gcc, gcc-c++, git, glibc-devel, jq, kernel, kernel-devel, kernel-headers, kmod, make, nodejs, npm, openssl3, pciutils, pkgconf-pkg-config, podman, python3.11, python3.11-devel, python3.11-pip, rpm, tar, wget, xz, zlib-devel]
  files:
{files}  scripts: [./install-openclaw-vllm.sh]
  variants:
{variants}

deploy:
  provider: aliyun
  image_variant: {deploy_variant}
  instance_type: {instance_type}
  region: {region}
  zone_id: {zone_id}
  disk_gb: {disk_gb}

attestation:
  tee: tdx
  mode: challenge
  reference_values: {reference_values}
{rekor}
a2a:
  id: {service_id}
  name: {service_id}
  version: "1.0.0"
  description: "OpenClaw vLLM confidential agent"
  skills:
    - id: chat
      name: Chat
      description: "OpenClaw gateway chat"

resources:
  openclaw_config:
    source: ./openclaw-vllm.json
    target: /home/openclaw/.openclaw/openclaw.json
    owner: openclaw
    group: openclaw
    mode: "0600"
    required: true
"#,
        service_id = &project.service_id,
        files = files,
        variants = variants,
        deploy_variant = deploy_variant,
        instance_type = yaml_quote(&project.instance_type)?,
        region = yaml_quote(&project.region)?,
        zone_id = yaml_quote(&project.zone_id)?,
        disk_gb = project.disk_gb,
        reference_values = yaml_quote(reference_values_name(project.reference_values))?,
        rekor = rekor_block(project)?,
    ))
}

fn openclaw_vllm_variants_yaml(variants: VllmBuildVariants) -> String {
    let mut out = format!("    release:\n      enabled: {}\n", variants.release);
    if variants.debug {
        out.push_str("    debug:\n      enabled: true\n");
    }
    out
}

fn vllm_inference_yaml(project: &InitProject) -> Result<String> {
    let connect_line = format!("[{{port: {}, encryption: ohttp}}]", project.vllm_port);
    let multinode = if project.replicas > 1 {
        format!(
            "  replicas: {}\n  slb:\n    enabled: true\n    slb_type: alb",
            project.replicas
        )
    } else {
        "  # Multi-node: pass --replicas N (N>1) to deploy N replicas behind\n  # an ALB."
            .to_string()
    };
    Ok(format!(
        r#"schema: confidential-agent/v1

service:
  id: {service_id}
  ports: [{vllm_port}]
  connect: {connect_line}
  app_service: cai-vllm.service

build:
  image_name: vllm-inference-agent
  kernel_cmdline_append: swiotlb=4194304,any rd.driver.blacklist=nouveau modprobe.blacklist=nouveau nouveau.modeset=0
  resize: 80G
  cleanup:
    remove_static_libs: false
  repos:
    - name: epel
      baseurl: https://mirrors.aliyun.com/epel/8/Everything/$basearch/
      gpgkey: https://mirrors.aliyun.com/epel/RPM-GPG-KEY-EPEL-8
  packages: [anolis-epao-release, binutils, ca-certificates, curl, dracut, elfutils-libelf-devel, fuse, fuse-libs, fuse3, fuse3-libs, gocryptfs, gcc, gcc-c++, glibc-devel, git, jq, kernel, kernel-devel, kernel-headers, kmod, mailcap, make, openssl3, pciutils, pkgconf-pkg-config, python3.11, python3.11-devel, python3.11-pip, rpm, s3fs-fuse, tar, wget, xz, zlib-devel]
  files:
    - source: ./cai-nvidia-cc-stack-install.sh
      target: /usr/local/sbin/cai-nvidia-cc-stack-install.sh
      executable: true
    - source: ./nvidia-persistenced.service
      target: /usr/local/share/cai/nvidia-persistenced.service
  scripts: [./install-vllm-inference.sh]
  variants:
{variants}
deploy:
  provider: aliyun
  image_variant: {deploy_variant}
  instance_type: {instance_type}
  region: {region}
  zone_id: {zone_id}
  disk_gb: {disk_gb}
{multinode}
attestation:
  tee: tdx
  mode: challenge
  reference_values: {reference_values}
{rekor}
resources:
  model_decryption_key:
    source: ./model-decryption-key
    target: /run/cai/secrets/model-decryption-key
    mode: "0600"
    required: true
  cai_model_env:
    source: ./cai-model.env
    target: /etc/cai/model.env
    mode: "0600"
    required: true
"#,
        service_id = &project.service_id,
        instance_type = yaml_quote(&project.instance_type)?,
        region = yaml_quote(&project.region)?,
        zone_id = yaml_quote(&project.zone_id)?,
        disk_gb = project.disk_gb,
        connect_line = connect_line,
        vllm_port = project.vllm_port,
        multinode = multinode,
        variants = openclaw_vllm_variants_yaml(project.vllm_build_variants),
        deploy_variant = project.vllm_build_variants.deploy_variant(),
        reference_values = yaml_quote(reference_values_name(project.reference_values))?,
        rekor = rekor_block(project)?,
    ))
}

fn openclaw_vllm_files_yaml(pep_bin: &Path) -> Result<String> {
    let mut out = String::new();
    out.push_str("    - source: ./nvidia-persistenced.service\n      target: /usr/local/share/cai/nvidia-persistenced.service\n");
    out.push_str("    - source: ./cai-nvidia-cc-stack-install.sh\n      target: /usr/local/sbin/cai-nvidia-cc-stack-install.sh\n      executable: true\n");
    out.push_str(&format!(
        "    - source: {}\n      target: /usr/local/bin/cai-pep\n      executable: true\n",
        yaml_quote(&pep_bin.to_string_lossy())?
    ));
    out.push_str("    - source: ./files/tdx-remote-attestation.SKILL.md\n      target: /home/openclaw/.openclaw/skills/tdx-remote-attestation/SKILL.md\n");
    out.push_str("    - source: ./files/install-cai-pep.sh\n      target: /usr/local/libexec/confidential-agent/openclaw/install-cai-pep.sh\n      executable: true\n");
    out.push_str("    - source: ./files/install-openclaw-runtime.sh\n      target: /usr/local/libexec/confidential-agent/openclaw/install-openclaw-runtime.sh\n      executable: true\n");
    out.push_str("    - source: ./files/cai-pep-default-policy.json\n      target: /usr/local/share/confidential-agent/openclaw/cai-pep-default-policy.json\n");
    out.push_str("    - source: ./files/cai-pep-plugin\n      target: /usr/local/share/confidential-agent/openclaw/cai-pep-plugin\n");
    out.push_str("    - source: ./files/cai-a2a-plugin\n      target: /usr/local/share/confidential-agent/openclaw/cai-a2a-plugin\n");
    out.push_str("    - source: ./files/patch-openclaw-cai-pep.js\n      target: /usr/local/share/confidential-agent/openclaw/patch-openclaw-cai-pep.js\n      executable: true\n");
    Ok(out)
}

fn rekor_block(project: &InitProject) -> Result<String> {
    if project.reference_values != InitReferenceValues::Rekor
        || matches!(project.plan, InitPlan::GpuStaged(_, InitStage::Deploy))
    {
        return Ok(String::new());
    }
    let cosign = project
        .cosign_key
        .as_ref()
        .context("Rekor reference values require cosign key")?;
    Ok(format!(
        "  rekor:\n    cosign_key: {}\n    slsa_generator: {}\n    required: true\n",
        yaml_quote(&cosign.to_string_lossy())?,
        yaml_quote(&project.slsa_generator.to_string_lossy())?,
    ))
}

fn shell_path(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

fn write_project_next_steps(cli: &Cli, project: &InitProject, body: &str) -> Result<()> {
    let state = absolute_path_for_state(&cli.state_dir);
    let directory = project.dir.canonicalize()?;
    let command = format!("confidential-agent --state-dir {}", shell_path(&state));
    let body = body.replace("confidential-agent ", &format!("{command} "));
    let preamble = format!(
        "Project directory: `{}`\nCA state directory: `{}`\n\nStart here:\n\n```bash\ncd {}\n```\n\nSpec resource paths are relative to the spec file. Command-line key/output paths are relative to this directory.\nBuild/publication records, keys and managed Shelter/Terraform work remain under the CA state directory; keep it across upgrades.\nUse the same --state-dir in later commands even after changing directories.\nThe source one-click/ scripts and their generated work directory are separate entry points; no one-click directory is needed here.\n\n",
        directory.display(),
        state.display(),
        shell_path(&directory)
    );
    write_file(
        &project.dir.join("NEXT_STEPS.md"),
        &(preamble + &body),
        0o644,
    )
}

fn write_next_steps(cli: &Cli, project: &InitProject) -> Result<()> {
    let connect = format!(
        "\n5. Start connect when the service is active:\n\n```bash\nconfidential-agent connect start --service {} --ready-json ./{}-connect-ready.json --wait-ready 180\n```\n",
        project.service_id, project.service_id
    );
    let content = format!(
        r#"# Confidential Agent Init: {service}

Generated at: {generated}

1. Add an operator peering:

```bash
confidential-agent peering add --role operator --cidr <operator-cidr> --label ops
```

2. Validate the AppSpec:

```bash
confidential-agent spec validate --spec {spec}
```

3. Build and deploy:

```bash
confidential-agent build --spec {spec}
confidential-agent deploy --spec {spec}
```

4. Check live status:

```bash
confidential-agent status --live
```
{connect}
Cleanup:

```bash
confidential-agent destroy {service}
```
"#,
        service = project.service_id,
        generated = current_utc_timestamp(),
        spec = shell_path(Path::new(
            project
                .spec_path
                .file_name()
                .context("spec has no filename")?
        )),
        connect = connect,
    );
    write_project_next_steps(cli, project, &content)
}

fn find_assets_root() -> Result<PathBuf> {
    let current = std::env::current_dir().context("failed to resolve current directory")?;
    for base in current.ancestors() {
        if base.join("Cargo.toml").is_file() && base.join("examples").is_dir() {
            return Ok(base.to_path_buf());
        }
    }
    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    for base in exe.ancestors() {
        if base.join("Cargo.toml").is_file() && base.join("examples").is_dir() {
            return Ok(base.to_path_buf());
        }
    }
    let packaged = PathBuf::from("/usr/share/cagent");
    if packaged.join("examples").is_dir() {
        return Ok(packaged);
    }
    bail!("could not locate application templates; install the cagent RPM or run init from a source checkout")
}

fn find_cai_pep_binary_for_init() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CA_PEP_BIN").map(PathBuf::from) {
        return require_executable(path, "CA_PEP_BIN");
    }
    let exe = std::env::current_exe().context("failed to resolve current executable")?;
    let mut candidates = Vec::new();
    if let Some(dir) = exe.parent() {
        candidates.push(dir.join("cai-pep"));
        if dir.file_name().and_then(|name| name.to_str()) == Some("deps") {
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join("cai-pep"));
            }
        }
    }
    if let Ok(repo) = find_assets_root() {
        candidates.push(repo.join("target/debug/cai-pep"));
        candidates.push(repo.join("target/release/cai-pep"));
    }
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("cai-pep binary is required for this target; build it with `cargo build -p cai-pep` or set CA_PEP_BIN")
}

fn require_executable(path: PathBuf, name: &str) -> Result<PathBuf> {
    if path.is_file() {
        Ok(path)
    } else {
        bail!(
            "{name} does not point to an existing file: {}",
            path.display()
        )
    }
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    if !src.is_dir() {
        bail!("source directory does not exist: {}", src.display());
    }
    if dst.exists() {
        fs::remove_dir_all(dst).with_context(|| format!("failed to remove '{}'", dst.display()))?;
    }
    fs::create_dir_all(dst).with_context(|| format!("failed to create '{}'", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("failed to read '{}'", src.display()))? {
        let entry = entry?;
        let source = entry.path();
        let target = dst.join(entry.file_name());
        if source.is_dir() {
            copy_dir(&source, &target)?;
        } else {
            fs::copy(&source, &target).with_context(|| {
                format!(
                    "failed to copy '{}' to '{}'",
                    source.display(),
                    target.display()
                )
            })?;
            let mode = source
                .metadata()
                .map(|metadata| metadata.mode() & 0o777)
                .unwrap_or(0o644);
            set_mode(&target, mode)?;
        }
    }
    Ok(())
}

fn write_file(path: &Path, content: &str, mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create '{}'", parent.display()))?;
    }
    fs::write(path, content).with_context(|| format!("failed to write '{}'", path.display()))?;
    set_mode(path, mode)
}

fn write_secret_file(path: &Path, content: &str) -> Result<()> {
    write_file(path, content, 0o600)
}

fn random_hex(bytes_len: usize) -> String {
    let mut bytes = vec![0u8; bytes_len];
    OsRng.fill_bytes(&mut bytes);
    hex_encode(&bytes)
}

fn random_base64(bytes_len: usize) -> String {
    let mut bytes = vec![0u8; bytes_len];
    OsRng.fill_bytes(&mut bytes);
    BASE64_STANDARD.encode(bytes)
}

fn yaml_quote(value: &str) -> Result<String> {
    if value.contains('\n') || value.contains('\r') {
        bail!("YAML scalar values must not contain newlines");
    }
    Ok(format!("'{}'", value.replace('\'', "''")))
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn default_zone_id(region: &str) -> &'static str {
    match region {
        "cn-hongkong" => "cn-hongkong-d",
        "cn-beijing" => "cn-beijing-i",
        _ => "cn-beijing-i",
    }
}

fn default_instance_type(region: &str, plan: InitPlan) -> &'static str {
    if matches!(plan, InitPlan::GpuDirect(_) | InitPlan::GpuStaged(_, _)) {
        return "ecs.gn8v-tee.4xlarge";
    }
    match region {
        "cn-hongkong" => "ecs.g8i.xlarge",
        "cn-beijing" => "ecs.g9i.xlarge",
        _ => "ecs.g8i.xlarge",
    }
}

fn reference_values_name(value: InitReferenceValues) -> &'static str {
    match value {
        InitReferenceValues::Sample => "sample",
        InitReferenceValues::Rekor => "rekor",
    }
}

impl InitTarget {
    pub(super) fn is_gpu(self) -> bool {
        matches!(self, InitTarget::OpenclawVllm | InitTarget::VllmInference)
    }

    pub(super) fn service_id(self) -> &'static str {
        match self {
            InitTarget::Openclaw => "openclaw",
            InitTarget::OpenclawVllm => "openclaw-vllm",
            InitTarget::Hermes => "hermes",
            InitTarget::Codex => "codex",
            InitTarget::Claudecode => "claude-code",
            InitTarget::Qodercli => "qodercli",
            InitTarget::Opencode => "opencode",
            InitTarget::Pi => "pi",
            InitTarget::Dsh => "dsh",
            InitTarget::VllmInference => "vllm-inference",
        }
    }

    pub(super) fn spec_name(self) -> &'static str {
        match self {
            InitTarget::OpenclawVllm => "openclaw-vllm.yaml",
            InitTarget::VllmInference => "vllm-inference.yaml",
            _ => "instance.yaml",
        }
    }

    pub(super) fn command_name(self) -> &'static str {
        self.service_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::sync::Mutex;

    pub(super) static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn target_names_match_application_centric_layout() {
        assert_eq!(InitTarget::Openclaw.service_id(), "openclaw");
        assert_eq!(InitTarget::Hermes.service_id(), "hermes");
        assert_eq!(InitTarget::Claudecode.command_name(), "claude-code");
        assert_eq!(InitTarget::Opencode.service_id(), "opencode");
        assert_eq!(InitTarget::Dsh.service_id(), "dsh");
        assert_eq!(InitTarget::OpenclawVllm.spec_name(), "openclaw-vllm.yaml");
        assert_eq!(InitTarget::VllmInference.service_id(), "vllm-inference");
        assert_eq!(InitTarget::VllmInference.spec_name(), "vllm-inference.yaml");
        assert!(Cli::try_parse_from(["confidential-agent", "init", "allinone"]).is_err());
        assert!(Cli::try_parse_from(["confidential-agent", "init", "all-in-one"]).is_err());
        assert!(Cli::try_parse_from([
            "confidential-agent",
            "init",
            "openclaw",
            "--harness",
            "codex"
        ])
        .is_err());
    }

    #[test]
    fn yaml_quote_escapes_single_quotes() {
        assert_eq!(yaml_quote("a'b").unwrap(), "'a''b'");
        assert!(yaml_quote("a\nb").is_err());
    }

    #[test]
    fn parses_vllm_build_variants() {
        assert_eq!(
            parse_vllm_build_variants("release").unwrap(),
            VllmBuildVariants {
                release: true,
                debug: false,
            }
        );
        assert_eq!(
            parse_vllm_build_variants("debug").unwrap(),
            VllmBuildVariants {
                release: false,
                debug: true,
            }
        );
        assert_eq!(
            parse_vllm_build_variants("release,debug").unwrap(),
            VllmBuildVariants {
                release: true,
                debug: true,
            }
        );
        assert!(parse_vllm_build_variants("").is_err());
        assert!(parse_vllm_build_variants("release,fast").is_err());
    }

    #[test]
    fn openclaw_vllm_init_defaults_to_release_and_preserves_static_libs() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let pep = temp.path().join("cai-pep");
        fs::write(&pep, "#!/bin/sh\nexit 0\n").unwrap();
        set_mode(&pep, 0o755).unwrap();
        let previous = std::env::var_os("CA_PEP_BIN");
        std::env::set_var("CA_PEP_BIN", &pep);

        let cli = Cli::parse_from([
            "confidential-agent",
            "--state-dir",
            temp.path().join("state").to_str().unwrap(),
            "init",
            "openclaw-vllm",
            "--non-interactive",
            "--force",
            "--output-dir",
            temp.path().join("init").to_str().unwrap(),
            "--reference-values",
            "sample",
            "--gateway-token",
            "0123456789abcdef0123456789abcdef",
            "--vllm-served-model-name",
            "custom-model",
            "--vllm-port",
            "18090",
            "--vllm-version",
            "0.20.0",
        ]);
        let Commands::Init(args) = &cli.command else {
            panic!("expected init command");
        };
        cmd_init(&cli, args).unwrap();

        let dir = temp.path().join("init/openclaw-vllm");
        let spec = fs::read_to_string(dir.join("openclaw-vllm.yaml")).unwrap();
        let parsed: AgentSpec = AgentSpec::from_yaml(&spec, &dir).unwrap();
        assert!(parsed.build.packages.contains(&"gcc-c++".into()));
        assert!(spec.contains("cleanup:\n    remove_static_libs: false"));
        assert!(spec.contains("image_variant: release"));
        assert!(spec.contains("release:\n      enabled: true"));
        assert!(!spec.contains("debug:\n      enabled: true"));
        let install_script = fs::read_to_string(dir.join("install-openclaw-vllm.sh")).unwrap();
        assert!(install_script.contains("export OPENCLAW_VLLM_PORT='18090'"));
        assert!(install_script.contains("export OPENCLAW_VLLM_VERSION='0.20.0'"));
        let config: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("openclaw-vllm.json")).unwrap())
                .unwrap();
        assert_eq!(
            config["models"]["providers"]["local-vllm"]["baseUrl"],
            "http://127.0.0.1:18090/v1"
        );
        assert_eq!(
            config["agents"]["defaults"]["model"]["primary"],
            "local-vllm/custom-model"
        );

        if let Some(value) = previous {
            std::env::set_var("CA_PEP_BIN", value);
        } else {
            std::env::remove_var("CA_PEP_BIN");
        }
    }

    #[test]
    fn openclaw_vllm_init_can_generate_debug_variant() {
        let project = InitProject {
            plan: InitPlan::GpuDirect(InitTarget::OpenclawVllm),
            service_id: "openclaw-vllm".to_string(),
            dir: PathBuf::from("/tmp/openclaw-vllm"),
            spec_path: PathBuf::from("/tmp/openclaw-vllm/openclaw-vllm.yaml"),
            region: "cn-beijing".to_string(),
            zone_id: "cn-beijing-l".to_string(),
            instance_type: "ecs.gn8v-tee.4xlarge".to_string(),
            disk_gb: 512,
            replicas: 1,
            reference_values: InitReferenceValues::Sample,
            cosign_key: None,
            slsa_generator: PathBuf::from("/usr/libexec/shelter/slsa/slsa-generator"),
            dashscope_key: None,
            dashscope_base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string(),
            dashscope_anthropic_base_url: "https://dashscope.aliyuncs.com/apps/anthropic"
                .to_string(),
            model: confidential_agent_core::software::version("bailian_model"),
            gateway_token: Some("0123456789abcdef0123456789abcdef".to_string()),
            disable_pep: false,
            enable_dingtalk: false,
            dingtalk_client_id: None,
            dingtalk_client_secret: None,
            vllm_model_id: "Qwen/Qwen3.6-35B-A3B".to_string(),
            vllm_model_dir: "/opt/models/Qwen3.6-35B-A3B".to_string(),
            vllm_served_model_name: "Qwen3.6-35B-A3B".to_string(),
            vllm_port: 8090,
            vllm_version: "0.19.1".to_string(),
            vllm_build_variants: VllmBuildVariants {
                release: false,
                debug: true,
            },
            oss_bucket: String::new(),
            oss_endpoint: "https://oss-cn-beijing-internal.aliyuncs.com".to_string(),
            model_key_file: PathBuf::new(),
        };
        let yaml = openclaw_vllm_yaml(&project, Path::new("/usr/local/bin/cai-pep")).unwrap();
        assert!(yaml.contains("release:\n      enabled: false"));
        assert!(yaml.contains("debug:\n      enabled: true"));
        assert!(yaml.contains("image_variant: debug"));
    }

    fn vllm_inference_project(replicas: u32) -> InitProject {
        InitProject {
            plan: InitPlan::GpuDirect(InitTarget::VllmInference),
            service_id: "vllm-inference".to_string(),
            dir: PathBuf::from("/tmp/vllm-inference"),
            spec_path: PathBuf::from("/tmp/vllm-inference/vllm-inference.yaml"),
            region: "cn-beijing".to_string(),
            zone_id: "cn-beijing-l".to_string(),
            instance_type: "ecs.gn8v-tee.4xlarge".to_string(),
            disk_gb: 512,
            replicas,
            reference_values: InitReferenceValues::Sample,
            cosign_key: None,
            slsa_generator: PathBuf::from("/usr/libexec/shelter/slsa/slsa-generator"),
            dashscope_key: None,
            dashscope_base_url: String::new(),
            dashscope_anthropic_base_url: String::new(),
            model: String::new(),
            gateway_token: None,
            disable_pep: false,
            enable_dingtalk: false,
            dingtalk_client_id: None,
            dingtalk_client_secret: None,
            vllm_model_id: String::new(),
            vllm_model_dir: String::new(),
            vllm_served_model_name: "Qwen3-32B".to_string(),
            vllm_port: 8080,
            vllm_version: "0.19.1".to_string(),
            vllm_build_variants: VllmBuildVariants::default(),
            oss_bucket: String::new(),
            oss_endpoint: "https://oss-cn-beijing-internal.aliyuncs.com".to_string(),
            model_key_file: PathBuf::new(),
        }
    }

    #[test]
    fn vllm_inference_yaml_multinode_when_replicas_gt_1() {
        let yaml = vllm_inference_yaml(&vllm_inference_project(2)).unwrap();
        assert!(yaml.contains("replicas: 2"));
        assert!(yaml.contains("slb:\n    enabled: true\n    slb_type: alb"));
        assert!(yaml.contains("connect: [{port: 8080, encryption: ohttp}]"));
    }

    #[test]
    fn vllm_inference_yaml_single_node_when_replicas_is_1() {
        let yaml = vllm_inference_yaml(&vllm_inference_project(1)).unwrap();
        assert!(yaml.contains("connect: [{port: 8080, encryption: ohttp}]"));
        assert!(!yaml.contains("\n  replicas:"));
        assert!(!yaml.contains("\n  slb:"));
    }

    #[test]
    fn openclaw_json_supports_dingtalk_and_per_instance_disable_pep() {
        let mut project = InitProject {
            plan: InitPlan::SharedCpuDeploy(InitTarget::Openclaw),
            service_id: "openclaw".to_string(),
            dir: PathBuf::from("/tmp/openclaw"),
            spec_path: PathBuf::from("/tmp/openclaw/instance.yaml"),
            region: "cn-beijing".to_string(),
            zone_id: "cn-beijing-i".to_string(),
            instance_type: "ecs.g9i.xlarge".to_string(),
            disk_gb: 120,
            replicas: 1,
            reference_values: InitReferenceValues::Rekor,
            cosign_key: None,
            slsa_generator: PathBuf::from("/usr/libexec/shelter/slsa/slsa-generator"),
            dashscope_key: Some("sk-test".to_string()),
            dashscope_base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string(),
            dashscope_anthropic_base_url: "https://dashscope.aliyuncs.com/apps/anthropic"
                .to_string(),
            model: confidential_agent_core::software::version("bailian_model"),
            gateway_token: Some("0123456789abcdef0123456789abcdef".to_string()),
            disable_pep: false,
            enable_dingtalk: true,
            dingtalk_client_id: Some("ding-id".to_string()),
            dingtalk_client_secret: Some("ding-secret".to_string()),
            vllm_model_id: String::new(),
            vllm_model_dir: String::new(),
            vllm_served_model_name: String::new(),
            vllm_port: 8090,
            vllm_version: "0.28.0".to_string(),
            vllm_build_variants: VllmBuildVariants::default(),
            oss_bucket: String::new(),
            oss_endpoint: "https://oss-cn-beijing-internal.aliyuncs.com".to_string(),
            model_key_file: PathBuf::new(),
        };
        let config = openclaw_json(
            &project,
            false,
            Some("/opt/data/allinone/workspace"),
            "lan",
            18789,
        )
        .unwrap();
        assert_eq!(config["plugins"]["entries"]["dingtalk"]["enabled"], true);
        assert_eq!(config["plugins"]["entries"]["cai-pep"]["enabled"], true);
        assert_eq!(config["channels"]["dingtalk"]["clientId"], "ding-id");
        assert_eq!(
            config["channels"]["dingtalk"]["clientSecret"],
            "ding-secret"
        );
        assert_eq!(config["gateway"]["controlUi"]["basePath"], "/openclaw");
        assert_eq!(config["gateway"]["bind"], "lan");

        project.disable_pep = true;
        let no_pep = openclaw_json(
            &project,
            false,
            Some("/opt/data/allinone/workspace"),
            "lan",
            18789,
        )
        .unwrap();
        assert!(no_pep["plugins"]["entries"].get("cai-pep").is_none());
        assert!(!no_pep["plugins"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "cai-pep"));
        assert_eq!(
            no_pep["gateway"]["auth"]["token"],
            "0123456789abcdef0123456789abcdef"
        );
    }

    #[test]
    fn vllm_inference_init_succeeds_without_dashscope_key() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let pep = temp.path().join("cai-pep");
        fs::write(&pep, "#!/bin/sh\nexit 0\n").unwrap();
        set_mode(&pep, 0o755).unwrap();
        let previous_pep = std::env::var_os("CA_PEP_BIN");
        std::env::set_var("CA_PEP_BIN", &pep);
        let previous_dashscope = std::env::var_os("DASHSCOPE_API_KEY");
        let previous_bailian = std::env::var_os("BAILIAN_API_KEY");
        std::env::remove_var("DASHSCOPE_API_KEY");
        std::env::remove_var("BAILIAN_API_KEY");

        let key_file = temp.path().join("model.key");
        fs::write(&key_file, "test-passphrase\n").unwrap();

        let cli = Cli::parse_from([
            "confidential-agent",
            "--state-dir",
            temp.path().join("state").to_str().unwrap(),
            "init",
            "vllm-inference",
            "--non-interactive",
            "--force",
            "--output-dir",
            temp.path().join("init").to_str().unwrap(),
            "--reference-values",
            "sample",
            "--model-key-file",
            key_file.to_str().unwrap(),
            "--oss-bucket",
            "test-bucket:/qwen",
            "--gateway-token",
            "0123456789abcdef0123456789abcdef",
        ]);
        let Commands::Init(args) = &cli.command else {
            panic!("expected init command");
        };
        cmd_init(&cli, args).unwrap();

        let spec = temp
            .path()
            .join("init")
            .join("vllm-inference")
            .join("vllm-inference.yaml");
        let spec = AgentSpec::from_path(&spec).unwrap();
        assert_eq!(spec.service.id, "vllm-inference");
        assert_eq!(spec.attestation.mode, AttestationMode::Challenge);
        assert_eq!(spec.service.ports, [8080]);
        let model_env = std::fs::read_to_string(
            temp.path()
                .join("init")
                .join("vllm-inference")
                .join("cai-model.env"),
        )
        .unwrap();
        assert!(model_env.contains("VLLM_PORT=8080"));

        if let Some(value) = previous_pep {
            std::env::set_var("CA_PEP_BIN", value);
        } else {
            std::env::remove_var("CA_PEP_BIN");
        }
        if let Some(value) = previous_dashscope {
            std::env::set_var("DASHSCOPE_API_KEY", value);
        }
        if let Some(value) = previous_bailian {
            std::env::set_var("BAILIAN_API_KEY", value);
        }
    }
}
