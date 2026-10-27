//! Reuse publisher-authenticated image metadata with CA's existing resource flow.
use super::*;
use confidential_agent_core::spec::PublishedImageSpec;
use confidential_agent_shelter::render_image_runtime_target;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ImageDeployment {
    pub source: PublishedImageSpec,
    pub image_id: String,
}

#[derive(Deserialize)]
struct ImageInspection {
    schema_version: String,
    manifest_digest: String,
    image: ImageMapping,
    reference_value: serde_json::Value,
    rekor_value: Option<serde_json::Value>,
    security: serde_json::Value,
    runtime_metadata: Option<image_runtime::ImageRuntimeContract>,
}

#[derive(Deserialize)]
struct ImageMapping {
    provider: String,

    region: String,
    image_id: String,
}

fn copy_metadata(source: &Path, destination: &Path, limit: u64) -> Result<()> {
    let file = File::open(source)
        .with_context(|| format!("failed to read image metadata '{}'", source.display()))?;
    anyhow::ensure!(
        file.metadata()?.is_file(),
        "image metadata must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= limit,
        "image metadata exceeds the size limit"
    );
    write_private_file(destination, &bytes, 0o600)
}

pub(super) fn import(cli: &Cli, spec: &AgentSpec) -> Result<BuildManifest> {
    let source = spec
        .deploy
        .image
        .as_ref()
        .context("no published image selected")?;
    let paths = context_paths(&cli.state_dir, &spec.service.id);
    ensure_private_context_dirs(&paths)?;
    fs::create_dir_all(&paths.artifacts_dir)?;
    let mut source = source.clone();
    let release = paths.artifacts_dir.join("image-release.json");
    let trusted_key = paths.artifacts_dir.join("image-publisher.pem");
    // Authenticate the snapshots that this deployment will use, including on
    // later reconciliation. Never copy producer state or private signing keys.
    copy_metadata(&source.release, &release, 16 * 1024 * 1024)?;
    if !source.skip_signature_verification {
        copy_metadata(&source.trusted_key, &trusted_key, 64 * 1024)?;
        source.trusted_key = trusted_key;
    } else {
        source.trusted_key = PathBuf::new();
        println!("[ca] release signature verification explicitly skipped; runtime attestation remains required");
    }
    source.release = release;
    let mut command = shelter_command(cli, &[])?;
    command
        .args(["image", "inspect-release"])
        .arg(&source.release)
        .arg("--region")
        .arg(&spec.deploy.region);
    if let Some(digest) = &source.manifest {
        command.arg("--expect-manifest").arg(digest);
    }
    if source.skip_signature_verification {
        command.arg("--skip-signature-verification");
    } else {
        command.arg("--trusted-key").arg(&source.trusted_key);
    }
    if let Some(id) = &source.image_id {
        command.arg("--image-id").arg(id);
    }
    let output = command
        .output()
        .context("failed to authenticate published image with Shelter")?;
    anyhow::ensure!(
        output.status.success(),
        "published image authentication failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let inspected: ImageInspection =
        serde_json::from_slice(&output.stdout).context("invalid Shelter cloud-image inspection")?;
    anyhow::ensure!(
        inspected.schema_version == "io.shelter.cloud-image-inspection/v1"
            && source
                .manifest
                .as_ref()
                .map_or(true, |pin| pin == &inspected.manifest_digest)
            && inspected.image.provider == "terraform-alicloud"
            && inspected.image.region == spec.deploy.region
            && source
                .image_id
                .as_ref()
                .map_or(true, |id| id == &inspected.image.image_id),
        "Shelter inspection does not match the selected image and deployment target"
    );
    let digest = inspected
        .manifest_digest
        .strip_prefix("sha256:")
        .unwrap_or_default();
    anyhow::ensure!(
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "Shelter inspection returned an invalid manifest digest"
    );
    // Freeze the authenticated identity even when the operator did not supply a
    // version pin. Subsequent preparation and apply use this exact snapshot.
    source.manifest = Some(inspected.manifest_digest.clone());
    if spec.attestation.reference_values == ReferenceValueMode::Rekor {
        anyhow::ensure!(inspected.rekor_value.is_some(), "published image has no Rekor metadata; select sample reference values or export an image built with Rekor");
    }
    let runtime_contract = inspected.runtime_metadata.context("published image has no CA runtime contract; rebuild and export it with a compatible CA publisher")?;
    runtime_contract.validate_deploy(spec, &inspected.security)?;
    let build_id = format!("image-{digest}");
    let build_result = paths.artifacts_dir.join("image-metadata.json");
    // Compatibility projection for the existing CA attestation/resource code.
    // This is metadata, never a fabricated local Shelter artifact or QCOW2.
    write_json_atomic(
        &build_result,
        &serde_json::json!({
            "tag": build_id,
            "image_path": "",
            "reference_value": inspected.reference_value,
            "rekor_value": inspected.rekor_value,
        }),
    )?;
    materialize_shelter_build_artifacts(&paths, &build_result, &build_id)?;
    Ok(BuildManifest {
        runtime_contract: Some(runtime_contract),
        ssh: instance_ssh::identity(&paths, spec)?,
        image: Some(ImageDeployment {
            source,
            image_id: inspected.image.image_id,
        }),
        service_id: spec.service.id.clone(),
        shelter_build_id: build_id,
        shelter_work_dir: paths.shelter_work_dir,
        build_result,
        deploy_result: paths.service_dir.join("deploy-result.json"),
        shelter_config: paths.rendered_config,
        images_dir: paths.artifacts_dir,
        cache_dir: paths.cache_dir,
        ..BuildManifest::default()
    })
}

pub(super) fn prepare(
    paths: &ContextPaths,
    spec: &AgentSpec,
    manifest: &BuildManifest,
    options: &PrepareOptions,
) -> Result<PreparedConfig> {
    let image = manifest
        .image
        .clone()
        .context("published image metadata is missing")?;
    let rendered = render_image_runtime_target(
        spec,
        &ShelterRenderOptions {
            deploy_resource_name: options
                .deploy_names
                .as_ref()
                .map(|names| names.resource_name.clone()),
            mesh_peer_cidrs: options.mesh_peer_cidrs.clone(),
            peerings: options.peerings.clone(),
            deploy_user_data: options.deploy_user_data.clone(),
            ..ShelterRenderOptions::default()
        },
        image.source.image_id.as_deref(),
    )?;
    fs::create_dir_all(&paths.shelter_work_dir)?;
    fs::write(&paths.rendered_config, rendered)?;
    write_json_atomic(&paths.manifest, manifest)?;
    Ok(PreparedConfig {
        image: Some(image.clone()),
        rendered_config: paths.rendered_config.clone(),
        shelter_build_id: manifest.shelter_build_id.clone(),
        shelter_work_dir: paths.shelter_work_dir.clone(),
        build_result: manifest.build_result.clone(),
        deploy_result: manifest.deploy_result.clone(),
        deploy_names: options.deploy_names.clone(),
        terraform_dir: None,
        debug_ssh: None,
        cloud_image_id: Some(image.image_id),
    })
}

pub(super) fn export(
    cli: &Cli,
    service: &str,
    image_id: Option<&str>,
    signing_key: &Path,
    output: &Path,
    region: Option<&str>,
    variant: Option<&str>,
) -> Result<()> {
    let _operation = lock_service_operation(&cli.state_dir, service)?;
    let paths = context_paths(&cli.state_dir, service);
    let mut state = read_service_state_file(&paths.service_state)?
        .context("image export requires existing build metadata")?;
    let manifest = read_build_manifest(&paths.manifest)?;
    anyhow::ensure!(
        manifest.image.is_none(),
        "image export requires the original local artifact, not an imported release"
    );
    let spec = AgentSpec::from_path(&state.spec.path)?;
    let region = region.unwrap_or(&spec.deploy.region);
    let variant = variant.unwrap_or(&state.build.variant);
    let built = manifest.variant(variant, Some(&state.build.variant))?;
    let publication = state
        .build
        .published
        .iter()
        .find(|(_, entry)| {
            entry.provider == "aliyun"
                && entry.region == region
                && entry.variant == variant
                && entry.build_id == built.shelter_build_id
                && entry.status == "available"
                && !entry.retiring
                && image_id.map_or(true, |id| entry.image_id.as_deref() == Some(id))
        })
        .map(|(key, entry)| (key.clone(), entry.clone()));
    let selected_image = image_id.map(str::to_owned)
        .or_else(|| publication.as_ref().and_then(|(_, entry)| entry.image_id.clone()))
        .context("no available publication; publish first or specify --image-id for an existing shared/marketplace image")?;
    anyhow::ensure!(
        !selected_image.is_empty()
            && selected_image
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "invalid cloud image ID"
    );
    let _lease = lock_image_operation(&cli.state_dir, region, &selected_image, true)?;
    let result = read_shelter_build_result(&built.build_result, &built.shelter_build_id)?;
    if let Some((_, published)) = &publication {
        anyhow::ensure!(
            sha256_file(&result.image_path)? == published.source_sha256,
            "published image no longer matches the local artifact"
        );
    }
    let mapping = paths.artifacts_dir.join("image-mapping.json");
    let runtime_metadata = paths.artifacts_dir.join("image-runtime.json");
    let mut contract = export_runtime_contract(&manifest, &built, variant)?;
    if result.rekor_value.is_some() {
        let root = result
            .image_path
            .parent()
            .context("artifact image has no parent directory")?;
        let uuids = report::local_rekor_entry_uuids(&[root.to_path_buf()]);
        anyhow::ensure!(
            !uuids.is_empty(),
            "Rekor build has no saved upload entry identifiers; rebuild before exporting"
        );
        contract.set_rekor_entry_uuids(uuids)?;
    }
    write_json_atomic(&runtime_metadata, &contract)?;
    write_json_atomic(
        &mapping,
        &serde_json::json!([{
            "provider": "terraform-alicloud",
            "region": region, "image_id": selected_image,
        }]),
    )?;
    // Record retention before producing a distributable release. Even if export
    // is interrupted, automatic pruning must not delete a possibly shared image.
    if let Some((publication_key, _)) = publication {
        publish::mark_exported(cli, &mut state, &publication_key)?;
    }
    run_shelter(
        cli,
        &mut [
            "--work-dir".into(),
            manifest.shelter_work_dir.into_os_string(),
            "image".into(),
            "release".into(),
            built.shelter_build_id.into(),
            "--mapping".into(),
            mapping.into_os_string(),
            "--signing-key".into(),
            signing_key.as_os_str().to_os_string(),
            "--output".into(),
            output.as_os_str().to_os_string(),
            "--runtime-metadata".into(),
            runtime_metadata.into_os_string(),
        ],
    )
}

fn export_runtime_contract(
    manifest: &BuildManifest,
    built: &BuildManifestVariant,
    variant: &str,
) -> Result<image_runtime::ImageRuntimeContract> {
    let prepared;
    let source = if let Some(directory) = &built.preparation_dir {
        prepared = read_build_manifest(&directory.join("manifest.json"))?;
        &prepared
    } else {
        manifest
    };
    anyhow::ensure!(
        source.service_id == manifest.service_id
            && source.shelter_build_id == built.shelter_build_id
            && source.build_result == built.build_result,
        "saved runtime contract belongs to a different build artifact"
    );
    let contract = source
        .runtime_contract
        .clone()
        .context("local build has no saved CA runtime contract; rebuild before exporting")?;
    contract.ensure_export_variant(variant)?;
    Ok(contract)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn fixture() -> (tempfile::TempDir, Cli, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let inspection = serde_json::json!({
            "schema_version": "io.shelter.cloud-image-inspection/v1",
            "manifest_digest": format!("sha256:{}", "a".repeat(64)),
            "image": {"provider": "terraform-alicloud", "account_id": "1234567890", "region": "cn-beijing", "image_id": "m-shared-image"},
            "reference_value": {"measurement.uki.SHA-384": ["b".repeat(96)], "measurement.uki.SHA-256": ["c".repeat(64)]},
            "rekor_value": null,
            "security": {"rootfs_verity": true},
            "runtime_metadata": {
                "schema_version": "confidential-agent/image-runtime/v1",
                "bootstrap_schema": BOOTSTRAP_SCHEMA_VERSION,
                "capabilities": ["external-fde-v1", "resources-v1", "mesh-client-ports-v1", "instance-ssh-v1"],
                "rootfs_integrity": true, "writable_layer_encryption": "luks2", "key_release": "ca-attested",
                "min_disk_gb": 80, "app_services": ["cai-allinone.service"],
                "guest_binary_sha256": "e".repeat(64)
            },
        });
        let inspected = root.path().join("inspection.json");
        fs::write(&inspected, serde_json::to_vec(&inspection).unwrap()).unwrap();
        let shelter = root.path().join("shelter");
        fs::write(
            &shelter,
            format!(
                "#!/bin/sh\nset -eu\n[ \"$1 $2\" = 'image inspect-release' ]\ncat '{}'\n",
                inspected.display()
            ),
        )
        .unwrap();
        set_mode(&shelter, 0o755).unwrap();
        fs::write(
            root.path().join("release.json"),
            "signed publisher metadata",
        )
        .unwrap();
        fs::write(root.path().join("publisher.pem"), "trusted public key").unwrap();
        let cli = Cli::parse_from([
            "confidential-agent",
            "--state-dir",
            root.path().join("state").to_str().unwrap(),
            "--shelter-bin",
            shelter.to_str().unwrap(),
            "status",
        ]);
        (root, cli, inspected)
    }

    fn instance_spec(root: &Path, id: &str) -> PathBuf {
        let path = root.join(format!("{id}.yaml"));
        fs::write(&path, format!("schema: confidential-agent/v1\nservice:\n  id: {id}\n  ports: []\n  app_service: cai-allinone.service\n  ssh: true\ndeploy:\n  provider: aliyun\n  instance_type: ecs.g9i.xlarge\n  region: cn-beijing\n  zone_id: cn-beijing-i\n  disk_gb: 80\n  image:\n    release: ./release.json\n    trusted_key: ./publisher.pem\n    manifest: sha256:{}\n    account_id: '1234567890'\nattestation:\n  tee: tdx\n  mode: challenge\n  reference_values: sample\nresources: {{}}\n", "a".repeat(64))).unwrap();
        path
    }

    #[test]
    fn omitted_manifest_is_resolved_and_pinned_for_apply() {
        let (root, cli, inspected) = fixture();
        let path = instance_spec(root.path(), "automatic-manifest");
        let content = fs::read_to_string(&path)
            .unwrap()
            .replace(&format!("    manifest: sha256:{}\n", "a".repeat(64)), "");
        fs::write(&path, content).unwrap();
        let snapshot = deployment_snapshot(&path).unwrap();
        let manifest = import(&cli, &snapshot.spec).unwrap();
        let pin = format!("sha256:{}", "a".repeat(64));
        assert_eq!(
            manifest.image.as_ref().unwrap().source.manifest.as_deref(),
            Some(pin.as_str())
        );
        let prepared = prepare(
            &context_paths(&cli.state_dir, "automatic-manifest"),
            &snapshot.spec,
            &manifest,
            &PrepareOptions::default(),
        )
        .unwrap();
        let args = deploy_shelter_args(&prepared);
        let position = args
            .iter()
            .position(|arg| arg == "--expect-manifest")
            .unwrap();
        assert_eq!(args[position + 1], OsString::from(&pin));
        let restored: BuildManifest =
            serde_json::from_slice(&serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert_eq!(
            restored.image.unwrap().source.manifest.as_deref(),
            Some(pin.as_str())
        );
        let mut invalid: serde_json::Value =
            serde_json::from_slice(&fs::read(&inspected).unwrap()).unwrap();
        invalid["manifest_digest"] = "invalid".into();
        fs::write(&inspected, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(import(&cli, &snapshot.spec).is_err());
    }

    #[test]
    fn deployment_flag_skips_only_signature_and_needs_no_key_file() {
        let (root, cli, inspected) = fixture();
        let path = instance_spec(root.path(), "unsigned-consumer");
        let content = fs::read_to_string(&path)
            .unwrap()
            .replace("    trusted_key: ./publisher.pem\n", "");
        fs::write(&path, content).unwrap();
        fs::remove_file(root.path().join("publisher.pem")).unwrap();
        assert!(deployment_snapshot(&path).is_err());
        let snapshot =
            deployment_snapshot_with_image_options(&path, true, Some("m-shared-image")).unwrap();
        let manifest = import(&cli, &snapshot.spec).unwrap();
        let image = manifest.image.as_ref().unwrap();
        assert!(image.source.skip_signature_verification);
        assert!(image.source.trusted_key.as_os_str().is_empty());
        let prepared = prepare(
            &context_paths(&cli.state_dir, "unsigned-consumer"),
            &snapshot.spec,
            &manifest,
            &PrepareOptions::default(),
        )
        .unwrap();
        let args = deploy_shelter_args(&prepared);
        assert!(args.iter().any(|a| a == "--skip-signature-verification"));
        assert!(!args.iter().any(|a| a == "--trusted-key"));
        let rendered: serde_yaml::Value =
            serde_yaml::from_slice(&fs::read(prepared.runtime_config()).unwrap()).unwrap();
        assert!(rendered.get("artifact").is_none());
        assert_eq!(
            rendered["targets"]["deploy"]["provider_options"]["image_id"],
            "m-shared-image"
        );
        let mut invalid: serde_json::Value =
            serde_json::from_slice(&fs::read(&inspected).unwrap()).unwrap();
        invalid["runtime_metadata"]["bootstrap_schema"] = "unknown".into();
        fs::write(&inspected, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(import(&cli, &snapshot.spec).is_err());
    }

    #[test]
    fn export_uses_requested_variants_saved_contract_and_checks_artifact_binding() {
        let (root, _, inspection) = fixture();
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(inspection).unwrap()).unwrap();
        let contract = |variant: &str| {
            let mut runtime = value["runtime_metadata"].clone();
            runtime["image_variant"] = variant.into();
            serde_json::from_value::<image_runtime::ImageRuntimeContract>(runtime).unwrap()
        };
        let directory = root.path().join("release");
        fs::create_dir(&directory).unwrap();
        let mut saved = BuildManifest {
            service_id: "inference".into(),
            shelter_build_id: "release-build".into(),
            build_result: directory.join("build-result.json"),
            preparation_dir: Some(directory.clone()),
            runtime_contract: Some(contract("release")),
            ..BuildManifest::default()
        };
        write_json_atomic(&directory.join("manifest.json"), &saved).unwrap();
        let built = saved.fallback_variant();
        let selected = BuildManifest {
            service_id: "inference".into(),
            shelter_build_id: "debug-build".into(),
            runtime_contract: Some(contract("debug")),
            ..BuildManifest::default()
        };
        let exported = export_runtime_contract(&selected, &built, "release").unwrap();
        assert_eq!(
            serde_json::to_value(exported).unwrap()["image_variant"],
            "release"
        );
        assert!(export_runtime_contract(&selected, &built, "debug").is_err());
        saved.shelter_build_id = "unrelated-build".into();
        write_json_atomic(&directory.join("manifest.json"), &saved).unwrap();
        assert!(export_runtime_contract(&selected, &built, "release").is_err());
        let mut legacy = selected.clone();
        legacy.build_result = root.path().join("debug-result.json");
        assert!(export_runtime_contract(&legacy, &legacy.fallback_variant(), "debug").is_ok());
        assert!(export_runtime_contract(&legacy, &legacy.fallback_variant(), "release").is_err());
    }

    #[test]
    fn published_debug_consumers_keep_variant_binding_without_copying_ssh_keys() {
        let (root, cli, inspection) = fixture();
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(&inspection).unwrap()).unwrap();
        metadata["runtime_metadata"]["image_variant"] = "debug".into();
        write_json_atomic(&inspection, &metadata).unwrap();
        for id in ["debug-a", "debug-b"] {
            let path = instance_spec(root.path(), id);
            let yaml = fs::read_to_string(&path)
                .unwrap()
                .replace("  ssh: true", "  ssh: false")
                .replace(
                    "  provider: aliyun",
                    "  image_variant: debug\n  provider: aliyun",
                );
            fs::write(&path, yaml).unwrap();
            commands::cmd_deploy(
                &cli,
                &DeployArgs {
                    skip_signature_verification: false,
                    image_id: None,
                    resume: false,
                    spec: path.clone(),
                    skip_inject: false,
                    render_only: true,
                    skip_peering_check: false,
                },
            )
            .unwrap();
            let spec = AgentSpec::from_path(&path).unwrap();
            let paths = context_paths(&cli.state_dir, id);
            let manifest = read_build_manifest(&paths.manifest).unwrap();
            assert!(manifest.ssh.is_none() && manifest.debug_ssh.is_none());
            assert!(!paths.secrets_dir.join("debug_ssh").exists());
            assert!(!paths.secrets_dir.join("ssh_client").exists());
            assert!(!paths.guest_staging_dir.exists());
            let prepared = prepare(
                &paths,
                &spec,
                &manifest,
                &PrepareOptions {
                    deploy_names: Some(DeployNames::new(&spec)),
                    ..Default::default()
                },
            )
            .unwrap();
            let args = commands::deploy_shelter_args(&prepared);
            assert!(args.iter().any(|arg| arg == "--release"));
            assert!(args.iter().any(|arg| arg == "--trusted-key"));
            assert!(args.iter().any(|arg| arg == "--expect-manifest"));
            assert!(!args
                .iter()
                .any(|arg| arg == "--skip-signature-verification"));
            let state = build_service_state(
                &cli.state_dir,
                &path,
                &spec,
                &DeployObservation::default(),
                &prepared,
                "built",
            )
            .unwrap();
            assert!(state.build.remote);
            assert_eq!(state.build.variant, "debug");
            assert!(state.build.debug_ssh.is_none());
            for variant in [Some("release"), None] {
                metadata["runtime_metadata"]
                    .as_object_mut()
                    .unwrap()
                    .remove("image_variant");
                if let Some(variant) = variant {
                    metadata["runtime_metadata"]["image_variant"] = variant.into();
                }
                write_json_atomic(&inspection, &metadata).unwrap();
                assert!(import(&cli, &spec)
                    .unwrap_err()
                    .to_string()
                    .contains("variant"));
            }
            metadata["runtime_metadata"]["image_variant"] = "debug".into();
            write_json_atomic(&inspection, &metadata).unwrap();
        }
    }

    #[test]
    fn two_instances_reuse_metadata_without_build_inputs_or_shared_secrets() {
        let (root, cli, _) = fixture();
        let mut states = Vec::new();
        let mut passphrases = Vec::new();
        for id in ["agent-a", "agent-b"] {
            let path = instance_spec(root.path(), id);
            commands::cmd_deploy(
                &cli,
                &DeployArgs {
                    skip_signature_verification: false,
                    image_id: None,
                    resume: false,
                    spec: path.clone(),
                    skip_inject: false,
                    render_only: true,
                    skip_peering_check: false,
                },
            )
            .unwrap();
            let spec = AgentSpec::from_path(&path).unwrap();
            let paths = context_paths(&cli.state_dir, id);
            let manifest = read_build_manifest(&paths.manifest).unwrap();
            assert!(manifest.ssh.is_some());
            let image = manifest.image.as_ref().unwrap();
            assert_eq!(image.image_id, "m-shared-image");
            assert!(image.source.release.starts_with(&paths.artifacts_dir));
            assert!(!paths.guest_staging_dir.exists());
            assert!(!paths.shelter_work_dir.join("records").exists());
            let runtime: serde_yaml::Value =
                serde_yaml::from_slice(&fs::read(&paths.rendered_config).unwrap()).unwrap();
            let target = &runtime["targets"]["deploy"];
            assert_eq!(target["attestation"], "disabled");
            assert_eq!(target["provider_options"]["cc_mode"], "tdx");
            assert!(target["provider_options"]["image"].get("source").is_none());
            let prepared = prepare(
                &paths,
                &spec,
                &manifest,
                &PrepareOptions {
                    deploy_names: Some(DeployNames::new(&spec)),
                    ..Default::default()
                },
            )
            .unwrap();
            let args = commands::deploy_shelter_args(&prepared);
            assert_eq!(args[2], "run");
            assert!(args.iter().any(|arg| arg == "--release"));
            assert!(args.iter().any(|arg| arg == "-d"));
            assert!(args.iter().any(|arg| arg == "-c"));
            assert!(!args.iter().any(|arg| arg == "build" || arg == "--config"));
            let state = build_service_state(
                &cli.state_dir,
                &path,
                &spec,
                &DeployObservation::default(),
                &prepared,
                "built",
            )
            .unwrap();
            assert!(state.build.remote);
            assert!(state.build.published.is_empty());
            assert_eq!(state.resources.len(), 2);
            assert!(state.resources["cai_ssh_host_key"].required);
            assert!(!state.resources["cai_ssh_host_key"].mutable);
            assert_eq!(
                state
                    .build
                    .sample_rv
                    .as_ref()
                    .map(|path| fs::read_to_string(path).unwrap())
                    .unwrap()
                    .contains(&"b".repeat(96)),
                true
            );
            passphrases.push(fs::read(ensure_disk_passphrase(&paths).unwrap()).unwrap());
            states.push(state);
        }
        assert_ne!(
            states[0].gateway_identity.as_ref().unwrap().public_key,
            states[1].gateway_identity.as_ref().unwrap().public_key
        );
        assert_ne!(passphrases[0], passphrases[1]);
        fs::remove_file(root.path().join("release.json")).unwrap();
        fs::remove_file(root.path().join("publisher.pem")).unwrap();
        for state in states {
            let paths = context_paths(&cli.state_dir, &state.service_id);
            let before = fs::read(&paths.rendered_config).unwrap();
            let source = fs::read_to_string(&state.spec.path).unwrap();
            fs::write(
                &state.spec.path,
                source
                    .replace("disk_gb: 80", "disk_gb: 200")
                    .replace("ecs.g9i.xlarge", "ecs.g9i.2xlarge"),
            )
            .unwrap();
            render_service_config_from_state(&cli.state_dir, &state, Vec::new()).unwrap();
            assert_eq!(fs::read(&paths.rendered_config).unwrap(), before);
            fs::remove_file(&state.spec.path).unwrap();
            render_service_config_from_state(&cli.state_dir, &state, Vec::new()).unwrap();
            assert_eq!(fs::read(&paths.rendered_config).unwrap(), before);
        }
    }

    #[test]
    fn invalid_image_metadata_is_rejected_before_deployment_state_or_resource_release() {
        for field in [
            "manifest",
            "region",
            "reference",
            "signature",
            "protocol",
            "disk",
            "capability",
            "protection",
            "missing-contract",
            "rekor-entry",
        ] {
            let (root, cli, inspected) = fixture();
            let path = instance_spec(root.path(), "agent-a");
            let mut value: serde_json::Value =
                serde_json::from_slice(&fs::read(&inspected).unwrap()).unwrap();
            match field {
                "manifest" => value["manifest_digest"] = "sha256:other".into(),
                "region" => value["image"]["region"] = "cn-shanghai".into(),
                "reference" => value["reference_value"] = serde_json::json!({}),
                "protocol" => {
                    value["runtime_metadata"]["bootstrap_schema"] =
                        "confidential-agent/bootstrap/v99".into()
                }
                "disk" => value["runtime_metadata"]["min_disk_gb"] = 120.into(),
                "capability" => value["runtime_metadata"]["capabilities"] = serde_json::json!([]),
                "protection" => value["security"]["rootfs_verity"] = false.into(),
                "missing-contract" => value["runtime_metadata"] = serde_json::Value::Null,
                "rekor-entry" => {
                    value["runtime_metadata"]["rekor_entry_uuids"] =
                        serde_json::json!(["../unrelated"])
                }
                _ => fs::write(
                    &cli.shelter_bin,
                    "#!/bin/sh\necho 'invalid signature' >&2\nexit 1\n",
                )
                .unwrap(),
            }
            fs::write(&inspected, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(
                commands::cmd_deploy(
                    &cli,
                    &DeployArgs {
                        skip_signature_verification: false,
                        image_id: None,
                        resume: false,
                        spec: path,
                        skip_inject: false,
                        render_only: true,
                        skip_peering_check: false
                    }
                )
                .is_err(),
                "accepted {field}"
            );
            let paths = context_paths(&cli.state_dir, "agent-a");
            assert!(!paths.service_state.exists());
            assert!(!paths.bootstrap_file.exists());
            assert!(!paths.shelter_work_dir.join("deploy").exists());
        }
    }

    #[test]
    fn resume_rejects_unknown_infrastructure_and_modified_inputs_before_import() {
        let (root, cli, _) = fixture();
        let path = instance_spec(root.path(), "agent-a");
        let args = DeployArgs {
            skip_signature_verification: false,
            image_id: None,
            spec: path.clone(),
            resume: true,
            skip_inject: false,
            render_only: false,
            skip_peering_check: false,
        };
        assert!(commands::cmd_deploy(&cli, &args)
            .unwrap_err()
            .to_string()
            .contains("existing deployed instance"));
        let mut state = crate::app::tests::local_state("agent-a", Vec::new(), Vec::new());
        state.phase = "deploying".into();
        write_local_service_state(&cli.state_dir, &state).unwrap();
        assert!(commands::cmd_deploy(&cli, &args)
            .unwrap_err()
            .to_string()
            .contains("confirmed deployed infrastructure"));
        state.phase = "deleted".into();
        write_local_service_state(&cli.state_dir, &state).unwrap();
        commands::cmd_deploy(
            &cli,
            &DeployArgs {
                skip_signature_verification: false,
                image_id: None,
                resume: false,
                render_only: true,
                ..args
            },
        )
        .unwrap();
        let paths = context_paths(&cli.state_dir, "agent-a");
        state.phase = "deployed".into();
        write_local_service_state(&cli.state_dir, &state).unwrap();
        let original_key = fs::read(paths.secrets_dir.join("ssh_client")).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        fs::write(&path, raw.replace("disk_gb: 80", "disk_gb: 200")).unwrap();
        assert!(commands::cmd_deploy(
            &cli,
            &DeployArgs {
                skip_signature_verification: false,
                image_id: None,
                spec: path,
                resume: true,
                skip_inject: false,
                render_only: false,
                skip_peering_check: false
            }
        )
        .unwrap_err()
        .to_string()
        .contains("original deployment configuration"));
        assert_eq!(
            fs::read(paths.secrets_dir.join("ssh_client")).unwrap(),
            original_key
        );
    }

    #[test]
    fn inject_uses_committed_authorization_after_project_and_secret_relocation() {
        let (root, cli, _) = fixture();
        let path = instance_spec(root.path(), "agent-a");
        let spec = AgentSpec::from_path(&path).unwrap();
        let paths = context_paths(&cli.state_dir, "agent-a");
        let mut manifest = import(&cli, &spec).unwrap();
        let mut saved = deployment_snapshot(&path).unwrap();
        freeze_deployment_inputs(&mut saved, &paths, "attempt").unwrap();
        manifest.deployment = Some(saved.clone());
        let mut state = crate::app::tests::local_state("agent-a", vec![], vec![]);
        state.build.image_name = spec.image_id().into();
        fs::remove_file(root.path().join("release.json")).unwrap();
        fs::remove_file(root.path().join("publisher.pem")).unwrap();
        let relocated = root.path().join("moved-project");
        fs::create_dir(&relocated).unwrap();
        let moved_spec = relocated.join("agent-a.yaml");
        fs::copy(&path, &moved_spec).unwrap();
        let moved_disk = relocated.join("disk-key");
        fs::copy(paths.secrets_dir.join("disk_passphrase"), &moved_disk).unwrap();
        let mut current = AgentSpec::from_path(&moved_spec).unwrap();
        current.secrets.disk_passphrase = Some(moved_disk.clone());
        // Removing a redundant version pin from an older project is safe:
        // authorization and release bytes are still bound to the deployment.
        current.deploy.image.as_mut().unwrap().manifest = None;
        let delivery = validated_injection_spec(&state, &manifest, &current, &paths).unwrap();
        assert_eq!(
            delivery.deploy.image.unwrap().release,
            manifest.image.as_ref().unwrap().source.release
        );
        assert_ne!(delivery.secrets.disk_passphrase.unwrap(), moved_disk);
        current.deploy.image.as_mut().unwrap().manifest =
            Some(format!("sha256:{}", "b".repeat(64)));
        assert!(
            validated_injection_spec(&state, &manifest, &current, &paths)
                .unwrap_err()
                .to_string()
                .contains("manifest")
        );
        current.deploy.image.as_mut().unwrap().manifest = None;
        // Resume remains strict; ordinary inject is an explicit resource update.
        assert!(
            validate_resume_inputs(&saved, &deployment_snapshot(&moved_spec).unwrap(), &paths)
                .is_err()
        );
        fs::write(&moved_disk, "different disk key").unwrap();
        assert!(
            validated_injection_spec(&state, &manifest, &current, &paths)
                .unwrap_err()
                .to_string()
                .contains("contents of committed input")
        );
        fs::copy(paths.secrets_dir.join("disk_passphrase"), &moved_disk).unwrap();
        fs::write(
            &current.deploy.image.as_ref().unwrap().trusted_key,
            "different publisher",
        )
        .unwrap();
        assert!(
            validated_injection_spec(&state, &manifest, &current, &paths)
                .unwrap_err()
                .to_string()
                .contains("publisher key")
        );
    }

    #[test]
    fn legacy_inputs_are_bound_only_after_historical_disk_and_identity_checks() {
        let (root, cli, _) = fixture();
        let path = instance_spec(root.path(), "agent-a");
        let mut saved = deployment_snapshot(&path).unwrap();
        let paths = context_paths(&cli.state_dir, "agent-a");
        let spec = instance_ssh::runtime_spec(&paths, &saved.spec).unwrap();
        let gateway = ensure_gateway_identity(&paths).unwrap();
        let disk = ensure_disk_passphrase(&paths).unwrap();
        let digest = sha256_file(&disk).unwrap();
        let mut state = crate::app::tests::local_state("agent-a", vec![], vec![]);
        state.gateway_identity = Some(gateway.clone());
        state.resources = resource_states(&spec).unwrap();
        state.phase = "active".into();
        let mut manifest = import(&cli, &saved.spec).unwrap();
        manifest.deployment = Some(saved.clone());
        write_json_atomic(&paths.manifest, &manifest).unwrap();
        write_local_service_state(&cli.state_dir, &state).unwrap();
        let original_manifest = fs::read(&paths.manifest).unwrap();
        let check = UpgradeArgs {
            command: UpgradeCommands::Check,
        };
        assert!(upgrade::run(&cli, &check)
            .unwrap_err()
            .to_string()
            .contains("before resource updates or cold boot"));
        assert!(upgrade::run(
            &cli,
            &UpgradeArgs {
                command: UpgradeCommands::BindInputs {
                    service: "agent-a".into()
                }
            }
        )
        .unwrap_err()
        .to_string()
        .contains("no historical disk digest"));
        assert_eq!(fs::read(&paths.manifest).unwrap(), original_manifest);
        assert!(upgrade::bind_legacy_inputs(&state, &mut saved, &paths, &"0".repeat(64)).is_err());
        assert!(saved.inputs.is_none());
        let original = fs::read(&gateway.private_key_path).unwrap();
        fs::write(&gateway.private_key_path, URL_SAFE_NO_PAD.encode([99; 32])).unwrap();
        assert!(upgrade::bind_legacy_inputs(&state, &mut saved, &paths, &digest).is_err());
        assert!(saved.inputs.is_none());
        fs::write(&gateway.private_key_path, original).unwrap();
        let authorized = paths.secrets_dir.join("ssh_client.pub");
        let original = fs::read(&authorized).unwrap();
        fs::write(&authorized, "replacement SSH identity").unwrap();
        assert!(upgrade::bind_legacy_inputs(&state, &mut saved, &paths, &digest).is_err());
        assert!(saved.inputs.is_none());
        fs::write(&authorized, original).unwrap();
        upgrade::bind_legacy_inputs(&state, &mut saved, &paths, &digest).unwrap();
        assert_eq!(
            saved.inputs.as_ref().unwrap()["secret:disk_passphrase"].sha256,
            digest
        );
        assert!(saved.inputs.as_ref().unwrap()["secret:disk_passphrase"]
            .snapshot
            .as_ref()
            .unwrap()
            .is_file());
        manifest.deployment = Some(saved);
        write_json_atomic(&paths.manifest, &manifest).unwrap();
        upgrade::run(&cli, &check).unwrap();
        manifest.deployment.as_mut().unwrap().inputs = Some(BTreeMap::new());
        write_json_atomic(&paths.manifest, &manifest).unwrap();
        assert!(upgrade::run(&cli, &check).is_err());
    }

    #[test]
    fn inject_rejects_instance_changes_before_either_provider_is_written() {
        for mode in ["challenge", "trustee"] {
            let (root, cli, _) = fixture();
            let path = instance_spec(root.path(), "agent-a");
            let raw = fs::read_to_string(&path)
                .unwrap()
                .replace("mode: challenge", &format!("mode: {mode}"));
            fs::write(&path, &raw).unwrap();
            let paths = context_paths(&cli.state_dir, "agent-a");
            let spec = AgentSpec::from_path(&path).unwrap();
            let mut manifest = import(&cli, &spec).unwrap();
            let mut snapshot = deployment_snapshot(&path).unwrap();
            freeze_deployment_inputs(&mut snapshot, &paths, "attempt").unwrap();
            manifest.deployment = Some(snapshot);
            write_json_atomic(&paths.manifest, &manifest).unwrap();
            let mut state = crate::app::tests::local_state("agent-a", vec![], vec![]);
            state.phase = "active".into();
            state.build.image_name = spec.image_id().into();
            state.spec = manifest.deployment.as_ref().unwrap().source.clone();
            state.attestation_mode = mode.into();
            write_local_service_state(&cli.state_dir, &state).unwrap();
            let original_state = fs::read(&paths.service_state).unwrap();
            let original_manifest = fs::read(&paths.manifest).unwrap();
            let trustee_state = cli.state_dir.join("trustee/state.json");
            let original_trustee = if mode == "trustee" {
                fs::create_dir_all(trustee_state.parent().unwrap()).unwrap();
                write_json_atomic(&trustee_state, &serde_json::json!({
                    "schema": "confidential-agent/trustee-state/v1",
                    "owner_id": "a".repeat(32), "revision": 1,
                    "services": {"agent-a": {"enabled": true,
                        "runtime_config_sha384": "b".repeat(96), "updated_at": "2026-10-14T00:00:00Z"}}
                })).unwrap();
                Some(fs::read(&trustee_state).unwrap())
            } else {
                None
            };
            let disk = paths.secrets_dir.join("disk_passphrase");
            let original_disk = fs::read(&disk).unwrap();
            let args = InjectArgs {
                spec: path.clone(),
                target_ip: state.deploy.public_ip.clone().unwrap(),
                skip_peering_check: true,
            };
            for (yaml, replacement_disk, expected) in [
                (
                    raw.clone(),
                    Some("replacement disk key"),
                    "secret:disk_passphrase",
                ),
                (
                    raw.replace(&"a".repeat(64), &"b".repeat(64)),
                    None,
                    "deployed image",
                ),
                (
                    raw.replace("disk_gb: 80", "disk_gb: 200"),
                    None,
                    "infrastructure configuration",
                ),
                (
                    raw.replace("region: cn-beijing", "region: cn-hangzhou"),
                    None,
                    "infrastructure configuration",
                ),
            ] {
                fs::write(&path, yaml).unwrap();
                fs::write(&disk, &original_disk).unwrap();
                if let Some(bytes) = replacement_disk {
                    fs::write(&disk, bytes).unwrap();
                }
                let error = commands::cmd_inject(&cli, &args).unwrap_err();
                assert!(error.to_string().contains(expected), "{mode}: {error:#}");
                if mode == "trustee" {
                    let error = trustee::cmd_trustee(
                        &cli,
                        &crate::cli::TrusteeArgs {
                            command: crate::cli::TrusteeCommands::Sync {
                                service: Some("agent-a".into()),
                            },
                        },
                    )
                    .unwrap_err();
                    assert!(
                        error.to_string().contains(expected),
                        "trustee sync: {error:#}"
                    );
                }
                assert_eq!(fs::read(&paths.service_state).unwrap(), original_state);
                assert_eq!(fs::read(&paths.manifest).unwrap(), original_manifest);
                // No challenge bootstrap or Trustee resource staging was reached.
                assert!(!paths.bootstrap_file.exists());
                match &original_trustee {
                    Some(original) => assert_eq!(fs::read(&trustee_state).unwrap(), *original),
                    None => assert!(!cli.state_dir.join("trustee").exists()),
                }
            }
            fs::write(&path, &raw).unwrap();
            fs::write(&disk, &original_disk).unwrap();
            let mut updated = spec.clone();
            let token = root.path().join("new-api-token");
            fs::write(&token, "ordinary application update").unwrap();
            updated.resources.insert(
                "token".into(),
                confidential_agent_core::spec::ResourceSpec {
                    source: token.clone(),
                    target: "/opt/data/token".into(),
                    owner: None,
                    group: None,
                    mode: None,
                    required: true,
                    mutable: false,
                },
            );
            let delivery = validated_injection_spec(&state, &manifest, &updated, &paths).unwrap();
            assert_eq!(delivery.resources["token"].source, token);
            let frozen = delivery.secrets.disk_passphrase.unwrap();
            fs::write(&disk, "changed after validation").unwrap();
            assert_eq!(fs::read(frozen).unwrap(), original_disk);
            // Immutable failures must not suggest inject as their remedy.
            let error = validate_resume_inputs(
                manifest.deployment.as_ref().unwrap(),
                &deployment_snapshot(&path).unwrap(),
                &paths,
            )
            .unwrap_err();
            assert!(error.to_string().contains("inject cannot rotate disk keys"));
        }
    }

    #[test]
    fn resume_binds_canonical_resource_paths_contents_and_instance_identities() {
        let (root, cli, _) = fixture();
        let path = instance_spec(root.path(), "agent-a");
        let source = root.path().join("runtime.json");
        fs::write(&source, "original runtime").unwrap();
        let alias = root.path().join("runtime-link.json");
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        let raw = fs::read_to_string(&path).unwrap().replace("resources: {}", "resources:\n  runtime:\n    source: ./runtime-link.json\n    target: /opt/data/allinone/runtime.json");
        fs::write(&path, &raw).unwrap();
        let paths = context_paths(&cli.state_dir, "agent-a");
        let mut saved = deployment_snapshot(&path).unwrap();
        freeze_deployment_inputs(&mut saved, &paths, "attempt-1").unwrap();
        // Persist and reload the exact contract used by a later process.
        let file = root.path().join("snapshot.json");
        write_json_atomic(&file, &saved).unwrap();
        let saved: DeploymentSnapshot = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
        let current = || deployment_snapshot(&path).unwrap();
        validate_resume_inputs(&saved, &current(), &paths).unwrap();
        let delivery = deployment_delivery_spec(&saved).unwrap();
        let frozen = &delivery.resources["runtime"].source;
        assert_ne!(frozen, &source);
        assert_eq!(
            fs::metadata(frozen).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::write(&source, "modified runtime").unwrap();
        assert!(validate_resume_inputs(&saved, &current(), &paths)
            .unwrap_err()
            .to_string()
            .contains("resource:runtime"));
        assert_eq!(fs::read(frozen).unwrap(), b"original runtime");
        fs::write(&source, "original runtime").unwrap();
        let other = root.path().join("other-runtime.json");
        fs::write(&other, "original runtime").unwrap();
        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&other, &alias).unwrap();
        assert!(validate_resume_inputs(&saved, &current(), &paths)
            .unwrap_err()
            .to_string()
            .contains("source path or contents"));
        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        let moved = root.path().join("moved");
        fs::create_dir(&moved).unwrap();
        let moved_spec = moved.join("agent-a.yaml");
        fs::write(&moved_spec, &raw).unwrap();
        assert!(
            validate_resume_inputs(&saved, &deployment_snapshot(&moved_spec).unwrap(), &paths)
                .unwrap_err()
                .to_string()
                .contains("canonical path")
        );
        let disk = paths.secrets_dir.join("disk_passphrase");
        let original_disk = fs::read(&disk).unwrap();
        fs::write(&disk, "replacement disk secret").unwrap();
        assert!(validate_resume_inputs(&saved, &current(), &paths)
            .unwrap_err()
            .to_string()
            .contains("secret:disk_passphrase"));
        fs::write(&disk, original_disk).unwrap();
        let client = paths.secrets_dir.join("ssh_client");
        let original_client = fs::read(&client).unwrap();
        fs::write(&client, "replacement client identity").unwrap();
        assert!(validate_resume_inputs(&saved, &current(), &paths)
            .unwrap_err()
            .to_string()
            .contains("identity:ssh_client"));
        fs::write(&client, original_client).unwrap();
        validate_resume_inputs(&saved, &current(), &paths).unwrap();
        fs::write(frozen, "tampered snapshot").unwrap();
        assert!(validate_resume_inputs(&saved, &current(), &paths)
            .unwrap_err()
            .to_string()
            .contains("saved deployment input"));
        let mut legacy = saved.clone();
        legacy.inputs = None;
        assert!(validate_resume_inputs(&legacy, &current(), &paths)
            .unwrap_err()
            .to_string()
            .contains("no frozen resource inputs"));
    }
}
