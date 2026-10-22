use super::*;
use std::net::ToSocketAddrs;

const REKOR_RV_SET_ATTEMPTS: usize = 2;
const REKOR_RV_SET_RETRY_DELAY: Duration = Duration::from_secs(10);

pub(super) fn inject_resources(
    cli: &Cli,
    state_dir: &Path,
    spec: &AgentSpec,
    build_result: &Path,
    build_id: &str,
    target_ip: &str,
    instance_ips: &[String],
    instance_public_ips: &[String],
) -> Result<()> {
    spec.ensure_mvp_supported()?;
    let tee = tee_name(spec.attestation.tee);
    let paths = context_paths(state_dir, &spec.service.id);
    ensure_private_context_dirs(&paths)?;
    let runtime_spec = instance_ssh::runtime_spec(&paths, spec)?;
    let spec = &runtime_spec;

    let artifacts = materialize_shelter_build_artifacts(&paths, build_result, build_id)?;
    prepare_challenge_reference_values(
        cli,
        state_dir,
        &spec.service.id,
        artifacts.sample_rv.as_ref(),
        artifacts.rekor_meta.as_ref(),
        reference_value_mode_name(spec.attestation.reference_values),
    )?;

    let mut bootstrap = render_bootstrap(&paths, spec)?;
    // Populate TNG gossip peers for multi-node OHTTP key sharing (serf
    // `peer_shared`).  Empty for single-instance deploys.
    bootstrap.tng_peers = instance_ips.to_vec();

    let guest_rv_source = guest_reference_value_source(
        &paths,
        artifacts.sample_rv.as_ref(),
        reference_value_mode_name(spec.attestation.reference_values),
    );
    if let Some(source) = &guest_rv_source {
        bootstrap.resources.push(GuestResource {
            id: REFERENCE_VALUE_LIST_RESOURCE_ID.to_string(),
            resource_path: resource_path(REFERENCE_VALUE_LIST_RESOURCE_ID),
            target: PathBuf::from(GUEST_REFERENCE_VALUE_LIST_PATH),
            owner: None,
            group: None,
            mode: "0644".to_string(),
            required: true,
            mutable: true,
            sha256: Some(sha256_file(source)?),
        });
    }

    if spec.a2a.as_ref().is_some_and(|a2a| a2a.enabled) {
        let rekor_path = artifacts.rekor_meta.as_ref().with_context(|| {
            format!(
                "service '{}' enables a2a but has no Rekor metadata; set attestation.reference_values=rekor and configure attestation.rekor",
                spec.service.id
            )
        })?;
        let content = fs::read_to_string(rekor_path)
            .with_context(|| format!("failed to read '{}'", rekor_path.display()))?;
        let meta: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse '{}'", rekor_path.display()))?;
        let sample_reference_values = match &artifacts.sample_rv {
            Some(path) => {
                let content = fs::read_to_string(path)
                    .with_context(|| format!("failed to read '{}'", path.display()))?;
                Some(
                    serde_json::from_str(&content)
                        .with_context(|| format!("failed to parse '{}'", path.display()))?,
                )
            }
            None => None,
        };
        let card = render_agent_card(spec, target_ip, &meta, sample_reference_values.as_ref())?;
        write_json_atomic(&paths.agent_card, &card)?;
        bootstrap.agent_card = Some(card);
    }

    write_bootstrap_file(&paths.bootstrap_file, &bootstrap)?;

    // Resolve disk passphrase (used for both disk and data passphrase resources).
    let disk_passphrase = match &spec.secrets.disk_passphrase {
        Some(path) => {
            if !path.exists() {
                bail!("disk passphrase file '{}' does not exist", path.display());
            }
            path.clone()
        }
        None => ensure_disk_passphrase(&paths)?,
    };

    // Build the ordered list of (resource_path, source_path) pairs to inject.
    // This is built once and delivered to every target instance.
    let mut injections: Vec<(String, PathBuf)> = Vec::new();
    injections.push((
        "default/local-resources/cagent_bootstrap_config".to_string(),
        paths.bootstrap_file.clone(),
    ));
    if let Some(source) = &guest_rv_source {
        injections.push((
            resource_path(REFERENCE_VALUE_LIST_RESOURCE_ID),
            source.clone(),
        ));
    }
    injections.push((
        "default/local-resources/disk_passphrase".to_string(),
        disk_passphrase.clone(),
    ));
    injections.push((
        "default/local-resources/data_passphrase".to_string(),
        disk_passphrase,
    ));
    for (id, resource) in &spec.resources {
        if !resource.source.exists() {
            bail!(
                "resource '{}' source '{}' does not exist",
                id,
                resource.source.display()
            );
        }
        injections.push((resource_path(id), resource.source.clone()));
    }

    // Determine target IPs: for multi-node, inject over the instances' PUBLIC
    // IPs so the deploy host can reach them without VPC routing; serf gossip
    // peers (tng_peers above) use the private IPs. Fall back to the single
    // target IP resolved from the deploy observation when no public IPs.
    let targets: Vec<&str> = if !instance_public_ips.is_empty() {
        instance_public_ips.iter().map(|s| s.as_str()).collect()
    } else if !instance_ips.is_empty() {
        instance_ips.iter().map(String::as_str).collect()
    } else {
        vec![target_ip]
    };

    if targets.len() > 1 {
        println!(
            "[ca] injecting attested resources to {} instances in parallel...",
            targets.len()
        );
        let injections = &injections;
        std::thread::scope(|s| {
            let mut handles = Vec::new();
            for &ip in &targets {
                handles.push(s.spawn(move || {
                    for (rp, src) in injections {
                        challenge_inject(cli, state_dir, ip, rp, src, tee)?;
                    }
                    Ok::<(), anyhow::Error>(())
                }));
            }
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Result<Vec<()>, anyhow::Error>>()
        })?;
    } else {
        for (rp, src) in &injections {
            challenge_inject(cli, state_dir, target_ip, rp, src, tee)?;
        }
    }

    println!("injected resources for service {}", spec.service.id);
    Ok(())
}

pub(super) fn guest_reference_value_source(
    paths: &ContextPaths,
    sample_rv: Option<&PathBuf>,
    mode: &str,
) -> Option<PathBuf> {
    match mode {
        "rekor" => {
            let path = paths.service_dir.join("rekor-rv-list.json");
            path.exists().then_some(path)
        }
        _ => sample_rv.filter(|path| path.exists()).cloned(),
    }
}

pub(super) fn write_bootstrap_file(path: &Path, bootstrap: &BootstrapConfig) -> Result<()> {
    let content = serde_json::to_vec_pretty(bootstrap)?;
    write_private_file(path, &content, 0o600)
        .with_context(|| format!("failed to write '{}'", path.display()))
}

pub(super) fn render_agent_card(
    spec: &AgentSpec,
    target_ip: &str,
    meta: &serde_json::Value,
    sample_reference_values: Option<&serde_json::Value>,
) -> Result<AgentCard> {
    confidential_agent_core::agent_card::render_agent_card(
        spec,
        target_ip,
        meta,
        sample_reference_values,
    )
}

pub(super) fn write_service_state(
    state_dir: &Path,
    spec_path: &Path,
    spec: &AgentSpec,
    observation: &DeployObservation,
    prepared: &PreparedConfig,
    phase: &str,
) -> Result<LocalServiceState> {
    with_state_dir_lock(state_dir, || {
        if phase == "deploying" {
            // Independent services can pass preflight together. Recheck after
            // acquiring the shared lock, before recording cloud ownership.
            let manifest =
                read_build_manifest(&context_paths(state_dir, &spec.service.id).manifest)?;
            validate_shared_guest_ports(state_dir, spec, &manifest)?;
        }
        let state = build_service_state(state_dir, spec_path, spec, observation, prepared, phase)?;
        write_local_service_state(state_dir, &state)?;
        Ok(state)
    })
}

pub(super) fn build_service_state(
    state_dir: &Path,
    spec_path: &Path,
    spec: &AgentSpec,
    observation: &DeployObservation,
    prepared: &PreparedConfig,
    phase: &str,
) -> Result<LocalServiceState> {
    let paths = context_paths(state_dir, &spec.service.id);
    let old_state = read_service_state_file(&paths.service_state).ok().flatten();
    let old_generation = old_state
        .as_ref()
        .map(|state| state.generation)
        .unwrap_or(0);
    let resources = resource_states(&instance_ssh::runtime_spec(&paths, spec)?)?;
    let names = prepared
        .deploy_names
        .clone()
        .context("deploy names are required when writing service state")?;
    let mut artifact_paths = context_paths(state_dir, &spec.service.id);
    if phase == "built" {
        artifact_paths.service_dir = prepared
            .rendered_config
            .parent()
            .context("build configuration has no preparation directory")?
            .to_path_buf();
    }
    let artifacts = materialize_shelter_build_artifacts(
        &artifact_paths,
        &prepared.build_result,
        &prepared.shelter_build_id,
    )?;
    let gateway_identity = ensure_gateway_identity(&paths)?;
    let deployment = if phase != "built" && paths.manifest.try_exists()? {
        read_build_manifest(&paths.manifest)?.deployment
    } else {
        None
    };
    let source = match deployment {
        Some(snapshot) => snapshot.source,
        None => LocalSpecState {
            path: absolute_path(spec_path)?,
            sha256: sha256_file(spec_path)?,
        },
    };
    Ok(LocalServiceState {
        schema: LOCAL_SERVICE_STATE_SCHEMA_VERSION.to_string(),
        service_id: spec.service.id.clone(),
        generation: old_generation + 1,
        phase: phase.to_string(),
        attestation_mode: attestation_mode_name(spec.attestation.mode).to_string(),
        spec: source,
        build: LocalBuildState {
            build_id: prepared.shelter_build_id.clone(),
            image_name: spec.image_id().to_string(),
            variant: spec.image_variant().to_string(),
            image_path: artifacts.image_path,
            images_dir: paths.artifacts_dir.clone(),
            cache_dir: paths.cache_dir.clone(),
            debug_ssh: prepared.debug_ssh.clone(),
            sample_rv: artifacts.sample_rv,
            rekor_meta: artifacts.rekor_meta,
            remote: prepared.image.is_some(),
            published: old_state
                .as_ref()
                .map(|state| state.build.published.clone())
                .unwrap_or_default(),
        },
        deploy: LocalDeployState {
            provider: "aliyun".to_string(),
            run_id: names.run_id,
            resource_name: names.resource_name,
            terraform_dir: prepared.terraform_dir.clone(),
            image_source: None,
            image_import_name: Some(names.image_import_name),
            bucket: None,
            instance_id: observation.instance_id.clone(),
            security_group_id: observation.security_group_id.clone(),
            private_ip: observation
                .private_ip
                .clone()
                .or_else(|| spec.deploy.private_ip.clone()),
            public_ip: observation.public_ip.clone(),
            tee: tee_name(spec.attestation.tee).to_string(),
            published_image_id: prepared.cloud_image_id.clone(),
            instance_ids: observation.instance_ids.clone(),
            instance_private_ips: observation.instance_private_ips.clone(),
            instance_public_ips: observation.instance_public_ips.clone(),
            slb_vip: observation.slb_vip.clone(),
        },
        service: LocalServiceNetwork {
            ports: spec.service.ports.clone(),
            connect: spec.service.connect.clone(),
            mcp_ports: spec.service.mcp_ports.clone(),
        },
        gateway_identity: Some(gateway_identity),
        resources,
        mesh_generation: 0,
        reference_values: reference_value_mode_name(spec.attestation.reference_values).to_string(),
    })
}

pub(super) fn activate_existing_service_state(
    state_dir: &Path,
    spec_path: &Path,
    spec: &AgentSpec,
    mut state: LocalServiceState,
) -> Result<LocalServiceState> {
    state.schema = LOCAL_SERVICE_STATE_SCHEMA_VERSION.to_string();
    state.generation += 1;
    state.phase = "activating".to_string();
    state.attestation_mode = attestation_mode_name(spec.attestation.mode).to_string();
    state.spec = LocalSpecState {
        path: absolute_path(spec_path)?,
        sha256: sha256_file(spec_path)?,
    };
    state.service = LocalServiceNetwork {
        ports: spec.service.ports.clone(),
        connect: spec.service.connect.clone(),
        mcp_ports: spec.service.mcp_ports.clone(),
    };
    let paths = context_paths(state_dir, &spec.service.id);
    state.resources = resource_states(&instance_ssh::runtime_spec(&paths, spec)?)?;
    state.reference_values =
        reference_value_mode_name(spec.attestation.reference_values).to_string();
    state.deploy.tee = tee_name(spec.attestation.tee).to_string();
    if state.gateway_identity.is_none() {
        let paths = context_paths(state_dir, &spec.service.id);
        state.gateway_identity = Some(ensure_gateway_identity(&paths)?);
    }
    Ok(state)
}

pub(super) fn write_local_service_state(state_dir: &Path, state: &LocalServiceState) -> Result<()> {
    let paths = context_paths(state_dir, &state.service_id);
    fs::create_dir_all(&paths.service_dir)
        .with_context(|| format!("failed to create '{}'", paths.service_dir.display()))?;
    write_json_atomic(&paths.service_state, state)?;
    Ok(())
}

pub(super) fn resource_states(spec: &AgentSpec) -> Result<BTreeMap<String, LocalResourceState>> {
    let mut resources = BTreeMap::new();
    for (id, resource) in &spec.resources {
        resources.insert(
            id.clone(),
            LocalResourceState {
                sha256: sha256_file(&resource.source)?,
                target: PathBuf::from(&resource.target),
                owner: resource.owner.clone(),
                group: resource.group.clone(),
                mode: resource.mode.clone().unwrap_or_else(|| "0600".to_string()),
                required: resource.required,
                mutable: resource.mutable,
            },
        );
    }
    Ok(resources)
}

pub(super) fn sync_mesh(cli: &Cli, state_dir: &Path, service_filter: Option<&str>) -> Result<()> {
    with_state_dir_lock(state_dir, || {
        let services = read_service_states(state_dir)?;
        sync_mesh_for_services(cli, state_dir, services, service_filter).map(|_| ())
    })
}

pub(super) fn sync_mesh_with_candidate(
    cli: &Cli,
    state_dir: &Path,
    candidate: LocalServiceState,
) -> Result<u64> {
    commit_mesh_candidate(state_dir, candidate, |services| {
        sync_mesh_for_services(cli, state_dir, services, None)
    })
}

fn commit_mesh_candidate(
    state_dir: &Path,
    mut candidate: LocalServiceState,
    sync: impl FnOnce(Vec<LocalServiceState>) -> Result<u64>,
) -> Result<u64> {
    with_state_dir_lock(state_dir, || {
        let mut services = read_service_states(state_dir)?;
        services.retain(|service| service.service_id != candidate.service_id);
        services.push(candidate.clone());
        services.sort_by(|left, right| left.service_id.cmp(&right.service_id));
        let generation = sync(services)?;
        // Publish the candidate before releasing the topology lock. The next
        // service must see every member of the bundle just delivered.
        candidate.mesh_generation = generation;
        write_local_service_state(state_dir, &candidate)?;
        Ok(generation)
    })
}

pub(super) fn active_peer_public_cidrs(
    service_id: &str,
    services: &[LocalServiceState],
) -> Result<Vec<String>> {
    let mut cidrs = BTreeSet::new();
    for service in services
        .iter()
        .filter(|service| service.service_id != service_id && service.is_mesh_member())
    {
        // A service's public_ip can be its load-balancer DNS name. Firewall
        // source rules describe the physical guests, never the application VIP.
        let addresses = if service.deploy.instance_public_ips.is_empty() {
            service
                .deploy
                .public_ip
                .as_deref()
                .into_iter()
                .collect::<Vec<_>>()
        } else {
            service
                .deploy
                .instance_public_ips
                .iter()
                .map(String::as_str)
                .collect()
        };
        anyhow::ensure!(
            !addresses.is_empty(),
            "active peer service '{}' has no instance public IPs for public mesh SG",
            service.service_id
        );
        for address in addresses {
            let ip = address.trim().parse::<Ipv4Addr>().with_context(|| format!(
                "active peer service '{}' requires recorded instance IPv4 addresses for public mesh SG, not its load-balancer address", service.service_id))?;
            cidrs.insert(format!("{ip}/32"));
        }
    }
    Ok(cidrs.into_iter().collect())
}

pub(super) fn refresh_active_shelter_deploys(
    cli: &Cli,
    state_dir: &Path,
    services: &[LocalServiceState],
) -> Result<()> {
    for service in services.iter().filter(|service| service.phase == "active") {
        let _operation = lock_service_operation(state_dir, &service.service_id)?;
        let Some(service) =
            read_service_state_file(&context_paths(state_dir, &service.service_id).service_state)?
        else {
            continue;
        };
        if service.phase != "active" {
            continue;
        }
        render_service_config_from_state(
            state_dir,
            &service,
            active_peer_public_cidrs(&service.service_id, &read_service_states(state_dir)?)?,
        )?;
        let paths = context_paths(state_dir, &service.service_id);
        let manifest = read_build_manifest(&paths.manifest)?;
        let prepared = prepared_from_state(state_dir, &service, &manifest)?;
        let mut args = vec![
            "--work-dir".into(),
            prepared.shelter_work_dir.as_os_str().to_os_string(),
            "network".into(),
            "update".into(),
            service.deploy.resource_name.clone().into(),
            "-c".into(),
            prepared.runtime_config().into_os_string(),
            "--target".into(),
            "deploy".into(),
        ];
        run_shelter(cli, &mut args)?;
    }
    Ok(())
}

pub(super) fn render_service_config_from_state(
    state_dir: &Path,
    state: &LocalServiceState,
    mesh_peer_cidrs: Vec<String>,
) -> Result<()> {
    let paths = context_paths(state_dir, &state.service_id);
    let mut manifest = read_build_manifest(&paths.manifest)?;
    let snapshot = committed_deployment(state, &manifest)?;
    let mut spec = snapshot.spec.clone();
    spec.service.ports = state.service.ports.clone();
    spec.service.connect = state.service.connect.clone();
    spec.service.mcp_ports = state.service.mcp_ports.clone();
    // Re-rendering an existing deployment must preserve the provider that was
    // committed to durable state. A later AppSpec edit may change desired
    // settings for a future deployment, but it must not remove Trustee
    // user-data from the currently managed instance.
    spec.attestation.mode = if trustee::service_uses_trustee(
        state_dir,
        &state.service_id,
        Some(&state.attestation_mode),
    )? {
        AttestationMode::Trustee
    } else {
        AttestationMode::Challenge
    };
    // Persist a verified legacy snapshot before any automatic reconciliation.
    if manifest.deployment.is_none() {
        manifest.deployment = Some(snapshot.clone());
        write_json_atomic(&paths.manifest, &manifest)?;
    }
    if manifest.image.is_some() {
        let deploy_user_data = snapshot.user_data.clone();
        images::prepare(
            &paths,
            &spec,
            &manifest,
            &PrepareOptions {
                deploy_names: Some(DeployNames {
                    run_id: state.deploy.run_id.clone(),
                    resource_name: state.deploy.resource_name.clone(),
                    image_import_name: state.deploy.image_import_name.clone().unwrap_or_default(),
                }),
                deploy_user_data,
                mesh_peer_cidrs,
                peerings: read_peerings_or_empty(state_dir)?,
                ..PrepareOptions::default()
            },
        )?;
        return Ok(());
    }
    let variant = manifest.variant(&state.build.variant, Some(&state.build.variant))?;
    if let Some(debug_ssh) = variant.debug_ssh.as_ref() {
        apply_debug_ssh_public_key(&mut spec, &debug_ssh.public_key)?;
    }
    let deploy_user_data = snapshot.user_data;
    spec.deploy.image_variant = Some(state.build.variant.clone());
    let rendered_config = shelter_runtime_config_path(&manifest.shelter_config);
    let rendered = render_runtime_config(
        &spec,
        &ShelterRenderOptions {
            build_id: Some(state.build.build_id.clone()),
            include_deploy: true,
            local_image_source: None,
            deploy_resource_name: Some(state.deploy.resource_name.clone()),
            local_image_import_name: state.deploy.image_import_name.clone(),
            cloud_image_id: state.deploy.published_image_id.clone(),
            deploy_user_data,
            mesh_peer_cidrs,
            peerings: read_peerings_or_empty(state_dir)?,
            deployer_cidr: std::env::var("CA_DEPLOYER_CIDR").ok(),
            ..ShelterRenderOptions::default()
        },
    )?;
    fs::write(&rendered_config, rendered)
        .with_context(|| format!("failed to write '{}'", rendered_config.display()))?;
    link_committed_config(
        &shelter_runtime_config_path(&paths.rendered_config),
        &rendered_config,
    )?;
    Ok(())
}

pub(super) fn apply_debug_ssh_public_key(spec: &mut AgentSpec, public_key: &Path) -> Result<()> {
    if !spec.deploys_debug_image() {
        return Ok(());
    }
    let Some(debug) = spec.build.variants.debug.as_mut() else {
        bail!("deploy.image_variant=debug requires build.variants.debug");
    };
    debug.ssh_public_key = Some(public_key.to_path_buf());
    Ok(())
}

pub(super) fn read_build_manifest(path: &Path) -> Result<BuildManifest> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read '{}'", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("failed to parse '{}'", path.display()))
}

pub(super) fn sync_mesh_for_services(
    cli: &Cli,
    state_dir: &Path,
    services: Vec<LocalServiceState>,
    service_filter: Option<&str>,
) -> Result<u64> {
    sync_mesh_for_services_with_delivery(
        state_dir,
        services,
        service_filter,
        |service, bundle_path| deliver_mesh_bundle(cli, state_dir, service, bundle_path),
    )
}

fn sync_mesh_for_services_with_delivery(
    state_dir: &Path,
    services: Vec<LocalServiceState>,
    service_filter: Option<&str>,
    mut deliver: impl FnMut(&LocalServiceState, &Path) -> Result<()>,
) -> Result<u64> {
    if let Some(filter) = service_filter {
        let service = services
            .iter()
            .find(|service| service.service_id == filter)
            .with_context(|| format!("service '{}' is not present in local state", filter))?;
        if !service.is_mesh_member() {
            bail!("service '{}' is not active in local state", filter);
        }
    }

    if !services.iter().any(|service| service.is_mesh_member()) {
        let bundle_path = state_dir.join("mesh-bundle.json");
        if bundle_path.exists() {
            fs::remove_file(&bundle_path)
                .with_context(|| format!("failed to remove '{}'", bundle_path.display()))?;
        }
        println!("no active services; mesh bundle removed");
        return Ok(0);
    }

    validate_guest_port_recipients(
        state_dir,
        services
            .iter()
            .filter(|service| service.is_mesh_member())
            .map(|service| {
                (
                    service.service_id.as_str(),
                    service.service.ports.as_slice(),
                    None,
                )
            })
            .collect(),
    )?;
    let reference_values = collect_reference_values(&services)?;
    let generation = next_mesh_generation(state_dir, &services);
    let mut bundle = render_mesh_bundle(&services, &reference_values, generation);
    resolve_mesh_mapping_hosts(&mut bundle, resolve_host_to_ipv4)?;
    let bundle_path = state_dir.join("mesh-bundle.json");
    write_json_atomic(&bundle_path, &bundle)?;

    let mut delivered = Vec::new();
    for service in services
        .iter()
        .filter(|service| service.is_mesh_member())
        .filter(|service| {
            service_filter
                .map(|filter| filter == service.service_id)
                .unwrap_or(true)
        })
    {
        deliver(service, &bundle_path)?;
        delivered.push(service.service_id.clone());
    }

    update_mesh_generation(state_dir, &delivered, generation)?;
    println!("synced mesh bundle to active services");
    Ok(generation)
}

fn deliver_mesh_bundle(
    cli: &Cli,
    state_dir: &Path,
    service: &LocalServiceState,
    bundle_path: &Path,
) -> Result<()> {
    if trustee::service_uses_trustee(
        state_dir,
        &service.service_id,
        Some(&service.attestation_mode),
    )? {
        // `sync_mesh` and `sync_mesh_with_candidate` hold the global
        // state lock across bundle generation and delivery.
        trustee::put_dynamic_resource_with_state_lock(
            state_dir,
            &service.service_id,
            "default/local-resources/cagent_mesh_bundle",
            bundle_path,
        )?;
    } else {
        let Some(target_ip) = service.deploy.preferred_injection_ip() else {
            bail!(
                "service '{}' has no IP for mesh injection",
                service.service_id
            );
        };
        prepare_challenge_reference_values(
            cli,
            state_dir,
            &service.service_id,
            service.build.sample_rv.as_ref(),
            service.build.rekor_meta.as_ref(),
            &service.reference_values,
        )?;
        let targets: Vec<&str> = if service.deploy.instance_public_ips.is_empty() {
            if service.deploy.instance_private_ips.is_empty() {
                vec![target_ip]
            } else {
                service
                    .deploy
                    .instance_private_ips
                    .iter()
                    .map(String::as_str)
                    .collect()
            }
        } else {
            service
                .deploy
                .instance_public_ips
                .iter()
                .map(String::as_str)
                .collect()
        };
        for ip in targets {
            challenge_inject(
                cli,
                state_dir,
                ip,
                "default/local-resources/cagent_mesh_bundle",
                bundle_path,
                &service.deploy.tee,
            )?;
        }
    }
    Ok(())
}

pub(super) fn next_mesh_generation(state_dir: &Path, services: &[LocalServiceState]) -> u64 {
    let state_generation = services
        .iter()
        .map(|service| service.mesh_generation)
        .max()
        .unwrap_or(0);
    let bundle_generation = fs::read_to_string(state_dir.join("mesh-bundle.json"))
        .ok()
        .and_then(|content| serde_json::from_str::<MeshBundle>(&content).ok())
        .map(|bundle| bundle.generation)
        .unwrap_or(0);
    state_generation.max(bundle_generation) + 1
}

pub(super) fn update_mesh_generation(
    state_dir: &Path,
    service_ids: &[String],
    generation: u64,
) -> Result<()> {
    for service_id in service_ids {
        let paths = context_paths(state_dir, service_id);
        if let Some(mut state) = read_service_state_file(&paths.service_state)? {
            state.mesh_generation = generation;
            write_local_service_state(state_dir, &state)?;
        }
    }
    Ok(())
}

/// Resolve a connect target host to a literal IPv4 address.
///
/// TNG's ingress `mapping.in/out.host` is deserialized as `Option<Ipv4Addr>`
/// (see TNG `config/mapping_rule.rs` `MappingDe`), so a DNS name (e.g. an ALB
/// `*.aliyuncsslb.com` address) is rejected with "data did not match any
/// variant of untagged enum MappingDe". Classic SLB/NLB expose an IP VIP, but
/// ALB exposes a DNS name, so resolve it here. TNG is expected to gain domain
/// mapping targets later; until then this bridges the gap.
///
/// If the host is already an IPv4 literal it is returned unchanged. If DNS
/// resolution fails or yields no IPv4, the original host is returned so TNG
/// surfaces the error rather than failing silently.
fn resolve_host_to_ipv4(host: &str) -> String {
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return host.to_string();
    }
    let resolved = (host, 0u16).to_socket_addrs().ok().and_then(|mut addrs| {
        addrs.find_map(|a| match a.ip() {
            std::net::IpAddr::V4(v4) => Some(v4.to_string()),
            _ => None,
        })
    });
    match resolved {
        Some(ip) => ip,
        None => host.to_string(),
    }
}

fn resolve_mesh_mapping_hosts(
    bundle: &mut MeshBundle,
    mut resolve: impl FnMut(&str) -> String,
) -> Result<()> {
    // TNG mapping targets are IPv4 literals. Keep the authoritative addresses
    // in local deployment state, and normalize only the bundle sent to guests.
    // Resolve every target before replacing the committed bundle or delivering
    // it, so a failed lookup cannot publish an unusable guest configuration.
    let mut targets = Vec::new();
    for (id, service) in &bundle.services {
        let public = service
            .public_ip
            .as_deref()
            .filter(|host| !host.trim().is_empty());
        let host = public.or_else(|| {
            service
                .private_ip
                .as_deref()
                .filter(|host| !host.trim().is_empty())
        });
        if let Some(host) = host {
            let address = match host.parse::<std::net::Ipv4Addr>() {
                Ok(address) => address,
                Err(_) => resolve(host)
                    .parse::<std::net::Ipv4Addr>()
                    .with_context(|| {
                        format!("mesh target for service '{id}' did not resolve to IPv4: {host}")
                    })?,
            };
            targets.push((id.clone(), public.is_some(), address.to_string()));
        }
    }
    for (id, public, address) in targets {
        let service = bundle.services.get_mut(&id).expect("recorded mesh service");
        if public {
            service.public_ip = Some(address);
        } else {
            service.private_ip = Some(address);
        }
    }
    Ok(())
}

pub(super) fn render_connect_config(
    state_dir: &Path,
    service_filter: Option<&str>,
) -> Result<serde_json::Value> {
    let states = read_service_states(state_dir)?;
    let services = connect_services(&states, service_filter)?;

    let bundle = read_mesh_bundle(state_dir)?;
    let mut used_local_ports = BTreeSet::new();
    let mut ingress = Vec::new();
    let mut client_endpoints = Vec::new();
    for service in services {
        let host = connect_host(service)?;
        // When an SLB VIP is set (multi-node replicas > 1), route ingress to the
        // SLB VIP instead of an individual instance public IP.
        let host = service
            .deploy
            .slb_vip
            .as_deref()
            .filter(|ip| !ip.trim().is_empty())
            .unwrap_or(host);
        // ALB addresses are DNS names; TNG mapping hosts must be IPv4 literals.
        let host = resolve_host_to_ipv4(&host);
        let reference_values = connect_reference_values(&bundle, &service.service_id)?;
        for cp in &service.service.connect {
            let remote_port = cp.port();
            let encryption = cp.encryption();
            let local_port = allocate_local_port(remote_port, |port| {
                used_local_ports.contains(&port) || !port_is_available(port)
            })?;
            used_local_ports.insert(local_port);
            eprintln!(
                "connect 127.0.0.1:{} -> {}:{} ({})",
                local_port, host, remote_port, service.service_id
            );
            eprintln!(
                "CONNECT_FORWARD host=127.0.0.1 port={} remote_host={} remote_port={} service={}",
                local_port, host, remote_port, service.service_id
            );
            client_endpoints.push(ConnectClientEndpoint {
                service: service.service_id.clone(),
                guest_port: remote_port,
                local_host: "127.0.0.1".to_string(),
                local_port,
                protocol: "http".to_string(),
                http_base_url: format!("http://127.0.0.1:{local_port}"),
            });
            let mut entry = serde_json::Map::new();
            match encryption {
                EncryptionMode::RatsTls => {
                    entry.insert("rats_tls".into(), serde_json::json!({"multiplex": true}));
                }
                EncryptionMode::Ohttp => {
                    entry.insert("ohttp".into(), serde_json::json!({}));
                }
            }
            entry.insert(
                "mapping".into(),
                serde_json::json!({
                    "in": {"host": "127.0.0.1", "port": local_port},
                    "out": {"host": host, "port": remote_port}
                }),
            );
            entry.insert(
                "verify".into(),
                serde_json::json!({
                    "model": "background_check",
                    "as_provider": "coco",
                    "as_type": "builtin",
                    "attestation_policy": connect_policy_config(state_dir)?,
                    "reference_values": reference_values,
                }),
            );
            ingress.push(serde_json::Value::Object(entry));
        }
    }

    Ok(serde_json::json!({
        "add_ingress": ingress,
        "client_endpoints": client_endpoints,
    }))
}

pub(super) fn render_agent_card_connect_config(
    state_dir: &Path,
    card: &AgentCard,
) -> Result<serde_json::Value> {
    render_agent_card_connect_config_with_port_checker(state_dir, card, |port| {
        !port_is_available(port)
    })
}

pub(super) fn render_agent_card_connect_config_with_port_checker(
    state_dir: &Path,
    card: &AgentCard,
    is_occupied: impl Fn(u16) -> bool,
) -> Result<serde_json::Value> {
    let ext = confidential_extension(card)?;
    let policy_path = host_default_policy_path(state_dir)?;
    let policy_path = policy_path.to_string_lossy();
    let mut used_local_ports = BTreeSet::new();
    let mut client_endpoints = Vec::new();
    let control_port = allocate_local_port(50000, &is_occupied)?;
    used_local_ports.insert(control_port);
    let mut config = derive_tng_client_config_with_local_ports(
        card,
        |remote_port| {
            let local_port = allocate_local_port(remote_port, |port| {
                used_local_ports.contains(&port) || is_occupied(port)
            })?;
            used_local_ports.insert(local_port);
            eprintln!(
                "connect 127.0.0.1:{} -> {}:{} ({})",
                local_port, ext.public_ip, remote_port, ext.id
            );
            eprintln!(
                "CONNECT_FORWARD host=127.0.0.1 port={} remote_host={} remote_port={} service={}",
                local_port, ext.public_ip, remote_port, ext.id
            );
            client_endpoints.push(ConnectClientEndpoint {
                service: ext.id.clone(),
                guest_port: remote_port,
                local_host: "127.0.0.1".to_string(),
                local_port,
                protocol: "http".to_string(),
                http_base_url: format!("http://127.0.0.1:{local_port}"),
            });
            Ok(local_port)
        },
        control_port,
        &policy_path,
    )?;
    if let serde_json::Value::Object(map) = &mut config {
        map.insert(
            "client_endpoints".to_string(),
            serde_json::to_value(client_endpoints)?,
        );
    }
    Ok(config)
}

pub(super) fn connect_services<'a>(
    states: &'a [LocalServiceState],
    service_filter: Option<&str>,
) -> Result<Vec<&'a LocalServiceState>> {
    if let Some(service_id) = service_filter {
        let state = states
            .iter()
            .find(|state| state.service_id == service_id)
            .with_context(|| format!("no local state for service '{service_id}'"))?;
        if state.phase != "active" {
            bail!(
                "service '{}' is {}; connect requires an active deployed service",
                state.service_id,
                state.phase
            );
        }
        if state.service.connect.is_empty() {
            bail!(
                "service '{}' does not expose any service.connect ports",
                state.service_id
            );
        }
        return Ok(vec![state]);
    }

    let services = states
        .iter()
        .filter(|state| state.phase == "active" && !state.service.connect.is_empty())
        .collect::<Vec<_>>();
    if services.is_empty() {
        bail!("no active services expose host connect ports");
    }
    Ok(services)
}

pub(super) fn connect_host(service: &LocalServiceState) -> Result<&str> {
    service
        .deploy
        .public_ip
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("service '{}' has no public_ip", service.service_id))
}

pub(super) fn connect_policy_config(state_dir: &Path) -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "type": "path",
        "path": host_default_policy_path(state_dir)?,
    }))
}

pub(super) fn connect_reference_values(
    bundle: &MeshBundle,
    service_id: &str,
) -> Result<serde_json::Value> {
    if let Some(sample) = bundle.reference_values.get(service_id) {
        return Ok(serde_json::json!([
            {
                "type": "sample",
                "payload": {
                    "type": "inline",
                    "content": sample,
                }
            }
        ]));
    }

    if let Some(rekor) = bundle.rekor_reference_values.get(service_id) {
        let content = rekor_payload(rekor)?;
        return Ok(serde_json::json!([
            {
                "type": "slsa",
                "payload": {
                    "type": "inline",
                    "content": content,
                }
            }
        ]));
    }

    bail!(
        "mesh bundle has no reference values for service '{}'; collect sample or Rekor RV before connecting",
        service_id
    )
}

pub(super) fn read_mesh_bundle(state_dir: &Path) -> Result<MeshBundle> {
    let bundle_path = state_dir.join("mesh-bundle.json");
    if !bundle_path.exists() {
        bail!("mesh bundle does not exist; deploy at least one active service first");
    }
    let content = fs::read_to_string(&bundle_path)
        .with_context(|| format!("failed to read '{}'", bundle_path.display()))?;
    serde_json::from_str(&content).context("failed to parse mesh bundle")
}

pub(super) fn read_service_states(state_dir: &Path) -> Result<Vec<LocalServiceState>> {
    let services_dir = state_dir.join("services");
    if !services_dir.exists() {
        return Ok(Vec::new());
    }

    let mut states = Vec::new();
    for entry in fs::read_dir(&services_dir)
        .with_context(|| format!("failed to read '{}'", services_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path().join("state.json");
        if !path.exists() {
            continue;
        }
        if let Some(state) = read_service_state_file(&path)? {
            states.push(state);
        }
    }
    states.sort_by(|left, right| left.service_id.cmp(&right.service_id));
    Ok(states)
}

pub(super) fn read_service_state_file(path: &Path) -> Result<Option<LocalServiceState>> {
    if !path.exists() {
        return Ok(None);
    }
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read '{}'", path.display()))?;
    let header: LocalStateHeader = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse schema header in '{}'", path.display()))?;
    if header.schema != LOCAL_SERVICE_STATE_SCHEMA_VERSION {
        bail!(
            "unsupported local service state schema '{}' in '{}'; expected '{}'",
            header.schema,
            path.display(),
            LOCAL_SERVICE_STATE_SCHEMA_VERSION
        );
    }
    let state: LocalServiceState = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse '{}'", path.display()))?;
    Ok(Some(state))
}

pub(super) fn validate_shared_guest_ports(
    state_dir: &Path,
    spec: &AgentSpec,
    incoming: &BuildManifest,
) -> Result<()> {
    let recipients = read_service_states(state_dir)?
        .into_iter()
        .filter(|state| {
            state.service_id != spec.service.id
                && matches!(
                    state.phase.as_str(),
                    "active" | "activating" | "deployed" | "deploying" | "destroying"
                )
        })
        .collect::<Vec<_>>();
    validate_guest_port_recipients(
        state_dir,
        recipients
            .iter()
            .map(|state| {
                (
                    state.service_id.as_str(),
                    state.service.ports.as_slice(),
                    None,
                )
            })
            .chain(std::iter::once((
                spec.service.id.as_str(),
                spec.service.ports.as_slice(),
                Some(incoming),
            )))
            .collect(),
    )
}

fn validate_guest_port_recipients(
    state_dir: &Path,
    recipients: Vec<(&str, &[u16], Option<&BuildManifest>)>,
) -> Result<()> {
    let mut ports = BTreeSet::new();
    let shared = recipients
        .iter()
        .map(|(_, ports, _)| *ports)
        .any(|service_ports| {
            service_ports
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .any(|port| !ports.insert(port))
        });
    if !shared {
        return Ok(());
    }
    // Clients with no service ports receive the same peer bundle and must also
    // understand independent local endpoints for duplicate remote ports.
    for (service_id, _, incoming) in recipients {
        let saved;
        let peer = match incoming {
            Some(manifest) => manifest,
            None => {
                saved = read_build_manifest(&context_paths(state_dir, service_id).manifest)?;
                &saved
            }
        };
        anyhow::ensure!(peer.runtime_contract.as_ref().is_some_and(|c| c.supports_shared_ports()),
            "service {service_id} uses a legacy guest without stable mesh client endpoints; rebuild every mesh recipient before deploying overlapping service ports");
    }
    Ok(())
}

pub(super) fn collect_reference_values(
    services: &[LocalServiceState],
) -> Result<ReferenceValueArtifacts> {
    collect_reference_values_from_state(services)
}

#[cfg(test)]
pub(super) fn collect_reference_values_from_dir(
    _root: &Path,
    services: &[LocalServiceState],
) -> Result<ReferenceValueArtifacts> {
    let mut sample = BTreeMap::new();
    let mut rekor = BTreeMap::new();
    for service in services {
        if !service.is_mesh_member() {
            continue;
        }
        let path = service.build.sample_rv.as_ref().with_context(|| {
            format!(
                "missing sample reference values for service '{}'",
                service.service_id
            )
        })?;
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read '{}'", path.display()))?;
        let parsed = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse '{}'", path.display()))?;
        sample.insert(service.service_id.clone(), parsed);

        match service.reference_values.as_str() {
            "sample" => {}
            "rekor" => {
                let path = service.build.rekor_meta.as_ref().with_context(|| {
                    format!(
                        "missing Rekor metadata for service '{}'",
                        service.service_id
                    )
                })?;
                let content = fs::read_to_string(path)
                    .with_context(|| format!("failed to read '{}'", path.display()))?;
                let parsed = serde_json::from_str(&content)
                    .with_context(|| format!("failed to parse '{}'", path.display()))?;
                rekor.insert(service.service_id.clone(), parsed);
            }
            other => bail!(
                "unsupported reference value mode '{}' for service '{}'",
                other,
                service.service_id
            ),
        }
    }
    Ok(ReferenceValueArtifacts { sample, rekor })
}

pub(super) fn collect_reference_values_from_state(
    services: &[LocalServiceState],
) -> Result<ReferenceValueArtifacts> {
    let mut sample = BTreeMap::new();
    let mut rekor = BTreeMap::new();
    for service in services {
        if !service.is_mesh_member() {
            continue;
        }
        let path = service.build.sample_rv.as_ref().with_context(|| {
            format!(
                "missing sample reference values for service '{}'",
                service.service_id
            )
        })?;
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read '{}'", path.display()))?;
        let parsed = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse '{}'", path.display()))?;
        sample.insert(service.service_id.clone(), parsed);

        match service.reference_values.as_str() {
            "sample" => {}
            "rekor" => {
                let path = service.build.rekor_meta.as_ref().with_context(|| {
                    format!(
                        "missing Rekor metadata for service '{}'",
                        service.service_id
                    )
                })?;
                let content = fs::read_to_string(path)
                    .with_context(|| format!("failed to read '{}'", path.display()))?;
                let parsed = serde_json::from_str(&content)
                    .with_context(|| format!("failed to parse '{}'", path.display()))?;
                rekor.insert(service.service_id.clone(), parsed);
            }
            other => bail!(
                "unsupported reference value mode '{}' for service '{}'",
                other,
                service.service_id
            ),
        }
    }
    Ok(ReferenceValueArtifacts { sample, rekor })
}

pub(super) fn latest_built_image(state_dir: &Path, spec: &AgentSpec) -> Result<PathBuf> {
    let paths = context_paths(state_dir, &spec.service.id);
    let build_id = read_service_state_file(&paths.service_state)?
        .map(|state| state.build.build_id)
        .unwrap_or_else(|| shelter_build_id(spec));
    let result_path = shelter_build_result_path(&paths.shelter_work_dir, &build_id);
    let result = read_shelter_build_result(&result_path, &build_id)?;
    Ok(result.image_path)
}

pub(super) fn prepare_challenge_reference_values(
    cli: &Cli,
    state_dir: &Path,
    service_id: &str,
    sample_rv: Option<&PathBuf>,
    rekor_meta: Option<&PathBuf>,
    mode: &str,
) -> Result<()> {
    match mode {
        "sample" => {
            let path = sample_rv.with_context(|| {
                format!(
                    "missing sample reference values for service '{}'",
                    service_id
                )
            })?;
            set_sample_reference_value(cli, state_dir, path)
        }
        "rekor" => {
            let path = rekor_meta
                .with_context(|| format!("missing Rekor metadata for service '{}'", service_id))?;
            let content = fs::read_to_string(path)
                .with_context(|| format!("failed to read '{}'", path.display()))?;
            let metadata: serde_json::Value = serde_json::from_str(&content)
                .with_context(|| format!("failed to parse '{}'", path.display()))?;
            let rv_list = rekor_payload(&metadata)?;
            let paths = context_paths(state_dir, service_id);
            fs::create_dir_all(&paths.service_dir)
                .with_context(|| format!("failed to create '{}'", paths.service_dir.display()))?;
            let rv_list_path = paths.service_dir.join("rekor-rv-list.json");
            write_json_atomic(&rv_list_path, &rv_list)?;
            match set_rekor_reference_value_list(cli, state_dir, &rv_list_path) {
                Ok(()) => Ok(()),
                Err(err) => {
                    let sample_path = sample_rv.with_context(|| {
                        format!(
                            "missing sample reference values for service '{}' after Rekor RV setup failed",
                            service_id
                        )
                    })?;
                    eprintln!(
                        "[ca] Rekor reference value list setup failed for service '{}': {err:#}; falling back to local sample reference values for challenge setup",
                        service_id
                    );
                    set_sample_reference_value(cli, state_dir, sample_path).with_context(|| {
                        format!(
                            "failed to fall back to sample reference values for service '{}'",
                            service_id
                        )
                    })
                }
            }
        }
        other => bail!(
            "unsupported reference value mode '{}' for service '{}'",
            other,
            service_id
        ),
    }
}

pub(super) fn set_sample_reference_value(
    cli: &Cli,
    state_dir: &Path,
    payload: &Path,
) -> Result<()> {
    run_attestation_client(
        cli,
        state_dir,
        vec![
            OsString::from("set-reference-value"),
            OsString::from("--provenance-type"),
            OsString::from("sample"),
            OsString::from("--payload"),
            payload.as_os_str().to_os_string(),
        ],
        inherited_proxy_envs(None),
        false,
    )
}

pub(super) fn set_rekor_reference_value_list(
    cli: &Cli,
    state_dir: &Path,
    rv_list: &Path,
) -> Result<()> {
    let mut last_error = None;
    for attempt in 1..=REKOR_RV_SET_ATTEMPTS {
        let result = run_attestation_client(
            cli,
            state_dir,
            vec![
                OsString::from("set-reference-value-list"),
                OsString::from("--rv-list"),
                rv_list.as_os_str().to_os_string(),
            ],
            inherited_proxy_envs(None),
            true,
        );
        match result {
            Ok(()) => return Ok(()),
            Err(err) => {
                if attempt == REKOR_RV_SET_ATTEMPTS {
                    return Err(err).with_context(|| {
                        format!(
                            "failed to set Rekor reference value list after {} attempts",
                            REKOR_RV_SET_ATTEMPTS
                        )
                    });
                }
                eprintln!(
                    "[ca] set Rekor reference value list failed on attempt {}/{}: {err:#}; retrying in {}s",
                    attempt,
                    REKOR_RV_SET_ATTEMPTS,
                    REKOR_RV_SET_RETRY_DELAY.as_secs()
                );
                last_error = Some(err);
                thread::sleep(REKOR_RV_SET_RETRY_DELAY);
            }
        }
    }
    Err(last_error.expect("at least one Rekor RV set attempt must run"))
}

pub(super) fn render_mesh_bundle(
    services: &[LocalServiceState],
    reference_values: &ReferenceValueArtifacts,
    generation: u64,
) -> MeshBundle {
    confidential_agent_core::mesh::render_mesh_bundle(
        services,
        &reference_values.sample,
        &reference_values.rekor,
        generation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mesh_mapping_hosts_resolve_balancers_without_changing_deployment_state() {
        let mut group = test_service_state("group", "active", vec![8080], vec![]);
        group.deploy.public_ip = Some("nlb.example.test".into());
        group.deploy.private_ip = None;
        let mut isolated = test_service_state("isolated", "active", vec![8080], vec![]);
        isolated.deploy.public_ip = Some("198.51.100.20".into());
        let mut bundle = render_mesh_bundle(
            &[group.clone(), isolated],
            &ReferenceValueArtifacts {
                sample: BTreeMap::new(),
                rekor: BTreeMap::new(),
            },
            7,
        );
        let mut lookups = Vec::new();
        resolve_mesh_mapping_hosts(&mut bundle, |host| {
            lookups.push(host.to_string());
            "198.51.100.10".into()
        })
        .unwrap();
        assert_eq!(lookups, ["nlb.example.test"]);
        assert_eq!(
            bundle.services["group"].public_ip.as_deref(),
            Some("198.51.100.10")
        );
        assert_eq!(
            bundle.services["isolated"].public_ip.as_deref(),
            Some("198.51.100.20")
        );
        assert_eq!(group.deploy.public_ip.as_deref(), Some("nlb.example.test"));
        assert_eq!(bundle.generation, 7);
    }

    #[test]
    fn mesh_mapping_hosts_reject_unresolved_targets_without_partial_updates() {
        let mut first = test_service_state("a", "active", vec![8080], vec![]);
        first.deploy.public_ip = Some("alb.example.test".into());
        let mut second = test_service_state("b", "active", vec![8080], vec![]);
        second.deploy.public_ip = Some("missing.example.test".into());
        let original = render_mesh_bundle(
            &[first, second],
            &ReferenceValueArtifacts {
                sample: BTreeMap::new(),
                rekor: BTreeMap::new(),
            },
            7,
        );
        for unresolved in ["missing.example.test", "::1"] {
            let mut bundle = original.clone();
            let error = resolve_mesh_mapping_hosts(&mut bundle, |host| {
                if host == "alb.example.test" {
                    "198.51.100.10".into()
                } else {
                    unresolved.into()
                }
            })
            .unwrap_err();
            assert!(error
                .to_string()
                .contains("service 'b' did not resolve to IPv4"));
            assert_eq!(bundle, original);
        }
    }

    fn shared_port_manifest() -> BuildManifest {
        BuildManifest {
            runtime_contract: Some(
                serde_json::from_value(json!({
                    "schema_version": "confidential-agent/image-runtime/v1",
                    "bootstrap_schema": BOOTSTRAP_SCHEMA_VERSION,
                    "capabilities": ["external-fde-v1", "resources-v1", "mesh-client-ports-v1", "mesh-client-ports-v2"],
                    "rootfs_integrity": true, "writable_layer_encryption": "luks2",
                    "key_release": "ca-attested", "min_disk_gb": 32, "app_services": [],
                    "guest_binary_sha256": "e".repeat(64)
                }))
                .unwrap(),
            ),
            ..BuildManifest::default()
        }
    }

    #[test]
    fn duplicate_mesh_ports_require_capable_client_only_recipients() {
        let root = tempfile::tempdir().unwrap();
        let capable = shared_port_manifest();
        for (id, ports, manifest) in [
            ("a", vec![18789], capable.clone()),
            ("c", vec![], BuildManifest::default()),
        ] {
            write_local_service_state(
                root.path(),
                &test_service_state(id, "active", ports, vec![]),
            )
            .unwrap();
            write_json_atomic(&context_paths(root.path(), id).manifest, &manifest).unwrap();
        }
        let yaml = "schema: confidential-agent/v1\nservice: {id: b, ports: [18789]}\nbuild: {image_name: app}\ndeploy: {provider: aliyun, region: cn-beijing, zone_id: cn-beijing-i, instance_type: ecs.g9i.xlarge}\nattestation: {tee: tdx, mode: challenge, reference_values: sample}\nresources: {}";
        let mut spec = AgentSpec::from_yaml(yaml, root.path()).unwrap();
        let error = validate_shared_guest_ports(root.path(), &spec, &capable).unwrap_err();
        assert!(error.to_string().contains("service c uses a legacy guest"));
        spec.service.ports = vec![18790];
        validate_shared_guest_ports(root.path(), &spec, &capable).unwrap();
        spec.service.ports = vec![18789];
        write_json_atomic(&context_paths(root.path(), "c").manifest, &capable).unwrap();
        validate_shared_guest_ports(root.path(), &spec, &capable).unwrap();
        write_local_service_state(
            root.path(),
            &test_service_state("b", "active", vec![18789], vec![]),
        )
        .unwrap();
        write_json_atomic(&context_paths(root.path(), "b").manifest, &capable).unwrap();
        spec.service.id = "d".into();
        spec.service.ports.clear();
        assert!(
            validate_shared_guest_ports(root.path(), &spec, &BuildManifest::default()).is_err()
        );
        validate_shared_guest_ports(root.path(), &spec, &capable).unwrap();
    }

    #[test]
    fn shared_ports_are_rechecked_when_recording_an_attempt_and_publishing_a_bundle() {
        let root = tempfile::tempdir().unwrap();
        let capable = shared_port_manifest();
        let legacy = test_service_state("c", "active", vec![], vec![]);
        write_local_service_state(root.path(), &legacy).unwrap();
        write_json_atomic(
            &context_paths(root.path(), "c").manifest,
            &BuildManifest::default(),
        )
        .unwrap();
        let yaml = "schema: confidential-agent/v1\nservice: {id: a, ports: [18789]}\nbuild: {image_name: app}\ndeploy: {provider: aliyun, region: cn-beijing, zone_id: cn-beijing-i, instance_type: ecs.g9i.xlarge}\nattestation: {tee: tdx, mode: challenge, reference_values: sample}\nresources: {}";
        let mut spec = AgentSpec::from_yaml(yaml, root.path()).unwrap();
        // Both independent invocations passed before either recorded ownership.
        validate_shared_guest_ports(root.path(), &spec, &capable).unwrap();
        spec.service.id = "b".into();
        validate_shared_guest_ports(root.path(), &spec, &capable).unwrap();
        write_local_service_state(
            root.path(),
            &test_service_state("a", "deploying", vec![18789], vec![]),
        )
        .unwrap();
        for id in ["a", "b"] {
            write_json_atomic(&context_paths(root.path(), id).manifest, &capable).unwrap();
        }
        let prepared = PreparedConfig {
            image: None,
            rendered_config: root.path().join("unused.yml"),
            shelter_build_id: "unused".into(),
            shelter_work_dir: root.path().join("unused"),
            build_result: root.path().join("unused.json"),
            deploy_result: root.path().join("unused-deploy.json"),
            deploy_names: None,
            terraform_dir: None,
            debug_ssh: None,
            cloud_image_id: None,
        };
        let error = write_service_state(
            root.path(),
            root.path(),
            &spec,
            &DeployObservation::default(),
            &prepared,
            "deploying",
        )
        .unwrap_err();
        assert!(error.to_string().contains("service c uses a legacy guest"));
        assert!(!context_paths(root.path(), "b").service_state.exists());

        // Also reject an incompatible already-recorded topology before replacing
        // the committed bundle or delivering it, including filtered mesh sync.
        for id in ["a", "b"] {
            write_local_service_state(
                root.path(),
                &test_service_state(id, "active", vec![18789], vec![]),
            )
            .unwrap();
        }
        let bundle = root.path().join("mesh-bundle.json");
        let original = serde_json::to_vec(&render_mesh_bundle(
            &[legacy],
            &ReferenceValueArtifacts {
                sample: BTreeMap::new(),
                rekor: BTreeMap::new(),
            },
            7,
        ))
        .unwrap();
        fs::write(&bundle, &original).unwrap();
        let cli = Cli {
            command: Commands::Version,
            shelter_bin: "shelter".into(),
            state_dir: root.path().into(),
            tools_dir: PathBuf::from("/usr/libexec/cagent/bin"),
        };
        let error = sync_mesh(&cli, root.path(), Some("a")).unwrap_err();
        assert!(error.to_string().contains("service c uses a legacy guest"));
        assert_eq!(fs::read(bundle).unwrap(), original);
        assert!(read_service_states(root.path())
            .unwrap()
            .iter()
            .all(|state| state.mesh_generation == 1));
    }

    #[test]
    fn concurrent_candidates_commit_state_before_the_next_topology() {
        use std::sync::{Arc, Barrier, Mutex};
        let root = tempfile::tempdir().unwrap();
        let rv = root.path().join("rv.json");
        fs::write(&rv, "{}").unwrap();
        let mut candidates = Vec::new();
        for id in ["a", "b"] {
            let mut state = test_service_state(id, "deployed", vec![18789], vec![]);
            state.build.sample_rv = Some(rv.clone());
            write_local_service_state(root.path(), &state).unwrap();
            write_json_atomic(
                &context_paths(root.path(), id).manifest,
                &shared_port_manifest(),
            )
            .unwrap();
            state.phase = "active".into();
            state.generation += 1;
            candidates.push(state);
        }
        let deliveries = Arc::new(Mutex::new(BTreeMap::<String, MeshBundle>::new()));
        let returned = Arc::new(Barrier::new(2));
        let resume_caller = Arc::new(Barrier::new(2));
        let a = candidates.remove(0);
        let directory = root.path().to_path_buf();
        let received = deliveries.clone();
        let arrived = returned.clone();
        let resume = resume_caller.clone();
        let worker = thread::spawn(move || {
            let _operation = lock_service_operation(&directory, "a").unwrap();
            commit_mesh_candidate(&directory, a, |services| {
                sync_mesh_for_services_with_delivery(&directory, services, None, |service, path| {
                    received.lock().unwrap().insert(
                        service.service_id.clone(),
                        serde_json::from_slice(&fs::read(path)?).unwrap(),
                    );
                    Ok(())
                })
            })
            .unwrap();
            // Deterministically pause at the former gap between mesh return
            // and the caller's active-state write while B completes activation.
            arrived.wait();
            resume.wait();
        });
        returned.wait();
        assert_eq!(read_service_states(root.path()).unwrap()[0].phase, "active");
        let _operation = lock_service_operation(root.path(), "b").unwrap();
        let generation = commit_mesh_candidate(root.path(), candidates.remove(0), |services| {
            sync_mesh_for_services_with_delivery(root.path(), services, None, |service, path| {
                deliveries.lock().unwrap().insert(
                    service.service_id.clone(),
                    serde_json::from_slice(&fs::read(path)?).unwrap(),
                );
                Ok(())
            })
        })
        .unwrap();
        resume_caller.wait();
        worker.join().unwrap();
        let committed: MeshBundle =
            serde_json::from_slice(&fs::read(root.path().join("mesh-bundle.json")).unwrap())
                .unwrap();
        let states = read_service_states(root.path()).unwrap();
        assert!(states
            .iter()
            .all(|state| state.phase == "active" && state.mesh_generation == generation));
        assert_eq!(committed.services.len(), 2);
        for id in ["a", "b"] {
            assert_eq!(
                serde_json::to_value(&deliveries.lock().unwrap()[id]).unwrap(),
                serde_json::to_value(&committed).unwrap()
            );
        }
    }

    fn test_service_state(
        id: &str,
        phase: &str,
        ports: Vec<u16>,
        connect: Vec<ConnectPort>,
    ) -> LocalServiceState {
        LocalServiceState {
            schema: LOCAL_SERVICE_STATE_SCHEMA_VERSION.to_string(),
            service_id: id.to_string(),
            generation: 1,
            phase: phase.to_string(),
            attestation_mode: "challenge".to_string(),
            spec: LocalSpecState {
                path: PathBuf::from("/project/agent.yaml"),
                sha256: "spec-hash".to_string(),
            },
            build: LocalBuildState {
                build_id: "build-1".to_string(),
                image_name: format!("{id}-agent"),
                variant: "release".to_string(),
                debug_ssh: None,
                sample_rv: Some(PathBuf::from("/state/rv.json")),
                rekor_meta: None,
                remote: false,
                published: BTreeMap::new(),
                image_path: PathBuf::from("/state/image.qcow2"),
                images_dir: PathBuf::from("/state/images"),
                cache_dir: PathBuf::from("/state/cache"),
            },
            deploy: LocalDeployState {
                provider: "aliyun".to_string(),
                run_id: "run-1".to_string(),
                resource_name: format!("{id}-resource"),
                instance_id: Some("i-test".to_string()),
                security_group_id: None,
                public_ip: Some("1.2.3.4".to_string()),
                private_ip: Some("10.0.0.1".to_string()),
                terraform_dir: Some(PathBuf::from("/state/terraform")),
                image_source: None,
                image_import_name: None,
                bucket: None,
                tee: "tdx".to_string(),
                published_image_id: None,
                instance_ids: Vec::new(),
                instance_private_ips: Vec::new(),
                instance_public_ips: Vec::new(),
                slb_vip: None,
            },
            service: LocalServiceNetwork {
                ports: ports.clone(),
                connect: connect.clone(),
                mcp_ports: Vec::new(),
            },
            gateway_identity: Some(LocalGatewayIdentity {
                public_key: "pub".to_string(),
                private_key_path: PathBuf::from(format!(
                    "/state/services/{id}/secrets/gateway_identity.seed"
                )),
            }),
            resources: BTreeMap::new(),
            mesh_generation: 1,
            reference_values: "sample".to_string(),
        }
    }

    #[test]
    fn connect_services_filters_by_id() {
        let states = vec![
            test_service_state(
                "alpha",
                "active",
                vec![8080],
                vec![(confidential_agent_core::spec::ConnectPort::Plain(8080))],
            ),
            test_service_state(
                "beta",
                "active",
                vec![9090],
                vec![(confidential_agent_core::spec::ConnectPort::Plain(9090))],
            ),
        ];
        let result = connect_services(&states, Some("alpha")).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].service_id, "alpha");
    }

    #[test]
    fn connect_services_rejects_inactive() {
        let states = vec![test_service_state(
            "alpha",
            "deployed",
            vec![8080],
            vec![confidential_agent_core::spec::ConnectPort::Plain(8080)],
        )];
        assert!(connect_services(&states, Some("alpha")).is_err());
    }

    #[test]
    fn connect_services_rejects_no_connect_ports() {
        let states = vec![test_service_state("alpha", "active", vec![8080], vec![])];
        assert!(connect_services(&states, Some("alpha")).is_err());
    }

    #[test]
    fn connect_services_all_active_when_no_filter() {
        let alpha = test_service_state(
            "alpha",
            "active",
            vec![8080],
            vec![confidential_agent_core::spec::ConnectPort::Plain(8080)],
        );
        let beta = test_service_state(
            "beta",
            "deployed",
            vec![9090],
            vec![confidential_agent_core::spec::ConnectPort::Plain(9090)],
        );
        let gamma = test_service_state(
            "gamma",
            "active",
            vec![7070],
            vec![confidential_agent_core::spec::ConnectPort::Plain(7070)],
        );
        let states = vec![alpha, beta, gamma];
        let result = connect_services(&states, None).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn connect_services_errors_when_none_available() {
        let states = vec![test_service_state("alpha", "deployed", vec![8080], vec![])];
        assert!(connect_services(&states, None).is_err());
    }

    #[test]
    fn connect_host_extracts_ip() {
        let state = test_service_state("alpha", "active", vec![], vec![]);
        assert_eq!(connect_host(&state).unwrap(), "1.2.3.4");
    }

    #[test]
    fn connect_host_errors_when_missing() {
        let mut state = test_service_state("alpha", "active", vec![], vec![]);
        state.deploy.public_ip = None;
        assert!(connect_host(&state).is_err());
    }

    #[test]
    fn connect_host_errors_when_empty() {
        let mut state = test_service_state("alpha", "active", vec![], vec![]);
        state.deploy.public_ip = Some("  ".to_string());
        assert!(connect_host(&state).is_err());
    }

    #[test]
    fn connect_policy_config_structure() {
        let config = connect_policy_config(Path::new("/state")).unwrap();
        assert_eq!(config["type"], "path");
        assert_eq!(
            config["path"],
            "/state/attestation/token/ear/policies/opa/default.rego"
        );
    }

    #[test]
    fn connect_reference_values_from_sample() {
        let bundle: MeshBundle = serde_json::from_value(json!({
            "schema": "confidential-agent/mesh-bundle/v1",
            "generation": 1,
            "updated_at": 0,
            "services": {},
            "reference_values": {
                "openclaw": {"tdx": {"mr_td": "abc123"}}
            },
            "rekor_reference_values": {}
        }))
        .unwrap();
        let rv = connect_reference_values(&bundle, "openclaw").unwrap();
        let arr = rv.as_array().unwrap();
        assert_eq!(arr[0]["type"], "sample");
    }

    #[test]
    fn connect_reference_values_from_rekor() {
        let bundle: MeshBundle = serde_json::from_value(json!({
            "schema": "confidential-agent/mesh-bundle/v1",
            "generation": 1,
            "updated_at": 0,
            "services": {},
            "reference_values": {},
            "rekor_reference_values": {
                "openclaw": {
                    "artifact_id": "openclaw-agent",
                    "artifact_version": "v0.1.0",
                    "artifact_type": "uki",
                    "rekor_url": "https://rekor.example.com"
                }
            }
        }))
        .unwrap();
        let rv = connect_reference_values(&bundle, "openclaw").unwrap();
        let arr = rv.as_array().unwrap();
        assert_eq!(arr[0]["type"], "slsa");
    }

    #[test]
    fn connect_reference_values_missing_errors() {
        let bundle: MeshBundle = serde_json::from_value(json!({
            "schema": "confidential-agent/mesh-bundle/v1",
            "generation": 1,
            "updated_at": 0,
            "services": {},
            "reference_values": {},
            "rekor_reference_values": {}
        }))
        .unwrap();
        assert!(connect_reference_values(&bundle, "nonexistent").is_err());
    }

    #[test]
    fn active_peer_public_cidrs_excludes_self() {
        let alpha = test_service_state("alpha", "active", vec![], vec![]);
        let mut beta = test_service_state("beta", "active", vec![], vec![]);
        beta.deploy.public_ip = Some("5.6.7.8".to_string());
        let states = vec![alpha, beta];
        let cidrs = active_peer_public_cidrs("alpha", &states).unwrap();
        assert_eq!(cidrs, vec!["5.6.7.8/32"]);
    }

    #[test]
    fn peer_firewalls_use_all_replica_sources_instead_of_the_service_vip() {
        for vip in ["nlb.example.invalid", "198.51.100.200"] {
            let mut beta = test_service_state("beta", "active", vec![], vec![]);
            beta.deploy.public_ip = Some(vip.into());
            beta.deploy.instance_public_ips = vec![
                "192.0.2.11".into(),
                "192.0.2.12".into(),
                "192.0.2.11".into(),
            ];
            assert_eq!(
                active_peer_public_cidrs("alpha", &[beta]).unwrap(),
                vec!["192.0.2.11/32", "192.0.2.12/32"]
            );
        }
        let mut legacy = test_service_state("beta", "active", vec![], vec![]);
        legacy.deploy.public_ip = Some("nlb.example.invalid".into());
        assert!(active_peer_public_cidrs("alpha", &[legacy])
            .unwrap_err()
            .to_string()
            .contains("instance IPv4"));
    }

    #[test]
    fn active_peer_public_cidrs_skips_inactive() {
        let states = vec![
            test_service_state("alpha", "active", vec![], vec![]),
            test_service_state("beta", "deleted", vec![], vec![]),
        ];
        let cidrs = active_peer_public_cidrs("alpha", &states).unwrap();
        assert!(cidrs.is_empty());
    }

    #[test]
    fn read_service_states_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let svc_dir = dir.path().join("services").join("test-svc");
        fs::create_dir_all(&svc_dir).unwrap();
        let state = test_service_state("test-svc", "active", vec![8080], vec![]);
        fs::write(
            svc_dir.join("state.json"),
            serde_json::to_string(&state).unwrap(),
        )
        .unwrap();
        let states = read_service_states(dir.path()).unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].service_id, "test-svc");
    }

    #[test]
    fn read_service_states_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let states = read_service_states(dir.path()).unwrap();
        assert!(states.is_empty());
    }

    #[test]
    fn read_service_state_file_returns_none_for_missing() {
        let result = read_service_state_file(Path::new("/nonexistent/state.json")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn render_agent_card_produces_valid_card() {
        let spec_yaml = r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  image_name: openclaw-agent
  variants:
    release:
      enabled: true
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
attestation:
  tee: tdx
  mode: challenge
  reference_values: rekor
  rekor:
    artifact_id: openclaw-agent
    artifact_type: uki
    slsa_generator: ./slsa-gen
a2a:
  enabled: true
  id: openclaw-agent
  name: OpenClaw Alpha
resources: {}
"#;
        let spec = AgentSpec::from_yaml(spec_yaml, Path::new("/project")).unwrap();
        let meta = json!({
            "rekor_url": "https://rekor.example.com",
            "artifact_id": "openclaw-agent",
            "artifact_type": "uki",
            "artifact_version": "v0.1.0",
            "rv_name": "openclaw"
        });
        let card = render_agent_card(&spec, "1.2.3.4", &meta, None).unwrap();
        assert_eq!(card.name, "OpenClaw Alpha");
        let ext = confidential_agent_core::agent_card::confidential_extension(&card).unwrap();
        assert_eq!(ext.id, "openclaw-agent");
        assert_eq!(ext.public_ip, "1.2.3.4");
        assert_eq!(ext.ports.len(), 1);
        assert_eq!(ext.ports[0].port, 18789);
        assert_eq!(ext.tee, "tdx");
    }

    #[test]
    fn render_agent_card_requires_a2a_enabled() {
        let spec_yaml = r#"
schema: confidential-agent/v1
service:
  id: test
  ports: [8080]
  connect: [8080]
build:
  image_name: test
  variants:
    release:
      enabled: true
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#;
        let spec = AgentSpec::from_yaml(spec_yaml, Path::new("/project")).unwrap();
        assert!(render_agent_card(&spec, "1.2.3.4", &json!({}), None).is_err());
    }

    #[test]
    fn render_agent_card_requires_connect_ports() {
        let spec_yaml = r#"
schema: confidential-agent/v1
service:
  id: test
  ports: [8080]
build:
  image_name: test
  variants:
    release:
      enabled: true
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
a2a:
  enabled: true
  id: test-agent
  name: Test
resources: {}
"#;
        let spec = AgentSpec::from_yaml(spec_yaml, Path::new("/project")).unwrap();
        let meta = json!({
            "rekor_url": "https://r.example.com",
            "artifact_id": "t",
            "artifact_type": "uki",
            "artifact_version": "v1",
            "rv_name": "t"
        });
        assert!(render_agent_card(&spec, "1.2.3.4", &meta, None).is_err());
    }

    #[test]
    fn render_mesh_bundle_includes_only_active_services() {
        let states = vec![
            test_service_state(
                "alpha",
                "active",
                vec![8080],
                vec![(confidential_agent_core::spec::ConnectPort::Plain(8080))],
            ),
            test_service_state("beta", "deleted", vec![9090], vec![]),
        ];
        let rv = ReferenceValueArtifacts {
            sample: BTreeMap::from([
                ("alpha".to_string(), json!({"tdx": "test"})),
                ("beta".to_string(), json!({"tdx": "stale"})),
            ]),
            rekor: BTreeMap::new(),
        };
        let bundle = render_mesh_bundle(&states, &rv, 1);
        assert_eq!(bundle.services.len(), 1);
        assert!(bundle.services.contains_key("alpha"));
        assert!(!bundle.services.contains_key("beta"));
        assert!(bundle.reference_values.contains_key("alpha"));
        assert!(!bundle.reference_values.contains_key("beta"));
    }

    #[test]
    fn render_agent_card_connect_config_produces_tng_config() {
        let conf = AgentCardConfidential {
            id: "test-agent".to_string(),
            cache_ttl_sec: 300,
            public_ip: "1.2.3.4".to_string(),
            ports: vec![AgentCardPort {
                name: "api".to_string(),
                port: 18789,
            }],
            reference_values: None,
            rekor: AgentCardRekor {
                rekor_url: "https://rekor.example.com".to_string(),
                artifact_id: "test".to_string(),
                artifact_type: "uki".to_string(),
                artifact_version: "v1".to_string(),
                rv_name: "test".to_string(),
            },
            tee: "tdx".to_string(),
        };
        let card = AgentCard {
            protocol_version: "1.0".to_string(),
            name: "test".to_string(),
            description: "Test agent".to_string(),
            version: None,
            supported_interfaces: vec![AgentInterface {
                url: "http://1.2.3.4:18789".to_string(),
                protocol_binding: "jsonrpc/http".to_string(),
                protocol_version: "1.0".to_string(),
                tenant: None,
            }],
            preferred_transport: None,
            skills: vec![],
            default_input_modes: vec![],
            default_output_modes: vec![],
            capabilities: AgentCardCapabilities {
                extensions: vec![AgentExtension {
                    uri: confidential_agent_core::agent_card::CONFIDENTIAL_AGENT_EXTENSION
                        .to_string(),
                    description: None,
                    required: true,
                    params: serde_json::to_value(conf).unwrap(),
                }],
                ..Default::default()
            },
            provider: None,
            security_schemes: None,
            security: vec![],
            supports_authenticated_extended_card: None,
            signatures: vec![],
        };
        let config =
            render_agent_card_connect_config_with_port_checker(Path::new("/state"), &card, |_| {
                false
            })
            .unwrap();
        assert!(config["add_ingress"].is_array());
        assert!(config["client_endpoints"].is_array());
        assert!(config["control_interface"].is_object());
        let endpoints = config["client_endpoints"].as_array().unwrap();
        assert_eq!(endpoints[0]["service"], "test-agent");
        assert_eq!(endpoints[0]["guest_port"], 18789);
    }
}
