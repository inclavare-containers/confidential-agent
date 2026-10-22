use super::*;

pub(super) fn link_committed_config(alias: &Path, target: &Path) -> Result<()> {
    if alias == target {
        return Ok(());
    }
    let temporary = alias.with_extension("config-link.tmp");
    if temporary.symlink_metadata().is_ok() {
        fs::remove_file(&temporary)?;
    }
    std::os::unix::fs::symlink(target, &temporary)?;
    fs::rename(&temporary, alias)
        .with_context(|| format!("failed to publish configuration link {}", alias.display()))
}

pub(super) fn deployment_snapshot(path: &Path) -> Result<DeploymentSnapshot> {
    deployment_snapshot_with_image_options(path, false, None)
}

pub(super) fn deployment_snapshot_with_image_options(
    path: &Path,
    skip_signature: bool,
    image_id: Option<&str>,
) -> Result<DeploymentSnapshot> {
    let path = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve spec {}", path.display()))?;
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read spec '{}'", path.display()))?;
    let effective_content = if skip_signature || image_id.is_some() {
        let mut value: serde_yaml::Value = serde_yaml::from_str(&content)?;
        let image = value
            .get_mut("deploy")
            .and_then(|v| v.get_mut("image"))
            .and_then(|v| v.as_mapping_mut())
            .context("image options require deploy.image")?;
        if skip_signature {
            image.insert("skip_signature_verification".into(), true.into());
        }
        if let Some(id) = image_id {
            image.insert("image_id".into(), id.into());
        }
        serde_yaml::to_string(&value)?
    } else {
        content.clone()
    };
    Ok(DeploymentSnapshot {
        source: LocalSpecState {
            path: path.clone(),
            sha256: hex_encode(&Sha256::digest(content.as_bytes())),
        },
        spec: AgentSpec::from_yaml(
            &effective_content,
            path.parent().context("spec has no parent")?,
        )?,
        user_data: None,
        inputs: None,
    })
}

fn deployment_input_paths(
    paths: &ContextPaths,
    spec: &AgentSpec,
) -> Result<BTreeMap<String, PathBuf>> {
    ensure_private_context_dirs(paths)?;
    let mut inputs: BTreeMap<_, _> = instance_ssh::runtime_spec(paths, spec)?
        .resources
        .into_iter()
        .map(|(id, resource)| (format!("resource:{id}"), resource.source))
        .collect();
    inputs.insert(
        "secret:disk_passphrase".into(),
        match &spec.secrets.disk_passphrase {
            Some(path) => path.clone(),
            None => ensure_disk_passphrase(paths)?,
        },
    );
    let gateway = ensure_gateway_identity(paths)?;
    inputs.insert("identity:gateway".into(), gateway.private_key_path);
    if spec.service.ssh {
        inputs.insert(
            "identity:ssh_client".into(),
            paths.secrets_dir.join("ssh_client"),
        );
    }
    inputs
        .into_iter()
        .map(|(id, path)| {
            fs::canonicalize(&path)
                .with_context(|| {
                    format!(
                        "failed to resolve deployment input {id}: {}",
                        path.display()
                    )
                })
                .map(|path| (id, path))
        })
        .collect()
}

pub(super) fn freeze_deployment_inputs(
    snapshot: &mut DeploymentSnapshot,
    paths: &ContextPaths,
    run_id: &str,
) -> Result<()> {
    let directory = paths.service_dir.join("deployment-inputs").join(run_id);
    ensure_private_dir(&directory)?;
    let mut inputs = BTreeMap::new();
    for (id, source) in deployment_input_paths(paths, &snapshot.spec)? {
        let file =
            File::open(&source).with_context(|| format!("failed to read deployment input {id}"))?;
        anyhow::ensure!(
            file.metadata()?.is_file(),
            "deployment input {id} must be a regular file"
        );
        let mut content = Vec::new();
        file.take(100 * 1024 * 1024 + 1).read_to_end(&mut content)?;
        anyhow::ensure!(
            content.len() <= 100 * 1024 * 1024,
            "deployment input {id} exceeds 100 MiB"
        );
        let sha256 = hex_encode(&Sha256::digest(&content));
        // Generated management credentials keep their existing identity paths;
        // user resources and the disk secret are delivered from private copies.
        let staged = if !matches!(
            id.as_str(),
            "resource:cai_ssh_authorized_keys" | "resource:cai_ssh_host_key"
        ) && (snapshot
            .spec
            .resources
            .keys()
            .any(|key| id == format!("resource:{key}"))
            || id == "secret:disk_passphrase")
        {
            let staged = directory.join(hex_encode(&Sha256::digest(id.as_bytes())));
            write_private_file(&staged, &content, 0o600)?;
            Some(staged)
        } else {
            None
        };
        inputs.insert(
            id,
            DeploymentInput {
                source,
                sha256,
                snapshot: staged,
            },
        );
    }
    snapshot.inputs = Some(inputs);
    Ok(())
}

pub(super) fn deployment_delivery_spec(snapshot: &DeploymentSnapshot) -> Result<AgentSpec> {
    let mut spec = snapshot.spec.clone();
    if let Some(inputs) = &snapshot.inputs {
        for (id, input) in inputs {
            if let Some(path) = &input.snapshot {
                anyhow::ensure!(
                    sha256_file(path)? == input.sha256,
                    "saved deployment input {id} was modified"
                );
                if id == "secret:disk_passphrase" {
                    spec.secrets.disk_passphrase = Some(path.clone());
                } else if let Some(resource) = id
                    .strip_prefix("resource:")
                    .and_then(|id| spec.resources.get_mut(id))
                {
                    resource.source = path.clone();
                }
            }
        }
    }
    Ok(spec)
}

fn immutable_deployment_input(id: &str) -> bool {
    id.starts_with("secret:")
        || id.starts_with("identity:")
        || matches!(
            id,
            "resource:cai_ssh_authorized_keys" | "resource:cai_ssh_host_key"
        )
}

/// Check the instance binding before either resource provider can be updated.
pub(super) fn validated_injection_spec(
    state: &LocalServiceState,
    manifest: &BuildManifest,
    current: &AgentSpec,
    paths: &ContextPaths,
) -> Result<AgentSpec> {
    let saved = committed_deployment(state, manifest)?;
    anyhow::ensure!(
        current.deploy.replicas == saved.spec.deploy.replicas
            && current.deploy.slb == saved.spec.deploy.slb,
        "inject cannot change the deployed replica count or load balancer topology"
    );

    anyhow::ensure!(
        (current.image_id() == saved.spec.image_id()
            || (current.deploy.image.is_some() && saved.spec.deploy.image.is_some()))
            && current.image_variant() == state.build.variant,
        "inject cannot change the deployed image; deploy a new instance to use a different image"
    );
    let mut deployed = saved.spec.deploy.clone();
    let mut proposed = current.deploy.clone();
    deployed.image_variant = Some(state.build.variant.clone());
    proposed.image_variant = Some(current.image_variant().to_string());
    // The saved release and publisher key are the authorization. Download
    // locations are not instance identity, and need not remain available.
    if let (Some(image), Some(committed)) = (&mut proposed.image, &manifest.image) {
        anyhow::ensure!(
            image
                .manifest
                .as_ref()
                .map_or(true, |pin| committed.source.manifest.as_ref() == Some(pin)),
            "inject cannot change the deployed image manifest"
        );
        // A user-supplied version pin is optional; the authenticated snapshot
        // remains the identity, including for deployments made by older CLIs.
        image.manifest = saved
            .spec
            .deploy
            .image
            .as_ref()
            .context("missing committed image release")?
            .manifest
            .clone();
        for (name, source, frozen) in [
            ("release", &image.release, &committed.source.release),
            (
                "publisher key",
                &image.trusted_key,
                &committed.source.trusted_key,
            ),
        ] {
            if name == "publisher key" && committed.source.skip_signature_verification {
                continue;
            }
            let frozen_digest = sha256_file(frozen)
                .with_context(|| format!("failed to read committed image {name}"))?;
            match fs::symlink_metadata(source) {
                Ok(_) => anyhow::ensure!(sha256_file(source)? == frozen_digest,
                    "inject cannot change the committed image {name}; deploy a new instance to select another release"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).with_context(|| format!("failed to inspect image {name}")),
            }
        }
        image.release = saved
            .spec
            .deploy
            .image
            .as_ref()
            .context("missing committed image release")?
            .release
            .clone();
        image.trusted_key = saved
            .spec
            .deploy
            .image
            .as_ref()
            .context("missing committed publisher key")?
            .trusted_key
            .clone();
    }
    anyhow::ensure!(
        serde_json::to_value(&proposed)? == serde_json::to_value(&deployed)?,
        "inject cannot change the committed image release or infrastructure configuration; deploy a new instance for those changes"
    );
    anyhow::ensure!(
        serde_json::to_value(&current.attestation)?
            == serde_json::to_value(&saved.spec.attestation)?
            && current.service.ssh == saved.spec.service.ssh,
        "inject cannot change the committed attestation or management identity configuration"
    );
    let expected = saved.inputs.as_ref().context(
        "legacy deployment has no frozen input binding; run `ca upgrade check` before rebooting and `ca upgrade bind-inputs --service SERVICE` when historical evidence is available; keep the previous host toolchain for unsupported legacy state (do not destroy a data-bearing instance)",
    )?;
    let actual = deployment_input_paths(paths, current)?;
    let actual_immutable: BTreeSet<_> = actual
        .keys()
        .filter(|id| immutable_deployment_input(id))
        .collect();
    let expected_immutable: BTreeSet<_> = expected
        .keys()
        .filter(|id| immutable_deployment_input(id))
        .collect();
    anyhow::ensure!(
        actual_immutable == expected_immutable,
        "inject cannot change the committed disk or management identity input set"
    );
    for id in expected_immutable {
        let input = &expected[id];
        anyhow::ensure!(sha256_file(&actual[id])? == input.sha256,
            "inject cannot change the contents of committed input {id}; moving an unchanged file is allowed, but disk key rotation and identity replacement are not supported by inject");
    }
    // Deliver the committed disk bytes, closing the gap between checking a
    // mutable source file and sending its contents to CDH or KBS.
    let disk = expected
        .get("secret:disk_passphrase")
        .context("deployment has no frozen disk binding")?;
    let frozen = disk
        .snapshot
        .as_ref()
        .context("deployment has no saved disk secret")?;
    anyhow::ensure!(
        sha256_file(frozen)? == disk.sha256,
        "saved deployment disk secret was modified"
    );
    let mut delivery = current.clone();
    delivery.build = saved.spec.build.clone();
    delivery.deploy = saved.spec.deploy.clone();
    if let Some(committed) = &manifest.image {
        delivery.deploy.image = Some(committed.source.clone());
    }
    delivery.secrets.disk_passphrase = Some(frozen.clone());
    Ok(delivery)
}

pub(super) fn validate_resume_inputs(
    saved: &DeploymentSnapshot,
    current: &DeploymentSnapshot,
    paths: &ContextPaths,
) -> Result<()> {
    anyhow::ensure!(current.source.path == saved.source.path && current.source.sha256 == saved.source.sha256,
        "--resume requires the original deployment configuration and canonical path; restore them before resuming resource delivery");
    let expected = saved.inputs.as_ref().context("this deployment has no frozen resource inputs; run `ca upgrade check` and migrate using historical evidence before resuming; retain the previous host toolchain for unsupported legacy state")?;
    let actual = deployment_input_paths(paths, &current.spec)?;
    anyhow::ensure!(
        actual.keys().eq(expected.keys()),
        "--resume deployment input set changed"
    );
    for (id, path) in actual {
        let input = &expected[&id];
        if path != input.source || sha256_file(&path)? != input.sha256 {
            if immutable_deployment_input(&id) {
                bail!("--resume committed input {id} changed (source path or contents); restore the original input; inject cannot rotate disk keys or replace instance identities");
            }
            bail!("--resume deployment input {id} changed (source path or contents); restore the original input, or use an explicit inject to update application resources");
        }
    }
    deployment_delivery_spec(saved)?;
    Ok(())
}

pub(super) fn committed_deployment(
    state: &LocalServiceState,
    manifest: &BuildManifest,
) -> Result<DeploymentSnapshot> {
    let snapshot = match &manifest.deployment {
        Some(snapshot) => snapshot.clone(),
        None => {
            // Older state can be migrated only from its original, unchanged input.
            let mut snapshot = deployment_snapshot(&state.spec.path)
                .context("deployment has no saved configuration; restore its original AppSpec before refreshing network rules")?;
            anyhow::ensure!(
                snapshot.source.sha256 == state.spec.sha256,
                "deployment has no saved configuration and its AppSpec was modified; restore the original AppSpec before refreshing network rules"
            );
            snapshot.spec.deploy.image_variant = Some(state.build.variant.clone());
            if let Ok(raw) = fs::read(&manifest.shelter_config) {
                let config: serde_json::Value = serde_yaml::from_slice(&raw)
                    .context("invalid saved Shelter deployment configuration")?;
                snapshot.user_data = config
                    .pointer("/deploy/user_data")
                    .or_else(|| {
                        config.pointer("/runtime/targets/deploy/provider_options/user_data")
                    })
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
            }
            anyhow::ensure!(
                state.attestation_mode != "trustee" || snapshot.user_data.is_some(),
                "legacy Trustee deployment has no saved bootstrap; restore its Shelter configuration before refreshing network rules"
            );
            snapshot
        }
    };
    anyhow::ensure!(
        snapshot.spec.service.id == state.service_id,
        "saved deployment configuration belongs to a different service"
    );
    Ok(snapshot)
}

pub(super) fn context_paths(state_dir: &Path, service_id: &str) -> ContextPaths {
    let state_dir = absolute_path_for_state(state_dir);
    let service_dir = state_dir.join("services").join(service_id);
    let artifacts_dir = service_dir.join("artifacts");
    let shelter_work_dir = service_dir.join("shelter");
    ContextPaths {
        shelter_work_dir,
        artifacts_dir,
        cache_dir: service_dir.join("shelter").join("cache"),
        guest_staging_dir: service_dir.join("guest"),
        secrets_dir: service_dir.join("secrets"),
        rendered_config: service_dir.join("shelter.yaml"),
        manifest: service_dir.join("manifest.json"),
        bootstrap_file: service_dir.join("bootstrap.json"),
        service_state: service_dir.join("state.json"),
        agent_card: service_dir.join("agent-card.json"),
        service_dir,
    }
}

pub(super) fn prepared_from_state(
    _state_dir: &Path,
    state: &LocalServiceState,
    manifest: &BuildManifest,
) -> Result<PreparedConfig> {
    let variant = manifest.variant(&state.build.variant, Some(&state.build.variant))?;
    Ok(PreparedConfig {
        image: manifest.image.clone(),
        rendered_config: manifest.shelter_config.clone(),
        shelter_build_id: variant.shelter_build_id,
        shelter_work_dir: manifest.shelter_work_dir.clone(),
        build_result: variant.build_result,
        deploy_result: manifest.deploy_result.clone(),
        deploy_names: Some(DeployNames {
            run_id: state.deploy.run_id.clone(),
            resource_name: state.deploy.resource_name.clone(),
            image_import_name: state
                .deploy
                .image_import_name
                .clone()
                .unwrap_or_else(|| format!("{}-{}", state.build.image_name, state.deploy.run_id)),
        }),
        terraform_dir: state.deploy.terraform_dir.clone(),
        debug_ssh: variant.debug_ssh,
        cloud_image_id: state.deploy.published_image_id.clone(),
    })
}

pub(super) fn shelter_build_result_path(work_dir: &Path, build_id: &str) -> PathBuf {
    work_dir
        .join("images")
        .join(build_id)
        .join("build-result.json")
}

pub(super) fn shelter_deploy_result_path(terraform_dir: &Path) -> PathBuf {
    terraform_dir.join("deploy-result.json")
}

pub(super) fn read_shelter_build_result(
    path: &Path,
    expected_id: &str,
) -> Result<ShelterBuildResult> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read Shelter build result '{}'", path.display()))?;
    let result: ShelterBuildResult = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse Shelter build result '{}'", path.display()))?;
    if result.id != expected_id {
        bail!(
            "Shelter build result '{}' contains id '{}', expected '{}'",
            path.display(),
            result.id,
            expected_id
        );
    }
    Ok(result)
}

pub(super) fn materialize_shelter_build_artifacts(
    paths: &ContextPaths,
    result_path: &Path,
    build_id: &str,
) -> Result<ShelterBuildArtifacts> {
    let result = read_shelter_build_result(result_path, build_id)?;
    fs::create_dir_all(&paths.service_dir)
        .with_context(|| format!("failed to create '{}'", paths.service_dir.display()))?;
    let sample_values = result
        .reference_value
        .as_ref()
        .map(rvps_sample_values)
        .transpose()?;
    let sample_rv = write_json_artifact(
        sample_values.as_ref(),
        &paths.service_dir.join("shelter-reference-values.json"),
    )?;
    let rekor_meta = write_json_artifact(
        result.rekor_value.as_ref(),
        &paths.service_dir.join("shelter-rekor-meta.json"),
    )?;
    Ok(ShelterBuildArtifacts {
        image_path: result.image_path,
        sample_rv,
        rekor_meta,
    })
}

/// RVPS sample input is a map of string arrays. Shelter also publishes
/// namespaced boot metadata, which remains in the original build result.
fn rvps_sample_values(value: &serde_json::Value) -> Result<serde_json::Value> {
    let mut object = value
        .as_object()
        .context("Shelter reference values must be an object")?
        .clone();
    object.remove("shelter.reference-values.v1");
    let samples: BTreeMap<String, Vec<String>> =
        serde_json::from_value(serde_json::Value::Object(object))
            .context("Shelter external reference values must contain string arrays")?;
    let uki = samples
        .get("measurement.uki.SHA-384")
        .context("Shelter reference values are missing the UKI SHA-384 bank required by CA")?;
    if uki.len() != 1 || uki[0].len() != 96 || !uki[0].bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("Shelter UKI SHA-384 reference must contain exactly one 96-digit hex digest");
    }
    Ok(serde_json::to_value(samples)?)
}

pub(super) fn write_json_artifact(
    value: Option<&serde_json::Value>,
    path: &Path,
) -> Result<Option<PathBuf>> {
    match value {
        Some(value) => {
            fs::write(path, serde_json::to_string_pretty(value)?)
                .with_context(|| format!("failed to write '{}'", path.display()))?;
            Ok(Some(path.to_path_buf()))
        }
        None => {
            if path.exists() {
                fs::remove_file(path)
                    .with_context(|| format!("failed to remove stale '{}'", path.display()))?;
            }
            Ok(None)
        }
    }
}

pub(super) fn absolute_path_for_state(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

/// Check the compiled plan, since older Shelter accepts unknown initrd keys.
pub(super) fn verify_shelter_initrd_support(cli: &Cli, config: &Path) -> Result<()> {
    let binary = effective_shelter_bin(cli);
    let output = shelter_command(
        cli,
        &[
            "builder".into(),
            "plan".into(),
            "-f".into(),
            config.as_os_str().to_os_string(),
        ],
    )?
    .stdin(Stdio::null())
    .output()
    .with_context(|| {
        format!(
            "failed to run Shelter capability check: {}",
            binary.display()
        )
    })?;
    anyhow::ensure!(output.status.success(),
        "Shelter plan failed for '{}'; install the bundled Shelter with initrd.binaries and split Cryptpilot support: {}",
        binary.display(), String::from_utf8_lossy(&output.stderr));
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("Shelter plan did not return a JSON build plan")?;
    let config: serde_yaml::Value = serde_yaml::from_str(&fs::read_to_string(config)?)?;
    verify_initrd_binaries_in_plan(&config, &plan)
}

pub(super) fn verify_initrd_binaries_in_plan(
    config: &serde_yaml::Value,
    plan: &serde_json::Value,
) -> Result<()> {
    let required = config["initrd"]["binaries"]
        .as_sequence()
        .context("rendered Shelter config lacks initrd.binaries")?;
    let planned = plan
        .pointer("/plan/early_boot/binaries")
        .and_then(|v| v.as_array())
        .context(
            "Shelter lacks initrd.binaries support; install the bundled Shelter before building",
        )?;
    for path in required {
        let path = path.as_str().context("invalid initrd binary path")?;
        anyhow::ensure!(planned.iter().any(|v| v.as_str() == Some(path)),
            "Shelter plan omitted required initrd binary {path}; install the bundled Shelter before building");
    }
    Ok(())
}

pub(super) fn run_shelter(cli: &Cli, args: &mut [OsString]) -> Result<()> {
    let shelter_bin = effective_shelter_bin(cli);
    let mut command = shelter_command(cli, args)?;
    let status = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to execute '{}'", shelter_bin.display()))?;

    if !status.success() {
        bail!(
            "shelter exited with status {}; check rendered Shelter work directories under '{}'",
            status,
            cli.state_dir.display()
        );
    }
    Ok(())
}

pub(super) fn shelter_command(cli: &Cli, args: &[OsString]) -> Result<Command> {
    let mut command = Command::new(effective_shelter_bin(cli));
    command.args(args);
    command.env("PATH", native_tools_path(&cli.tools_dir)?).env(
        "COSIGN_PASSWORD",
        std::env::var_os("COSIGN_PASSWORD").unwrap_or_default(),
    );
    // All callers use either a bare subcommand or the global --work-dir pair.
    // Inspect only the command, so an image tag cannot select credentials.
    let subcommand = match args {
        [option, _, command, ..] if option == "--work-dir" => command.to_str(),
        [command, ..] => command.to_str(),
        [] => None,
    };
    if matches!(subcommand, Some("run" | "rm" | "network" | "inspect")) {
        if let Some(credentials) = cloud_credentials::load_if_configured()? {
            command.envs(credentials.environment());
        }
    }
    Ok(command)
}

pub(super) fn effective_shelter_bin(cli: &Cli) -> PathBuf {
    effective_shelter_bin_from_candidates(
        cli,
        &[
            PathBuf::from("/usr/bin/shelter"),
            PathBuf::from("/usr/local/bin/shelter"),
        ],
    )
}

pub(super) fn effective_shelter_bin_from_candidates(cli: &Cli, candidates: &[PathBuf]) -> PathBuf {
    if cli.shelter_bin != PathBuf::from("shelter") || std::env::var_os("CA_SHELTER_BIN").is_some() {
        return cli.shelter_bin.clone();
    }

    candidates
        .iter()
        .find(|candidate| candidate.is_file())
        .cloned()
        .unwrap_or_else(|| cli.shelter_bin.clone())
}

pub(super) fn bind_shelter_deployment(cli: &Cli, prepared: &mut PreparedConfig) -> Result<()> {
    let expected_name = prepared
        .deploy_names
        .as_ref()
        .map(|names| names.resource_name.as_str())
        .unwrap_or(&prepared.shelter_build_id);
    let output = shelter_command(
        cli,
        &[
            "--work-dir".into(),
            prepared.shelter_work_dir.as_os_str().to_os_string(),
            "inspect".into(),
            expected_name.into(),
            "--format".into(),
            "json".into(),
        ],
    )?
    .output()
    .context("failed to query Shelter deployment identity")?;
    anyhow::ensure!(
        output.status.success(),
        "Shelter deployment status failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("failed to parse Shelter deployment status")?;
    anyhow::ensure!(
        status["schema_version"] == "io.shelter.instance-status/v1"
            && status["name"].as_str() == Some(expected_name)
            && status["location"] == "remote",
        "Shelter status does not identify managed deployment '{expected_name}'"
    );
    let id = status["id"]
        .as_str()
        .filter(|id| id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .context("Shelter status contains an invalid deployment ID")?;
    let terraform_dir = prepared.shelter_work_dir.join("deploy").join(id);
    let deploy_result = shelter_deploy_result_path(&terraform_dir);
    let result = read_shelter_deploy_result(&deploy_result, id)?;
    anyhow::ensure!(
        result.id == id,
        "Shelter deploy result does not match deployment ID '{id}'"
    );
    prepared.terraform_dir = Some(terraform_dir);
    prepared.deploy_result = deploy_result;
    Ok(())
}

pub(super) fn resolve_deploy_observation(
    prepared: &PreparedConfig,
    spec: &AgentSpec,
) -> Result<DeployObservation> {
    let expected_name = prepared
        .deploy_names
        .as_ref()
        .map(|names| names.resource_name.as_str())
        .unwrap_or(&prepared.shelter_build_id);
    let expected = if prepared.image.is_some() {
        prepared
            .terraform_dir
            .as_ref()
            .and_then(|directory| directory.file_name())
            .and_then(|id| id.to_str())
            .context("published image deployment has no bound Shelter deployment ID")?
    } else {
        expected_name
    };
    let deploy_result = read_shelter_deploy_result(&prepared.deploy_result, expected)?;
    if prepared.image.is_some() {
        anyhow::ensure!(
            deploy_result.id == expected,
            "published image deploy result has a different Shelter deployment ID"
        );
    }
    let public_ip = deploy_result_value_as_string(
        deploy_result
            .deploy
            .public_ip
            .as_ref()
            .or_else(|| deploy_result.deploy.outputs.get("public_ip")),
    );
    let private_ip = deploy_result_value_as_string(
        deploy_result
            .deploy
            .private_ip
            .as_ref()
            .or_else(|| deploy_result.deploy.outputs.get("private_ip")),
    )
    .or_else(|| spec.deploy.private_ip.clone());
    let instance_ids = deploy_result_string_list(deploy_result.deploy.outputs.get("instance_ids"));
    let instance_private_ips =
        deploy_result_string_list(deploy_result.deploy.outputs.get("instance_private_ips"));
    let instance_public_ips =
        deploy_result_string_list(deploy_result.deploy.outputs.get("instance_public_ips"));
    let slb_vip = deploy_result_value_as_string(deploy_result.deploy.outputs.get("slb_vip"));
    if spec.deploy.replicas > 1 {
        anyhow::ensure!(
            instance_ids.len() == spec.deploy.replicas as usize,
            "Shelter outputs do not contain the expected replica count"
        );
        for (name, values) in [
            ("private", &instance_private_ips),
            ("public", &instance_public_ips),
        ] {
            anyhow::ensure!(
                values.is_empty() || values.len() == instance_ids.len(),
                "Shelter {name} address list is not aligned with replica IDs"
            );
            for value in values {
                value
                    .parse::<std::net::Ipv4Addr>()
                    .with_context(|| format!("invalid {name} replica address"))?;
            }
        }
        anyhow::ensure!(
            !instance_private_ips.is_empty(),
            "Shelter has no private replica addresses"
        );
        let unique: BTreeSet<_> = instance_ids.iter().collect();
        anyhow::ensure!(
            unique.len() == instance_ids.len() && instance_ids.iter().all(|id| !id.is_empty()),
            "Shelter replica IDs are empty or duplicated"
        );
    }

    Ok(DeployObservation {
        instance_id: deploy_result_value_as_string(
            deploy_result
                .deploy
                .instance_id
                .as_ref()
                .or_else(|| deploy_result.deploy.outputs.get("instance_id")),
        ),
        security_group_id: deploy_result_value_as_string(
            deploy_result.deploy.outputs.get("security_group_id"),
        ),
        public_ip,
        private_ip,
        instance_ids,
        instance_private_ips,
        instance_public_ips,
        slb_vip,
    })
}

pub(super) fn read_shelter_deploy_result(
    path: &Path,
    expected_id: &str,
) -> Result<ShelterDeployResult> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read Shelter deploy result '{}'", path.display()))?;
    let result: ShelterDeployResult = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse Shelter deploy result '{}'", path.display()))?;
    // Status binds an instance alias to its durable ID. A release deployment's
    // provider resource name is independent of that alias.
    if result.deploy.name.as_deref() != Some(expected_id) && result.id != expected_id {
        bail!(
            "Shelter deploy result '{}' contains deployment '{}' named {:?}, expected '{}'",
            path.display(),
            result.id,
            result.deploy.name,
            expected_id
        );
    }
    Ok(result)
}

pub(super) fn deploy_result_value_as_string(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::String(value) => non_empty_string(value),
        serde_json::Value::Object(map) => map.get("value").and_then(|value| match value {
            serde_json::Value::String(value) => non_empty_string(value),
            other => non_empty_string(&other.to_string()),
        }),
        serde_json::Value::Null => None,
        other => non_empty_string(&other.to_string()),
    }
}

/// Extracts a list of non-empty strings from a Terraform-style deploy result output.
/// Handles both bare arrays (`["a","b"]`) and wrapped objects (`{"value": ["a","b"]}`).
pub(super) fn deploy_result_string_list(value: Option<&serde_json::Value>) -> Vec<String> {
    let array = match value {
        Some(serde_json::Value::Array(arr)) => arr,
        Some(serde_json::Value::Object(map)) => match map.get("value") {
            Some(serde_json::Value::Array(arr)) => arr,
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    array
        .iter()
        .filter_map(|item| match item {
            serde_json::Value::String(s) => non_empty_string(s),
            _ => None,
        })
        .collect()
}

pub(super) fn non_empty_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty() && trimmed != "null").then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn context_paths_structure() {
        let paths = context_paths(Path::new("/state"), "openclaw");
        assert_eq!(paths.service_dir, PathBuf::from("/state/services/openclaw"));
        assert_eq!(
            paths.shelter_work_dir,
            PathBuf::from("/state/services/openclaw/shelter")
        );
        assert_eq!(
            paths.secrets_dir,
            PathBuf::from("/state/services/openclaw/secrets")
        );
        assert_eq!(
            paths.service_state,
            PathBuf::from("/state/services/openclaw/state.json")
        );
        assert_eq!(
            paths.agent_card,
            PathBuf::from("/state/services/openclaw/agent-card.json")
        );
        assert_eq!(
            paths.manifest,
            PathBuf::from("/state/services/openclaw/manifest.json")
        );
        assert_eq!(
            paths.rendered_config,
            PathBuf::from("/state/services/openclaw/shelter.yaml")
        );
    }

    #[test]
    fn shelter_build_result_path_structure() {
        let path = shelter_build_result_path(Path::new("/work"), "build-123");
        assert_eq!(
            path,
            PathBuf::from("/work/images/build-123/build-result.json")
        );
    }

    #[test]
    fn shelter_deploy_result_path_structure() {
        let path = shelter_deploy_result_path(Path::new("/terraform"));
        assert_eq!(path, PathBuf::from("/terraform/deploy-result.json"));
    }

    #[test]
    fn read_shelter_build_result_parses_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("build-result.json");
        let data = json!({
            "id": "build-42",
            "image_path": "/images/disk.qcow2",
            "reference_value": null,
            "rekor_value": null
        });
        fs::write(&path, serde_json::to_string(&data).unwrap()).unwrap();
        let result = read_shelter_build_result(&path, "build-42").unwrap();
        assert_eq!(result.id, "build-42");
    }

    #[test]
    fn read_shelter_build_result_rejects_id_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("build-result.json");
        let data = json!({
            "id": "build-42",
            "image_path": "/images/disk.qcow2",
            "reference_value": null,
            "rekor_value": null
        });
        fs::write(&path, serde_json::to_string(&data).unwrap()).unwrap();
        let err = read_shelter_build_result(&path, "wrong-id").unwrap_err();
        assert!(err.to_string().contains("wrong-id"));
    }

    #[test]
    fn rvps_sample_keeps_uki_banks_and_excludes_shelter_metadata() {
        let values = json!({
            "measurement.uki.SHA-256": ["a".repeat(64)],
            "measurement.uki.SHA-384": ["b".repeat(96)],
            "shelter.reference-values.v1": {"measured_boot": {"tdx_rtmr2_sha384": "c".repeat(96)}}
        });
        let sample = rvps_sample_values(&values).unwrap();
        assert_eq!(sample.as_object().unwrap().len(), 2);
        assert_eq!(sample["measurement.uki.SHA-384"], json!(["b".repeat(96)]));
        assert!(rvps_sample_values(
            &json!({"shelter.reference-values.v1": values["shelter.reference-values.v1"]})
        )
        .is_err());
        let mut bad = values.clone();
        bad["measurement.uki.SHA-384"] = json!(["bad"]);
        assert!(rvps_sample_values(&bad).is_err());
        bad["measurement.uki.SHA-384"] = json!({"unexpected": "object"});
        assert!(rvps_sample_values(&bad).is_err());
    }

    #[test]
    fn write_json_artifact_writes_some() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact.json");
        let value = json!({"key": "value"});
        let result = write_json_artifact(Some(&value), &path).unwrap();
        assert_eq!(result, Some(path.clone()));
        let content: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["key"], "value");
    }

    #[test]
    fn write_json_artifact_removes_stale_on_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact.json");
        fs::write(&path, "old").unwrap();
        assert!(path.exists());
        let result = write_json_artifact(None, &path).unwrap();
        assert!(result.is_none());
        assert!(!path.exists());
    }

    #[test]
    fn write_json_artifact_none_nonexistent_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact.json");
        assert!(write_json_artifact(None, &path).unwrap().is_none());
    }

    #[test]
    fn absolute_path_for_state_keeps_absolute() {
        assert_eq!(
            absolute_path_for_state(Path::new("/absolute")),
            PathBuf::from("/absolute")
        );
    }

    #[test]
    fn absolute_path_for_state_resolves_relative() {
        let result = absolute_path_for_state(Path::new("relative"));
        assert!(result.is_absolute());
    }

    #[test]
    fn effective_shelter_bin_prefers_existing_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let candidate = dir.path().join("shelter");
        fs::write(&candidate, "#!/bin/sh").unwrap();
        let cli = crate::cli::Cli {
            command: crate::cli::Commands::Version,
            shelter_bin: PathBuf::from("shelter"),
            state_dir: PathBuf::from("/state"),
            tools_dir: PathBuf::from("/usr/libexec/cagent/bin"),
        };
        let result = effective_shelter_bin_from_candidates(&cli, &[candidate.clone()]);
        assert_eq!(result, candidate);
    }

    #[test]
    fn effective_shelter_bin_returns_cli_default_when_no_candidate() {
        let cli = crate::cli::Cli {
            command: crate::cli::Commands::Version,
            shelter_bin: PathBuf::from("shelter"),
            state_dir: PathBuf::from("/state"),
            tools_dir: PathBuf::from("/usr/libexec/cagent/bin"),
        };
        let result =
            effective_shelter_bin_from_candidates(&cli, &[PathBuf::from("/nonexistent/shelter")]);
        assert_eq!(result, PathBuf::from("shelter"));
    }

    #[test]
    fn deploy_result_value_as_string_plain_string() {
        let v = json!("10.0.0.1");
        assert_eq!(
            deploy_result_value_as_string(Some(&v)),
            Some("10.0.0.1".to_string())
        );
    }

    #[test]
    fn deploy_result_value_as_string_object_with_value() {
        let v = json!({"value": "10.0.0.1"});
        assert_eq!(
            deploy_result_value_as_string(Some(&v)),
            Some("10.0.0.1".to_string())
        );
    }

    #[test]
    fn deploy_result_value_as_string_null_returns_none() {
        assert!(deploy_result_value_as_string(Some(&json!(null))).is_none());
        assert!(deploy_result_value_as_string(None).is_none());
    }

    #[test]
    fn deploy_result_value_as_string_number() {
        let v = json!(42);
        assert_eq!(
            deploy_result_value_as_string(Some(&v)),
            Some("42".to_string())
        );
    }

    #[test]
    fn non_empty_string_filters() {
        assert_eq!(non_empty_string("hello"), Some("hello".to_string()));
        assert!(non_empty_string("").is_none());
        assert!(non_empty_string("  ").is_none());
        assert!(non_empty_string("null").is_none());
    }
}
