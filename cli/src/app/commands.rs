use super::*;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

const DAEMON_STATUS_WAIT_TIMEOUT: Duration = Duration::from_secs(180);
const DAEMON_STATUS_WAIT_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Commands::Init(args) => cmd_init(cli, args),
        Commands::Build(args) => cmd_build(cli, args),
        Commands::Deploy(args) => cmd_deploy(cli, args),
        Commands::Docs(args) => cmd_docs(args),
        Commands::Spec(args) => cmd_spec(args),
        Commands::Key(args) => cmd_key(cli, args),
        Commands::Trustee(args) => trustee::cmd_trustee(cli, args),
        Commands::Inject(args) => cmd_inject(cli, args),
        Commands::Mesh(args) => cmd_mesh(cli, args),
        Commands::Connect(args) => cmd_connect(cli, args),
        Commands::Peering(args) => cmd_peering(cli, args),
        Commands::A2a(args) => cmd_a2a(cli, args),
        Commands::Migrate(args) => cmd_migrate(cli, args),
        Commands::Image(args) => cmd_image(cli, args),
        Commands::Ssh(args) => cmd_ssh(cli, args),
        Commands::Tui(args) => cmd_tui(cli, args),
        Commands::Status(args) => cmd_status(cli, args),
        Commands::Report(args) => cmd_report(cli, args),
        Commands::Destroy(args) => cmd_destroy(cli, args),
        Commands::Version => {
            println!("confidential-agent {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

pub(super) fn cmd_build(cli: &Cli, args: &BuildArgs) -> Result<()> {
    let spec = AgentSpec::from_path(&args.spec)?;
    let paths = context_paths(&cli.state_dir, &spec.service.id);
    let existing_state = read_service_state_file(&paths.service_state)?;
    validate_build_start(existing_state.as_ref())?;
    let run_id = current_build_run_id();
    let selected_variant = spec.image_variant().to_string();
    let mut selected_prepared = None;
    let mut selected_manifest = None;
    let mut selected_rendered = None;
    let mut variants = BTreeMap::new();

    for variant in enabled_build_variants(&spec) {
        let build_id = timestamped_shelter_build_id_for_variant(&spec, &variant, &run_id);
        let mut variant_spec = spec.clone();
        variant_spec.deploy.image_variant = Some(variant.clone());
        let prepared = prepare(
            cli,
            &cli.state_dir,
            &args.spec,
            PrepareOptions {
                build_id: Some(build_id),
                image_variant: Some(variant.clone()),
                resolve_container_images: !args.render_only,
                deploy_names: Some(DeployNames::new(&variant_spec)),
                ..PrepareOptions::default()
            },
        )?;
        let prepared_manifest = read_build_manifest(&paths.manifest)?;
        variants.insert(variant.clone(), manifest_variant_from(&prepared_manifest));
        if !args.render_only {
            println!("[ca] building {variant} image with Shelter...");
            let mut shelter_args = vec![
                OsString::from("--work-dir"),
                prepared.shelter_work_dir.as_os_str().to_os_string(),
                OsString::from("build"),
                OsString::from("--config"),
                prepared.rendered_config.as_os_str().to_os_string(),
                OsString::from("--image-id"),
                OsString::from(prepared.shelter_build_id.clone()),
                OsString::from("--image-type"),
                OsString::from("disk"),
            ];
            run_shelter(cli, &mut shelter_args)?;
        }
        if variant == selected_variant {
            selected_rendered = Some(fs::read_to_string(&prepared.rendered_config).with_context(
                || format!("failed to read '{}'", prepared.rendered_config.display()),
            )?);
            selected_manifest = Some(prepared_manifest);
            selected_prepared = Some(prepared);
        }
    }

    let prepared = selected_prepared.with_context(|| {
        format!("deploy.image_variant '{selected_variant}' is not enabled under build.variants")
    })?;
    let mut manifest = selected_manifest.with_context(|| {
        format!("build manifest for variant '{selected_variant}' was not prepared")
    })?;
    manifest.variants = variants;
    fs::write(&paths.manifest, serde_json::to_string_pretty(&manifest)?)
        .with_context(|| format!("failed to write '{}'", paths.manifest.display()))?;
    fs::write(
        &prepared.rendered_config,
        selected_rendered.with_context(|| {
            format!("rendered config for variant '{selected_variant}' was not captured")
        })?,
    )
    .with_context(|| format!("failed to write '{}'", prepared.rendered_config.display()))?;

    if args.render_only {
        println!("{}", prepared.rendered_config.display());
        return Ok(());
    }

    let state = write_service_state(
        &cli.state_dir,
        &args.spec,
        &spec,
        &DeployObservation::default(),
        &prepared,
        "built",
    )?;
    if let Ok(image) = latest_built_image(&cli.state_dir, &spec) {
        println!(
            "[ca] build completed: service={} image={}",
            state.service_id,
            image.display()
        );
    } else {
        println!("[ca] build completed: service={}", state.service_id);
    }
    Ok(())
}

pub(super) fn validate_build_start(existing: Option<&LocalServiceState>) -> Result<()> {
    let Some(state) = existing else {
        return Ok(());
    };
    if matches!(state.phase.as_str(), "active" | "deployed") {
        bail!(
            "service '{}' is {}; destroy it before building a new local image",
            state.service_id,
            state.phase
        );
    }
    Ok(())
}

pub(super) fn validate_deploy_start(existing: Option<&LocalServiceState>) -> Result<()> {
    let Some(state) = existing else {
        bail!("deploy requires a local build; run build first");
    };
    match state.phase.as_str() {
        "built" | "deleted" => Ok(()),
        "active" | "deployed" => bail!(
            "service '{}' is {}; destroy it before deploying again",
            state.service_id,
            state.phase
        ),
        other => bail!(
            "service '{}' is in unsupported phase '{}' for deploy; run build first",
            state.service_id,
            other
        ),
    }
}

pub(super) fn cmd_key(cli: &Cli, args: &KeyArgs) -> Result<()> {
    match &args.command {
        KeyCommands::GenerateCosign {
            output_key_prefix,
            force,
        } => generate_cosign_key_pair(cli, output_key_prefix, *force),
    }
}

fn generate_cosign_key_pair(cli: &Cli, output_key_prefix: &Path, force: bool) -> Result<()> {
    let key_path = path_with_suffix(output_key_prefix, ".key");
    let pub_path = path_with_suffix(output_key_prefix, ".pub");
    if !force && (key_path.exists() || pub_path.exists()) {
        bail!(
            "cosign key output already exists ('{}' or '{}'); pass --force to overwrite",
            key_path.display(),
            pub_path.display()
        );
    }
    if force {
        for path in [&key_path, &pub_path] {
            if path.exists() {
                fs::remove_file(path)
                    .with_context(|| format!("failed to remove '{}'", path.display()))?;
            }
        }
    }
    if let Some(parent) = output_key_prefix
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create '{}'", parent.display()))?;
    }

    run_containerized_host_tool(
        cli,
        "cosign",
        vec![
            OsString::from("generate-key-pair"),
            OsString::from("--output-key-prefix"),
            output_key_prefix.as_os_str().to_os_string(),
        ],
        vec![output_key_prefix.to_path_buf()],
        vec![("COSIGN_PASSWORD".to_string(), String::new())],
        true,
    )?;
    if key_path.exists() {
        set_mode(&key_path, 0o600)?;
    }
    if pub_path.exists() {
        set_mode(&pub_path, 0o644)?;
    }
    println!(
        "[ca] cosign key pair generated: private={} public={}",
        key_path.display(),
        pub_path.display()
    );
    Ok(())
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn spec_requires_a2a_signing(spec: &AgentSpec) -> bool {
    spec.a2a
        .as_ref()
        .is_some_and(|a2a| a2a.enabled && a2a.signing.required)
}

fn ensure_build_variant_present(
    state: &LocalServiceState,
    variant: &str,
    manifest: &BuildManifestVariant,
    allow_missing_local_image: bool,
) -> Result<()> {
    if !manifest.build_result.exists() {
        bail!(
            "local build result for service '{}' variant '{}' is missing at '{}'; run build first",
            state.service_id,
            variant,
            manifest.build_result.display()
        );
    }
    let result = read_shelter_build_result(&manifest.build_result, &manifest.shelter_build_id)?;
    if allow_missing_local_image {
        return Ok(());
    }
    if !result.image_path.exists() {
        bail!(
            "local image for service '{}' variant '{}' was removed at '{}'; run build first",
            state.service_id,
            variant,
            result.image_path.display()
        );
    }
    Ok(())
}

pub(super) fn cmd_deploy(cli: &Cli, args: &DeployArgs) -> Result<()> {
    let spec = AgentSpec::from_path(&args.spec)?;
    let trustee_mode = spec.attestation.mode == AttestationMode::Trustee;
    if !trustee_mode && trustee::is_managed_service(&cli.state_dir, &spec.service.id)? {
        bail!(
            "service '{}' is still managed by Trustee; destroy or explicitly revoke that deployment before switching to challenge mode",
            spec.service.id
        );
    }
    if trustee_mode && args.skip_inject {
        bail!("--skip-inject is not supported in Trustee mode because boot resources must be synchronized before deployment");
    }
    if spec_requires_a2a_signing(&spec) {
        prepare_sigstore_tools_for_process(cli)?;
    }
    let paths = context_paths(&cli.state_dir, &spec.service.id);
    let current_state = read_service_state_file(&paths.service_state)?;
    validate_deploy_start(current_state.as_ref())?;
    let current_state = current_state.with_context(|| {
        format!(
            "deploy requires a local build for service '{}'; run build first",
            spec.service.id
        )
    })?;
    let current_manifest = read_build_manifest(&paths.manifest).with_context(|| {
        format!(
            "deploy requires a local build manifest for service '{}'; run build first",
            spec.service.id
        )
    })?;
    let deploy_variant = spec.image_variant().to_string();
    let build_variant =
        current_manifest.variant(&deploy_variant, Some(&current_state.build.variant))?;
    let published_image_id =
        publish::published_image_for_deploy(&current_state, &spec, &build_variant);
    ensure_build_variant_present(
        &current_state,
        &deploy_variant,
        &build_variant,
        published_image_id.is_some(),
    )?;
    if let Some(image_id) = published_image_id.as_deref() {
        println!("[ca] using published image {image_id} for deploy (skipping upload+import)");
    }
    validate_mesh_port_conflicts(
        &read_service_states(&cli.state_dir)?,
        &spec.service.id,
        &spec.service.ports,
    )?;
    if !trustee_mode && !args.render_only && !args.skip_inject {
        verify_operator_peering_for_direct_injection(&cli.state_dir, args.skip_peering_check)?;
    }

    let existing_services = read_service_states(&cli.state_dir)?;
    let peerings = read_peerings_or_empty(&cli.state_dir)?;
    let deploy_names = DeployNames::new(&spec);
    let deploy_user_data = if trustee_mode {
        if !args.render_only {
            println!("[ca] synchronizing Trustee boot resources before infrastructure launch...");
            trustee::sync_service(
                cli,
                &cli.state_dir,
                &spec,
                &build_variant.build_result,
                &build_variant.shelter_build_id,
                None,
            )?;
        }
        Some(trustee::runtime_user_data(
            &cli.state_dir,
            &spec.service.id,
        )?)
    } else {
        None
    };
    let prepared = prepare(
        cli,
        &cli.state_dir,
        &args.spec,
        PrepareOptions {
            build_id: Some(build_variant.shelter_build_id.clone()),
            image_variant: Some(deploy_variant.clone()),
            include_deploy: true,
            resolve_container_images: false,
            deploy_names: Some(deploy_names.clone()),
            mesh_peer_cidrs: active_peer_public_cidrs(&spec.service.id, &existing_services)?,
            peerings,
            cloud_image_id: published_image_id.clone(),
            deploy_user_data,
        },
    )?;
    let mut deploy_manifest = read_build_manifest(&paths.manifest)?;
    deploy_manifest.variants = current_manifest.variants.clone();
    fs::write(
        &paths.manifest,
        serde_json::to_string_pretty(&deploy_manifest)?,
    )
    .with_context(|| format!("failed to write '{}'", paths.manifest.display()))?;
    if args.render_only {
        println!("{}", prepared.rendered_config.display());
        return Ok(());
    }
    println!("[ca] deploying infrastructure with Shelter...");
    let mut shelter_args = deploy_shelter_args(&prepared);
    run_shelter(cli, &mut shelter_args)?;

    {
        println!("[ca] Shelter deploy completed; resolving instance outputs...");
        let observation = resolve_deploy_observation(&prepared, &spec)?;
        println!("[ca] writing local service state...");
        write_service_state(
            &cli.state_dir,
            &args.spec,
            &spec,
            &observation,
            &prepared,
            "deployed",
        )?;
        if args.skip_inject {
            return Ok(());
        }
        let injection_ip = observation
            .preferred_injection_ip()
            .context("could not resolve deploy IP from Shelter outputs")?;
        if trustee_mode {
            println!("[ca] refreshing Trustee bootstrap with resolved instance outputs...");
            trustee::sync_service(
                cli,
                &cli.state_dir,
                &spec,
                &prepared.build_result,
                &prepared.shelter_build_id,
                Some(&injection_ip),
            )?;
        } else {
            println!("[ca] injecting attested resources to {injection_ip}...");
            inject_resources(
                cli,
                &cli.state_dir,
                &spec,
                &prepared.build_result,
                &prepared.shelter_build_id,
                &injection_ip,
            )?;
        }
        println!("[ca] waiting for guest daemon status at {injection_ip}:{DAEMON_STATUS_PORT}...");
        wait_for_daemon_status(&injection_ip).with_context(|| {
            format!(
                "guest daemon status did not become reachable for service '{}'; check security group rules, confidential-agentd.service, and guest journal",
                spec.service.id
            )
        })?;
        println!("[ca] generating and delivering mesh bundle...");
        let mut active_state = build_service_state(
            &cli.state_dir,
            &args.spec,
            &spec,
            &observation,
            &prepared,
            "active",
        )?;
        let mesh_generation = sync_mesh_with_candidate(cli, &cli.state_dir, active_state.clone())?;
        active_state.mesh_generation = mesh_generation;
        write_local_service_state(&cli.state_dir, &active_state)?;
        sync_a2a_bundle(cli, &cli.state_dir)?;
        let active_services = read_service_states(&cli.state_dir)?;
        println!("[ca] refreshing active Shelter deploys for public mesh rules...");
        refresh_active_shelter_deploys(cli, &cli.state_dir, &active_services)?;
        println!(
            "[ca] deploy completed: service={} public_ip={} private_ip={} mesh_generation={}",
            active_state.service_id,
            active_state.deploy.public_ip.as_deref().unwrap_or("-"),
            active_state.deploy.private_ip.as_deref().unwrap_or("-"),
            active_state.mesh_generation
        );
        print_debug_ssh_hint(&active_state);
    }

    Ok(())
}

pub(super) fn cmd_inject(cli: &Cli, args: &InjectArgs) -> Result<()> {
    let spec = AgentSpec::from_path(&args.spec)?;
    let trustee_mode = spec.attestation.mode == AttestationMode::Trustee;
    if !trustee_mode && trustee::is_managed_service(&cli.state_dir, &spec.service.id)? {
        bail!(
            "service '{}' is still managed by Trustee; revoke it before using challenge injection",
            spec.service.id
        );
    }
    if spec_requires_a2a_signing(&spec) {
        prepare_sigstore_tools_for_process(cli)?;
    }
    if !trustee_mode {
        verify_operator_peering_for_direct_injection(&cli.state_dir, args.skip_peering_check)?;
    }
    validate_mesh_port_conflicts(
        &read_service_states(&cli.state_dir)?,
        &spec.service.id,
        &spec.service.ports,
    )?;
    let paths = context_paths(&cli.state_dir, &spec.service.id);
    let state = read_service_state_file(&paths.service_state)?.with_context(|| {
        format!(
            "inject requires an existing managed service state for '{}'; run deploy first",
            spec.service.id
        )
    })?;
    if state.phase != "active" && state.phase != "deployed" {
        bail!(
            "inject requires service '{}' to be active or deployed in local state",
            spec.service.id
        );
    }
    let manifest = read_build_manifest(&paths.manifest).with_context(|| {
        format!(
            "inject requires service '{}' to have a build manifest from deploy",
            spec.service.id
        )
    })?;
    let build_variant = manifest.variant(&state.build.variant, Some(&state.build.variant))?;

    if trustee_mode {
        trustee::sync_service(
            cli,
            &cli.state_dir,
            &spec,
            &build_variant.build_result,
            &build_variant.shelter_build_id,
            Some(&args.target_ip),
        )?;
    } else {
        inject_resources(
            cli,
            &cli.state_dir,
            &spec,
            &build_variant.build_result,
            &build_variant.shelter_build_id,
            &args.target_ip,
        )?;
    }
    let mut active_state =
        activate_existing_service_state(&cli.state_dir, &args.spec, &spec, state)?;
    if active_state.deploy.public_ip.is_none() && active_state.deploy.private_ip.is_none() {
        active_state.deploy.public_ip = Some(args.target_ip.clone());
    }
    let mesh_generation = sync_mesh_with_candidate(cli, &cli.state_dir, active_state.clone())?;
    active_state.mesh_generation = mesh_generation;
    write_local_service_state(&cli.state_dir, &active_state)?;
    sync_a2a_bundle(cli, &cli.state_dir)?;
    let active_services = read_service_states(&cli.state_dir)?;
    refresh_active_shelter_deploys(cli, &cli.state_dir, &active_services)?;
    Ok(())
}

pub(super) fn cmd_mesh(cli: &Cli, args: &MeshArgs) -> Result<()> {
    match &args.command {
        MeshCommands::Sync { service } => sync_mesh(cli, &cli.state_dir, service.as_deref()),
    }
}

pub(super) fn cmd_peering(cli: &Cli, args: &PeeringArgs) -> Result<()> {
    match &args.command {
        PeeringCommands::Add {
            role,
            cidr,
            label,
            scope,
            note,
        } => {
            let mut peerings = read_peerings_or_empty(&cli.state_dir)?;
            if peerings.peerings.iter().any(|entry| entry.label == *label) {
                bail!("peering label '{}' already exists", label);
            }
            let entry = PeeringEntry {
                label: label.clone(),
                role: parse_peering_role(role)?,
                cidr: cidr.clone(),
                scope: parse_peering_scopes(scope)?,
                note: note.clone(),
                added_at: Some(current_utc_timestamp()),
                added_by: std::env::var("USER").ok(),
            };
            peerings.peerings.push(entry);
            peerings.write_to_path(&peerings_path(&cli.state_dir))?;
            println!(
                "added peering '{}' ({}). Run `confidential-agent peering apply` to push SG changes.",
                label, cidr
            );
            Ok(())
        }
        PeeringCommands::List => {
            let peerings = read_peerings_or_empty(&cli.state_dir)?;
            if peerings.peerings.is_empty() {
                println!("no peerings");
                return Ok(());
            }
            println!("{:<20} {:<9} {:<20} SCOPE", "LABEL", "ROLE", "CIDR");
            for entry in &peerings.peerings {
                let scopes = entry
                    .effective_scope()
                    .into_iter()
                    .map(peering_scope_name)
                    .collect::<Vec<_>>()
                    .join(",");
                println!(
                    "{:<20} {:<9} {:<20} {}",
                    entry.label,
                    peering_role_name(entry.role),
                    entry.cidr,
                    scopes
                );
            }
            Ok(())
        }
        PeeringCommands::Show { label } => {
            let peerings = read_peerings_or_empty(&cli.state_dir)?;
            let entry = peerings
                .peerings
                .iter()
                .find(|entry| entry.label == *label)
                .with_context(|| format!("peering '{}' does not exist", label))?;
            println!("{}", serde_yaml::to_string(entry)?);
            Ok(())
        }
        PeeringCommands::Remove { label } => {
            let mut peerings = read_peerings_or_empty(&cli.state_dir)?;
            let before = peerings.peerings.len();
            peerings.peerings.retain(|entry| entry.label != *label);
            if peerings.peerings.len() == before {
                bail!("peering '{}' does not exist", label);
            }
            peerings.write_to_path(&peerings_path(&cli.state_dir))?;
            println!(
                "removed peering '{}'. Run `confidential-agent peering apply` to push SG changes.",
                label
            );
            Ok(())
        }
        PeeringCommands::Apply { dry_run } => {
            let peerings = read_peerings_or_empty(&cli.state_dir)?;
            peerings.validate()?;
            let services = read_service_states(&cli.state_dir)?;
            let active = services
                .iter()
                .filter(|service| service.phase == "active")
                .count();
            if *dry_run {
                println!("would refresh Shelter security groups for {active} active services");
                return Ok(());
            }
            refresh_active_shelter_deploys(cli, &cli.state_dir, &services)?;
            println!("applied peerings to {active} active services");
            Ok(())
        }
    }
}

pub(super) fn cmd_a2a(cli: &Cli, args: &A2aArgs) -> Result<()> {
    match &args.command {
        A2aCommands::Add {
            agent_card_url,
            alias,
            service,
            signer_issuer,
            signer_subject,
        } => {
            parse_agent_card_url(agent_card_url).map_err(anyhow::Error::new)?;
            let signer = match (signer_issuer.as_deref(), signer_subject.as_deref()) {
                (Some(issuer), Some(subject))
                    if !issuer.trim().is_empty() && !subject.trim().is_empty() =>
                {
                    Some(A2aSignerPin {
                        issuer: issuer.to_string(),
                        subject: subject.to_string(),
                    })
                }
                (None, None) => None,
                _ => bail!("--signer-issuer and --signer-subject must be provided together"),
            };
            if signer.is_some() {
                prepare_sigstore_tools_for_process(cli)?;
            }
            let mut state = read_a2a_state_or_empty(&cli.state_dir)?;
            if state.peers.iter().any(|peer| peer.url == *agent_card_url) {
                bail!("a2a peer URL '{}' already exists", agent_card_url);
            }
            if let Some(alias) = alias.as_deref() {
                confidential_agent_core::a2a::validate_id("a2a peer alias", alias)?;
                ensure_a2a_alias_available(&cli.state_dir, alias, None)?;
            }
            validate_a2a_scoped_services(&cli.state_dir, service)?;
            let preview = fetch_agent_card_preview(agent_card_url, signer.as_ref());
            let (cli_preview, cli_preview_error) = match preview {
                Ok(preview) => (Some(preview), None),
                Err(err) => {
                    let preview_error = a2a_cli_preview_error(&err);
                    eprintln!(
                        "[ca] warning: fetch from CLI failed (this is OK if peer only allows your service VM IPs); daemon will be authoritative: {err:#}"
                    );
                    (None, Some(preview_error))
                }
            };
            if let (None, Some(preview)) = (alias, cli_preview.as_ref()) {
                ensure_a2a_alias_available(&cli.state_dir, &preview.card_summary.id, None)?;
            }
            if let Some(err) = &cli_preview_error {
                eprintln!("[ca] a2a preview status: {} ({})", err.kind, err.message);
            }
            if signer.is_none()
                && cli_preview
                    .as_ref()
                    .is_some_and(|preview| preview.card_summary.signed)
            {
                eprintln!(
                    "[ca] warning: peer AgentCard is signed but no signer pin was configured; use --signer-issuer and --signer-subject to verify it"
                );
            }
            let peer = A2aStatePeer {
                alias: alias.clone(),
                url: agent_card_url.clone(),
                scoped_services: service.clone(),
                signer,
                added_at: current_utc_timestamp(),
                cli_preview,
                cli_preview_error,
            };
            state.peers.push(peer);
            write_a2a_state(&cli.state_dir, &state)?;
            sync_a2a_bundle(cli, &cli.state_dir)?;
            print_a2a_network_hint(&cli.state_dir, service)?;
            Ok(())
        }
        A2aCommands::Remove { alias_or_url } => {
            let mut state = read_a2a_state_or_empty(&cli.state_dir)?;
            let before = state.peers.len();
            state.peers.retain(|peer| {
                peer.url != *alias_or_url && peer.alias.as_deref() != Some(alias_or_url.as_str())
            });
            if state.peers.len() == before {
                bail!("a2a peer '{}' does not exist", alias_or_url);
            }
            write_a2a_state(&cli.state_dir, &state)?;
            sync_a2a_bundle(cli, &cli.state_dir)?;
            println!("removed a2a peer '{}'", alias_or_url);
            Ok(())
        }
        A2aCommands::List => {
            let state = read_a2a_state_or_empty(&cli.state_dir)?;
            if state.peers.is_empty() {
                println!("no a2a peers");
                return Ok(());
            }
            println!(
                "{:<20} {:<48} {:<18} SERVICES",
                "ALIAS", "URL", "CLI_PREVIEW"
            );
            for peer in &state.peers {
                let services = if peer.scoped_services.is_empty() {
                    "*".to_string()
                } else {
                    peer.scoped_services.join(",")
                };
                println!(
                    "{:<20} {:<48} {:<18} {}",
                    peer.alias.as_deref().unwrap_or("-"),
                    truncate_for_table(&peer.url, 48),
                    if let Some(error) = &peer.cli_preview_error {
                        error.kind.as_str()
                    } else if peer.cli_preview.is_some() {
                        "verified"
                    } else {
                        "unverified"
                    },
                    services
                );
            }
            Ok(())
        }
        A2aCommands::Show { alias_or_url } => {
            let state = read_a2a_state_or_empty(&cli.state_dir)?;
            let peer = find_a2a_peer(&state, alias_or_url)?;
            println!("{}", serde_json::to_string_pretty(peer)?);
            Ok(())
        }
        A2aCommands::Sync { alias, all } => {
            if *all && alias.is_some() {
                bail!("use either --all or --alias, not both");
            }
            let mut state = read_a2a_state_or_empty(&cli.state_dir)?;
            if state.peers.iter().any(|peer| {
                let selected = alias
                    .as_deref()
                    .map(|alias| peer.alias.as_deref() == Some(alias))
                    .unwrap_or(true);
                selected && peer.signer.is_some()
            }) {
                prepare_sigstore_tools_for_process(cli)?;
            }
            for peer in &mut state.peers {
                let selected = alias
                    .as_deref()
                    .map(|alias| peer.alias.as_deref() == Some(alias))
                    .unwrap_or(true);
                if !selected {
                    continue;
                }
                match fetch_agent_card_preview(&peer.url, peer.signer.as_ref()) {
                    Ok(preview) => {
                        peer.cli_preview = Some(preview);
                        peer.cli_preview_error = None;
                    }
                    Err(err) => {
                        peer.cli_preview = None;
                        peer.cli_preview_error = Some(a2a_cli_preview_error(&err));
                        eprintln!(
                            "[ca] warning: CLI fetch failed for '{}'; daemon remains authoritative: {err:#}",
                            peer.alias.as_deref().unwrap_or(&peer.url)
                        );
                    }
                }
            }
            if let Some(alias) = alias.as_deref() {
                if !state
                    .peers
                    .iter()
                    .any(|peer| peer.alias.as_deref() == Some(alias))
                {
                    bail!("a2a peer alias '{}' does not exist", alias);
                }
            }
            write_a2a_state(&cli.state_dir, &state)?;
            sync_a2a_bundle(cli, &cli.state_dir)?;
            Ok(())
        }
    }
}

pub(super) fn cmd_migrate(cli: &Cli, args: &MigrateArgs) -> Result<()> {
    let content = fs::read_to_string(&args.spec)
        .with_context(|| format!("failed to read '{}'", args.spec.display()))?;
    let mut spec_yaml: serde_yaml::Value = serde_yaml::from_str(&content)
        .with_context(|| format!("failed to parse '{}'", args.spec.display()))?;
    let mut migrated_peerings = read_peerings_or_empty(&cli.state_dir)?;

    if let Some(root) = spec_yaml.as_mapping_mut() {
        root.remove(serde_yaml::Value::String("peers".to_string()));
        if let Some(deploy) = root
            .get_mut(serde_yaml::Value::String("deploy".to_string()))
            .and_then(|value| value.as_mapping_mut())
        {
            if let Some(security) = deploy.remove(serde_yaml::Value::String("security".to_string()))
            {
                migrate_security_peerings(&security, &mut migrated_peerings)?;
            }
        }
    }

    let migrated_spec = serde_yaml::to_string(&spec_yaml)?;
    if let Some(out) = args.out.as_ref() {
        fs::write(out, migrated_spec)
            .with_context(|| format!("failed to write '{}'", out.display()))?;
        println!("wrote migrated spec to {}", out.display());
    } else {
        print!("{migrated_spec}");
    }

    let peerings_out = args
        .peerings_out
        .clone()
        .unwrap_or_else(|| peerings_path(&cli.state_dir));
    migrated_peerings.write_to_path(&peerings_out)?;
    println!("wrote migrated peerings to {}", peerings_out.display());
    Ok(())
}

pub(super) fn cmd_connect(cli: &Cli, args: &ConnectArgs) -> Result<()> {
    match &args.command {
        Some(ConnectCommands::Start(start)) => {
            if args.render_only {
                bail!("connect start does not support --render-only; use `connect --render-only` to inspect the forwarding plan");
            }
            let from_card = merge_connect_option(
                "from-card",
                args.from_card.as_ref(),
                start.from_card.as_ref(),
            )?;
            let service =
                merge_connect_option("service", args.service.as_ref(), start.service.as_ref())?;
            return cmd_connect_start(cli, start, from_card, service);
        }
        Some(ConnectCommands::Stop(stop)) => {
            if args.render_only || args.from_card.is_some() || args.service.is_some() {
                bail!("connect stop only accepts --ready-json");
            }
            return cmd_connect_stop(stop);
        }
        None => {}
    }

    let config = resolve_connect_config(cli, args.from_card.as_deref(), args.service.as_deref())?;
    if args.render_only {
        println!("{}", serde_json::to_string_pretty(&config)?);
        return Ok(());
    }

    let tng_config = tng_launch_config(&config);
    let config_content = serde_json::to_string(&tng_config)?;
    let workdir = std::env::current_dir().context("failed to resolve current working directory")?;
    let attestation_workdir = ensure_attestation_workdir(&cli.state_dir)?;
    let mounts = vec![workdir.clone(), attestation_workdir];

    run_tools_container(
        cli,
        ToolContainerSpec {
            tool: "tng",
            tool_args: vec![
                OsString::from("launch"),
                OsString::from(format!("--config-content={config_content}")),
            ],
            mounts,
            envs: inherited_proxy_envs(None),
            workdir: Some(workdir),
            container_name: Some(connect_container_name()),
        },
        true,
    )
}

fn merge_connect_option<'a>(
    name: &str,
    top_level: Option<&'a String>,
    subcommand: Option<&'a String>,
) -> Result<Option<&'a str>> {
    match (top_level, subcommand) {
        (Some(left), Some(right)) if left != right => {
            bail!("conflicting --{name} values: '{left}' and '{right}'")
        }
        (Some(value), _) | (_, Some(value)) => Ok(Some(value.as_str())),
        (None, None) => Ok(None),
    }
}

fn resolve_connect_config(
    cli: &Cli,
    from_card: Option<&str>,
    service: Option<&str>,
) -> Result<serde_json::Value> {
    if let Some(url) = from_card {
        let card = fetch_agent_card(url)
            .map_err(anyhow::Error::new)
            .with_context(|| format!("failed to fetch AgentCard from '{url}'"))?;
        render_agent_card_connect_config(&cli.state_dir, &card)
    } else {
        render_connect_config(&cli.state_dir, service)
    }
}

fn tng_launch_config(config: &serde_json::Value) -> serde_json::Value {
    let mut value = config.clone();
    if let serde_json::Value::Object(map) = &mut value {
        map.remove("client_endpoints");
    }
    value
}

fn cmd_connect_start(
    cli: &Cli,
    args: &ConnectStartArgs,
    from_card: Option<&str>,
    service: Option<&str>,
) -> Result<()> {
    if from_card.is_some() && service.is_some() {
        bail!("connect start accepts either --from-card or --service, not both");
    }
    let config = resolve_connect_config(cli, from_card, service)?;
    let endpoints = connect_client_endpoints(&config)?;
    let tng_config = tng_launch_config(&config);
    let config_content = serde_json::to_string(&tng_config)?;
    let workdir = std::env::current_dir().context("failed to resolve current working directory")?;
    let attestation_workdir = ensure_attestation_workdir(&cli.state_dir)?;
    let mounts = vec![workdir.clone(), attestation_workdir];
    guard_existing_connect_ready(&args.ready_json)?;
    let container_name = connect_container_name();
    let container_id = start_tools_container_detached(
        cli,
        ToolContainerSpec {
            tool: "tng",
            tool_args: vec![
                OsString::from("launch"),
                OsString::from(format!("--config-content={config_content}")),
            ],
            mounts,
            envs: inherited_proxy_envs(None),
            workdir: Some(workdir),
            container_name: Some(container_name.clone()),
        },
    )?;

    if let Err(err) = wait_for_connect_endpoints(&endpoints, Duration::from_secs(args.wait_ready)) {
        let logs = docker_logs(&container_name);
        let _ = stop_connect_container(&container_name);
        if let Some(log_file) = args.log_file.as_ref() {
            let _ = fs::write(log_file, logs.as_deref().unwrap_or(""));
        }
        return Err(err).with_context(|| {
            format!(
                "connect container '{}' did not become ready; logs: {}",
                container_name,
                logs.as_deref().unwrap_or("<unavailable>")
            )
        });
    }

    let ready = ConnectReadyFile {
        schema: "confidential-agent/connect-ready/v1".to_string(),
        container_name: container_name.clone(),
        container_id,
        started_at: current_utc_timestamp(),
        client_endpoints: endpoints,
    };
    if let Some(parent) = args
        .ready_json
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create '{}'", parent.display()))?;
    }
    fs::write(&args.ready_json, serde_json::to_string_pretty(&ready)?)
        .with_context(|| format!("failed to write '{}'", args.ready_json.display()))?;
    if let Some(log_file) = args.log_file.as_ref() {
        fs::write(
            log_file,
            format!(
                "connect container '{}' is running. Use `docker logs {}` for live tunnel logs.\n",
                container_name, container_name
            ),
        )
        .with_context(|| format!("failed to write '{}'", log_file.display()))?;
    }
    println!(
        "[ca] connect ready: {} endpoints; ready_json={}",
        ready.client_endpoints.len(),
        args.ready_json.display()
    );
    for endpoint in &ready.client_endpoints {
        println!(
            "CONNECT_READY service={} guest_port={} url={}",
            endpoint.service, endpoint.guest_port, endpoint.http_base_url
        );
    }
    Ok(())
}

fn cmd_connect_stop(args: &ConnectStopArgs) -> Result<()> {
    let ready: ConnectReadyFile = serde_json::from_str(
        &fs::read_to_string(&args.ready_json)
            .with_context(|| format!("failed to read '{}'", args.ready_json.display()))?,
    )
    .with_context(|| format!("failed to parse '{}'", args.ready_json.display()))?;
    stop_connect_container(&ready.container_name)?;
    println!("[ca] connect stopped: {}", ready.container_name);
    Ok(())
}

fn start_tools_container_detached(cli: &Cli, spec: ToolContainerSpec) -> Result<String> {
    ensure_docker_available()?;
    ensure_docker_image_available(&cli.tools_image)?;
    let envs = spec.envs.clone();
    let mut args = tools_container_args(&cli.tools_image, spec);
    args.insert(2, OsString::from("-d"));
    let mut command = Command::new("docker");
    command.args(args).stdin(Stdio::null());
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command.output().context("failed to execute 'docker'")?;
    if !output.status.success() {
        bail!(
            "docker run for connect tunnel failed with {}; stderr: {}; stdout: {}",
            output.status,
            summarize_command_bytes(&output.stderr),
            summarize_command_bytes(&output.stdout)
        );
    }
    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if id.is_empty() {
        bail!("docker run for connect tunnel did not return a container id");
    }
    Ok(id)
}

fn guard_existing_connect_ready(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let Ok(ready) = serde_json::from_str::<ConnectReadyFile>(&fs::read_to_string(path)?) else {
        return Ok(());
    };
    if connect_container_running(&ready.container_name) {
        bail!(
            "connect tunnel '{}' from '{}' is already running; stop it first or use a different --ready-json",
            ready.container_name,
            path.display()
        );
    }
    Ok(())
}

fn connect_container_running(container_name: &str) -> bool {
    Command::new("docker")
        .args(["inspect", "-f", "{{.State.Running}}", container_name])
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "true")
        .unwrap_or(false)
}

fn connect_client_endpoints(config: &serde_json::Value) -> Result<Vec<ConnectClientEndpoint>> {
    let endpoints = config
        .get("client_endpoints")
        .cloned()
        .context("connect config has no client_endpoints")?;
    let endpoints: Vec<ConnectClientEndpoint> =
        serde_json::from_value(endpoints).context("connect config client_endpoints are invalid")?;
    if endpoints.is_empty() {
        bail!("connect config has no client endpoints");
    }
    Ok(endpoints)
}

fn wait_for_connect_endpoints(
    endpoints: &[ConnectClientEndpoint],
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let mut missing = Vec::new();
        for endpoint in endpoints {
            let address = format!("{}:{}", endpoint.local_host, endpoint.local_port);
            let socket = address
                .parse()
                .with_context(|| format!("connect endpoint address '{address}' is invalid"))?;
            if TcpStream::connect_timeout(&socket, Duration::from_secs(1)).is_err() {
                missing.push(address);
            }
        }
        if missing.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for connect local endpoints: {}",
                missing.join(", ")
            );
        }
        thread::sleep(Duration::from_secs(1));
    }
}

fn stop_connect_container(container_name: &str) -> Result<()> {
    let output = Command::new("docker")
        .args(["rm", "-f", container_name])
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to execute docker rm for '{container_name}'"))?;
    if !output.status.success() {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if text.contains("No such container") {
            eprintln!(
                "[ca] connect container '{}' is already gone",
                container_name
            );
            return Ok(());
        }
        bail!(
            "docker rm -f '{}' failed with {}; stderr: {}; stdout: {}",
            container_name,
            output.status,
            summarize_command_bytes(&output.stderr),
            summarize_command_bytes(&output.stdout)
        );
    }
    Ok(())
}

fn docker_logs(container_name: &str) -> Option<String> {
    Command::new("docker")
        .args(["logs", "--tail", "200", container_name])
        .stdin(Stdio::null())
        .output()
        .ok()
        .map(|output| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
}

fn connect_container_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("ca-connect-{}-{nanos}", std::process::id())
}

pub(super) fn cmd_image(cli: &Cli, args: &ImageArgs) -> Result<()> {
    match &args.command {
        ImageCommands::List { json } => {
            let entries = collect_image_entries(&cli.state_dir)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                print_image_table(&entries);
            }
            Ok(())
        }
        ImageCommands::Rm { service, force } => {
            let paths = context_paths(&cli.state_dir, service);
            let state = read_service_state_file(&paths.service_state)?;
            if let Some(state) = state.as_ref() {
                match state.phase.as_str() {
                    "built" | "deleted" => {}
                    "active" | "deployed" => bail!(
                        "service '{}' is {}; destroy it before removing local images",
                        state.service_id,
                        state.phase
                    ),
                    other => bail!(
                        "service '{}' is in unsupported phase '{}'; only built or deleted local image state can be removed",
                        state.service_id,
                        other
                    ),
                }
            }
            if !paths.service_dir.exists() {
                if *force {
                    println!("local image state for service '{}' does not exist", service);
                    return Ok(());
                }
                bail!("local image state for service '{}' does not exist", service);
            }
            fs::remove_dir_all(&paths.service_dir)
                .with_context(|| format!("failed to remove '{}'", paths.service_dir.display()))?;
            println!("removed local image state for service {}", service);
            Ok(())
        }
        ImageCommands::Publish(args) => publish::cmd_image_publish(cli, args),
        ImageCommands::Unpublish(args) => publish::cmd_image_unpublish(cli, args),
        ImageCommands::Prune(args) => publish::cmd_image_prune(cli, args),
    }
}

pub(super) fn collect_image_entries(state_dir: &Path) -> Result<Vec<ImageListEntry>> {
    let services_dir = state_dir.join("services");
    if !services_dir.exists() {
        return Ok(Vec::new());
    }
    let states = read_service_states(state_dir)?
        .into_iter()
        .map(|state| (state.service_id.clone(), state))
        .collect::<BTreeMap<_, _>>();
    let mut entries = Vec::new();
    for entry in fs::read_dir(&services_dir)
        .with_context(|| format!("failed to read '{}'", services_dir.display()))?
    {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let service_id = entry.file_name().to_string_lossy().trim().to_string();
        if service_id.is_empty() {
            continue;
        }
        let state = states.get(&service_id);
        let image_root = entry.path().join("shelter").join("images");
        let mut service_entries = Vec::new();
        if image_root.exists() {
            for image_entry in fs::read_dir(&image_root)
                .with_context(|| format!("failed to read '{}'", image_root.display()))?
            {
                let image_entry = image_entry?;
                if !image_entry.path().is_dir() {
                    continue;
                }
                let build_id = image_entry.file_name().to_string_lossy().to_string();
                let result_path = image_entry.path().join("build-result.json");
                if !result_path.exists() {
                    continue;
                }
                let image_path = match read_shelter_build_result(&result_path, &build_id) {
                    Ok(result) => result.image_path,
                    Err(err) => {
                        eprintln!(
                            "[ca] warning: skipping invalid build result '{}': {err:#}",
                            result_path.display()
                        );
                        continue;
                    }
                };
                let image_size = image_path
                    .metadata()
                    .ok()
                    .filter(|metadata| metadata.is_file())
                    .map(|metadata| metadata.len());
                service_entries.push(ImageListEntry {
                    service_id: service_id.clone(),
                    phase: state.map(|state| state.phase.clone()),
                    current: state
                        .map(|state| state.build.build_id == build_id)
                        .unwrap_or(false),
                    build_id: build_id.clone(),
                    image_present: image_size.is_some(),
                    image_size,
                    image_path,
                    build_result: result_path,
                    published: state.and_then(|state| {
                        published_info_for_build(&state.build.published, &build_id)
                    }),
                });
            }
        }
        if service_entries.is_empty() {
            if let Some(state) = state {
                let paths = context_paths(state_dir, &state.service_id);
                let result_path =
                    shelter_build_result_path(&paths.shelter_work_dir, &state.build.build_id);
                let image_size = state
                    .build
                    .image_path
                    .metadata()
                    .ok()
                    .filter(|metadata| metadata.is_file())
                    .map(|metadata| metadata.len());
                service_entries.push(ImageListEntry {
                    service_id: service_id.clone(),
                    phase: Some(state.phase.clone()),
                    build_id: state.build.build_id.clone(),
                    current: true,
                    image_path: state.build.image_path.clone(),
                    image_present: image_size.is_some(),
                    image_size,
                    build_result: result_path,
                    published: published_info_for_build(
                        &state.build.published,
                        &state.build.build_id,
                    ),
                });
            }
        }
        entries.extend(service_entries);
    }
    entries.sort_by(|left, right| {
        left.service_id
            .cmp(&right.service_id)
            .then_with(|| right.current.cmp(&left.current))
            .then_with(|| right.build_id.cmp(&left.build_id))
    });
    Ok(entries)
}

fn print_image_table(entries: &[ImageListEntry]) {
    if entries.is_empty() {
        println!("no local images");
        return;
    }
    let has_published = entries.iter().any(|entry| entry.published.is_some());
    println!("Confidential Agent Local Images");
    if has_published {
        println!(
            "{:<18} {:<9} {:<7} {:<30} {:<12} {:<28} IMAGE",
            "SERVICE", "PHASE", "CURRENT", "BUILD_ID", "SIZE", "PUBLISHED"
        );
    } else {
        println!(
            "{:<18} {:<9} {:<7} {:<30} {:<12} IMAGE",
            "SERVICE", "PHASE", "CURRENT", "BUILD_ID", "SIZE"
        );
    }
    for entry in entries {
        let size = entry
            .image_size
            .map(format_bytes)
            .unwrap_or_else(|| "-".to_string());
        if has_published {
            println!(
                "{:<18} {:<9} {:<7} {:<30} {:<12} {:<28} {}",
                entry.service_id,
                entry.phase.as_deref().unwrap_or("-"),
                if entry.current { "yes" } else { "no" },
                truncate_for_table(&entry.build_id, 30),
                size,
                entry.published.as_deref().unwrap_or("-"),
                entry.image_path.display()
            );
        } else {
            println!(
                "{:<18} {:<9} {:<7} {:<30} {:<12} {}",
                entry.service_id,
                entry.phase.as_deref().unwrap_or("-"),
                if entry.current { "yes" } else { "no" },
                truncate_for_table(&entry.build_id, 30),
                size,
                entry.image_path.display()
            );
        }
    }
}

fn published_info_for_build(
    published: &BTreeMap<String, PublishedImage>,
    build_id: &str,
) -> Option<String> {
    let values = published
        .values()
        .filter(|entry| entry.build_id == build_id)
        .map(|entry| {
            let status = match entry.status.as_str() {
                "available" => "avl",
                "importing" => "imp",
                "uploaded" => "upl",
                "uploading" => "upg",
                "failed" => "err",
                _ => "?",
            };
            format!(
                "{}:{}({})",
                entry.region,
                entry.image_id.as_deref().unwrap_or("-"),
                status
            )
        })
        .collect::<Vec<_>>();
    if values.is_empty() {
        None
    } else {
        Some(values.join(","))
    }
}

pub(super) fn cmd_status(cli: &Cli, args: &StatusArgs) -> Result<()> {
    let mut states = read_service_states(&cli.state_dir)?;
    if let Some(service) = args.service.as_deref() {
        states.retain(|state| state.service_id == service);
        if states.is_empty() {
            bail!("no local state for service '{}'", service);
        }
    }
    if states.is_empty() {
        if args.json {
            println!("[]");
        } else {
            println!("no local services");
        }
        return Ok(());
    }
    let views = status_views(&cli.state_dir, &states);
    if args.live {
        let live = collect_live_status(&cli.state_dir, &states);
        if args.json {
            println!("{}", serde_json::to_string_pretty(&live)?);
        } else {
            print_status_table(&views);
            print_live_status_table(&live);
        }
        return Ok(());
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&views)?);
    } else {
        print_status_table(&views);
    }
    Ok(())
}

pub(super) fn status_views(state_dir: &Path, states: &[LocalServiceState]) -> Vec<StatusView> {
    states
        .iter()
        .map(|state| status_view(state_dir, state))
        .collect()
}

fn status_view(state_dir: &Path, state: &LocalServiceState) -> StatusView {
    let paths = context_paths(state_dir, &state.service_id);
    let build_result = shelter_build_result_path(&paths.shelter_work_dir, &state.build.build_id);
    let image_size = state
        .build
        .image_path
        .metadata()
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len());
    let cloud_present = matches!(state.phase.as_str(), "active" | "deployed")
        && (state.deploy.instance_id.is_some()
            || state.deploy.public_ip.is_some()
            || state.deploy.private_ip.is_some());
    StatusView {
        service_id: state.service_id.clone(),
        phase: state.phase.clone(),
        build: StatusBuildView {
            build_id: state.build.build_id.clone(),
            image_name: state.build.image_name.clone(),
            variant: state.build.variant.clone(),
            debug_ssh: state.build.debug_ssh.clone(),
        },
        local_image: StatusLocalImageView {
            present: image_size.is_some(),
            path: state.build.image_path.clone(),
            size: image_size,
            build_result: build_result.clone(),
            build_result_present: build_result.exists(),
        },
        cloud: status_cloud_view(state, cloud_present),
        service: state.service.clone(),
        resources: state.resources.clone(),
        mesh_generation: state.mesh_generation,
        reference_values: state.reference_values.clone(),
    }
}

fn status_cloud_view(state: &LocalServiceState, cloud_present: bool) -> StatusCloudView {
    if !cloud_present {
        return StatusCloudView {
            present: false,
            run_id: None,
            resource_name: None,
            terraform_dir: None,
            image_import_name: None,
            instance_id: None,
            security_group_id: None,
            private_ip: None,
            public_ip: None,
            tee: None,
        };
    }
    StatusCloudView {
        present: true,
        run_id: Some(state.deploy.run_id.clone()),
        resource_name: Some(state.deploy.resource_name.clone()),
        terraform_dir: state.deploy.terraform_dir.clone(),
        image_import_name: state.deploy.image_import_name.clone(),
        instance_id: state.deploy.instance_id.clone(),
        security_group_id: state.deploy.security_group_id.clone(),
        private_ip: state.deploy.private_ip.clone(),
        public_ip: state.deploy.public_ip.clone(),
        tee: Some(state.deploy.tee.clone()),
    }
}

pub(super) fn collect_live_status(
    state_dir: &Path,
    states: &[LocalServiceState],
) -> Vec<LiveStatusView> {
    states
        .iter()
        .cloned()
        .map(|state| {
            let local = status_view(state_dir, &state);
            if !matches!(state.phase.as_str(), "active" | "deployed") {
                return LiveStatusView {
                    local,
                    daemon: None,
                    live_error: Some("service is not active or deployed".to_string()),
                };
            }
            let public_ip = state.deploy.public_ip.as_deref().unwrap_or("").trim();
            if public_ip.is_empty() {
                return LiveStatusView {
                    local,
                    daemon: None,
                    live_error: Some("service has no public_ip".to_string()),
                };
            }
            match fetch_daemon_status(public_ip) {
                Ok(status) => LiveStatusView {
                    local,
                    daemon: Some(status),
                    live_error: None,
                },
                Err(err) => LiveStatusView {
                    local,
                    daemon: None,
                    live_error: Some(err.to_string()),
                },
            }
        })
        .collect()
}

pub(super) fn status_table_columns() -> &'static [&'static str] {
    &[
        "SERVICE",
        "PHASE",
        "BUILD_ID",
        "LOCAL_IMAGE",
        "CLOUD",
        "PUBLIC_IP",
        "PORTS",
        "CONNECT",
    ]
}

pub(super) fn live_status_table_columns() -> &'static [&'static str] {
    &["SERVICE", "DAEMON_PHASE", "APP", "MESH", "SSH", "ERROR"]
}

fn print_status_table(states: &[StatusView]) {
    println!("Confidential Agent Status");
    let columns = status_table_columns();
    println!(
        "{:<18} {:<9} {:<28} {:<11} {:<7} {:<16} {:<12} {:<12}",
        columns[0],
        columns[1],
        columns[2],
        columns[3],
        columns[4],
        columns[5],
        columns[6],
        columns[7],
    );
    for state in states {
        let ports = join_ports(&state.service.ports);
        let connect = join_ports(&state.service.connect.iter().map(|cp| cp.port()).collect::<Vec<_>>());
        println!(
            "{:<18} {:<9} {:<28} {:<11} {:<7} {:<16} {:<12} {:<12}",
            state.service_id,
            state.phase,
            truncate_for_table(&state.build.build_id, 28),
            if state.local_image.present {
                "yes"
            } else {
                "no"
            },
            if state.cloud.present { "yes" } else { "no" },
            state.cloud.public_ip.as_deref().unwrap_or("-"),
            ports,
            if connect.is_empty() {
                "-".to_string()
            } else {
                connect
            }
        );
    }
    let hints = states
        .iter()
        .filter_map(debug_ssh_hint_from_status)
        .collect::<Vec<_>>();
    if !hints.is_empty() {
        println!();
        println!("Debug SSH");
        for hint in hints {
            println!("  {hint}");
        }
    }
}

fn print_live_status_table(statuses: &[LiveStatusView]) {
    println!();
    println!("Daemon Live Status");
    let columns = live_status_table_columns();
    println!(
        "{:<18} {:<18} {:<9} {:<9} {:<9} {}",
        columns[0], columns[1], columns[2], columns[3], columns[4], columns[5]
    );
    for status in statuses {
        let daemon = status.daemon.as_ref();
        let mesh_state = daemon
            .map(|daemon| {
                if daemon.mesh_ready {
                    "ready"
                } else {
                    "pending"
                }
            })
            .unwrap_or("-");
        let ssh_state = if status.local.build.debug_ssh.is_some() {
            daemon
                .map(|daemon| {
                    if daemon.debug_ssh_ready {
                        "ready"
                    } else {
                        "pending"
                    }
                })
                .unwrap_or("-")
        } else {
            "-"
        };
        println!(
            "{:<18} {:<18} {:<9} {:<9} {:<9} {}",
            status.local.service_id,
            daemon
                .map(|daemon| daemon.phase.as_str())
                .unwrap_or("unreachable"),
            daemon
                .map(|daemon| if daemon.app_ready { "ready" } else { "pending" })
                .unwrap_or("-"),
            mesh_state,
            ssh_state,
            status.live_error.as_deref().unwrap_or("-"),
        );
    }

    let has_a2a = statuses
        .iter()
        .filter_map(|status| status.daemon.as_ref())
        .any(|daemon| !daemon.a2a_peers.is_empty());
    if has_a2a {
        println!();
        println!("A2A Peers");
        println!(
            "{:<18} {:<20} {:<9} {:<12} {:<12} {:<12} ERROR",
            "SERVICE", "PEER", "STATE", "FETCH", "SUCCESS", "PORTS"
        );
        for status in statuses {
            let Some(daemon) = status.daemon.as_ref() else {
                continue;
            };
            for (peer, peer_status) in &daemon.a2a_peers {
                let ports = join_ports(&peer_status.ports);
                println!(
                    "{:<18} {:<20} {:<9} {:<12} {:<12} {:<12} {}",
                    status.local.service_id,
                    truncate_for_table(peer, 20),
                    peer_status.state,
                    format_unix_age(peer_status.last_fetch_unix),
                    format_unix_age(peer_status.last_success_unix),
                    if ports.is_empty() {
                        "-".to_string()
                    } else {
                        ports
                    },
                    peer_status
                        .error
                        .as_deref()
                        .map(|err| truncate_for_table(err, 80))
                        .unwrap_or_else(|| "-".to_string()),
                );
            }
        }
    }
}

fn debug_ssh_hint_from_status(state: &StatusView) -> Option<String> {
    let key = state.build.debug_ssh.as_ref()?;
    let target = state
        .cloud
        .public_ip
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            state
                .cloud
                .private_ip
                .as_deref()
                .filter(|value| !value.trim().is_empty())
        });
    Some(match target {
        Some(target) => format!(
            "{}: ssh -i {} root@{}",
            state.service_id,
            key.private_key.display(),
            target
        ),
        None => format!(
            "{}: debug ssh key {}",
            state.service_id,
            key.private_key.display()
        ),
    })
}

fn format_unix_age(timestamp: Option<u64>) -> String {
    let Some(timestamp) = timestamp else {
        return "-".to_string();
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if timestamp > now {
        return "now".to_string();
    }
    format!("{}s ago", now - timestamp)
}

fn print_debug_ssh_hint(state: &LocalServiceState) {
    if let Some(key) = state.build.debug_ssh.as_ref() {
        println!("[ca] debug ssh key: {}", key.private_key.display());
    }
    if let Some(hint) = debug_ssh_hint(state) {
        println!("[ca] debug ssh: {hint}");
    }
}

pub(super) fn debug_ssh_hint(state: &LocalServiceState) -> Option<String> {
    let key = state.build.debug_ssh.as_ref()?;
    let target = state
        .deploy
        .public_ip
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            state
                .deploy
                .private_ip
                .as_deref()
                .filter(|value| !value.trim().is_empty())
        });
    Some(match target {
        Some(target) => format!(
            "{}: ssh -i {} root@{}",
            state.service_id,
            key.private_key.display(),
            target
        ),
        None => format!(
            "{}: debug ssh key {}",
            state.service_id,
            key.private_key.display()
        ),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DebugSshCommand {
    pub(super) private_key: PathBuf,
    pub(super) target: String,
    pub(super) argv: Vec<OsString>,
}

pub(super) fn cmd_ssh(cli: &Cli, args: &SshArgs) -> Result<()> {
    let command = resolve_debug_ssh_command(&cli.state_dir, &args.service, &args.ssh_args)?;
    exec_debug_ssh_command(&command)
}

pub(super) fn resolve_debug_ssh_command(
    state_dir: &Path,
    service: &str,
    ssh_args: &[OsString],
) -> Result<DebugSshCommand> {
    let paths = context_paths(state_dir, service);
    let state = read_service_state_file(&paths.service_state)?.with_context(|| {
        format!(
            "service '{}' has no local state at '{}'; run deploy first",
            service,
            paths.service_state.display()
        )
    })?;
    build_debug_ssh_command(&state, ssh_args)
}

pub(super) fn build_debug_ssh_command(
    state: &LocalServiceState,
    ssh_args: &[OsString],
) -> Result<DebugSshCommand> {
    let key = state.build.debug_ssh.as_ref().with_context(|| {
        format!(
            "service '{}' has no debug SSH key in local state; deploy a debug image first",
            state.service_id
        )
    })?;
    let target = debug_ssh_target(state).with_context(|| {
        format!(
            "service '{}' has no public_ip or private_ip in local state",
            state.service_id
        )
    })?;
    Ok(DebugSshCommand {
        private_key: key.private_key.clone(),
        target: target.to_string(),
        argv: build_debug_ssh_argv(&key.private_key, target, ssh_args),
    })
}

pub(super) fn build_debug_ssh_argv(
    private_key: &Path,
    target: &str,
    ssh_args: &[OsString],
) -> Vec<OsString> {
    let mut argv = vec![
        OsString::from("ssh"),
        OsString::from("-i"),
        private_key.as_os_str().to_os_string(),
        OsString::from(format!("root@{target}")),
    ];
    argv.extend(ssh_args.iter().cloned());
    argv
}

fn debug_ssh_target(state: &LocalServiceState) -> Option<&str> {
    non_empty_ip(state.deploy.public_ip.as_deref())
        .or_else(|| non_empty_ip(state.deploy.private_ip.as_deref()))
}

fn non_empty_ip(value: Option<&str>) -> Option<&str> {
    let value = value?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

#[cfg(unix)]
fn exec_debug_ssh_command(command: &DebugSshCommand) -> Result<()> {
    set_mode(&command.private_key, 0o600).with_context(|| {
        format!(
            "failed to chmod debug SSH private key '{}'",
            command.private_key.display()
        )
    })?;
    let (program, args) = command
        .argv
        .split_first()
        .context("debug SSH command argv is empty")?;
    let err = Command::new(program).args(args).exec();
    Err(err).with_context(|| format!("failed to exec '{}'", program.to_string_lossy()))
}

#[cfg(not(unix))]
fn exec_debug_ssh_command(_command: &DebugSshCommand) -> Result<()> {
    bail!("confidential-agent ssh is only supported on Unix hosts")
}

fn fetch_daemon_status(host: &str) -> Result<DaemonStatus> {
    fetch_daemon_status_from(host, DAEMON_STATUS_PORT, Duration::from_secs(3))
}

fn wait_for_daemon_status(host: &str) -> Result<DaemonStatus> {
    // Override via `CA_DAEMON_STATUS_WAIT_SEC=N` when the guest is known to
    // take longer to converge (e.g. cross-org A2A pairs whose mesh peer
    // can race the first /status probe). Falls back to the conservative
    // 180s default used by single-instance deploys.
    let timeout = std::env::var("CA_DAEMON_STATUS_WAIT_SEC")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DAEMON_STATUS_WAIT_TIMEOUT);
    wait_for_daemon_status_from(
        host,
        DAEMON_STATUS_PORT,
        timeout,
        DAEMON_STATUS_WAIT_INTERVAL,
    )
}

pub(super) fn wait_for_daemon_status_from(
    host: &str,
    port: u16,
    timeout: Duration,
    interval: Duration,
) -> Result<DaemonStatus> {
    let started = Instant::now();
    loop {
        let err = match fetch_daemon_status_from(host, port, Duration::from_secs(3)) {
            Ok(status) => return Ok(status),
            Err(err) => err,
        };
        if started.elapsed() >= timeout {
            bail!(
                "timed out after {}s waiting for daemon status at {}:{}: {err}",
                timeout.as_secs(),
                host,
                port
            );
        }
        thread::sleep(interval);
    }
}

pub(super) fn fetch_daemon_status_from(
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<DaemonStatus> {
    let address = format!("{host}:{port}");
    address
        .parse::<std::net::SocketAddr>()
        .with_context(|| format!("invalid daemon status address '{address}'"))?;

    match fetch_daemon_status_direct(host, port, timeout) {
        Ok(status) => Ok(status),
        Err(direct_error)
            if outbound_proxy_configured() && daemon_status_proxy_retryable(&direct_error) =>
        {
            fetch_daemon_status_via_proxy(host, port, timeout).with_context(|| {
                format!("direct daemon status request failed first: {direct_error:#}")
            })
        }
        Err(error) => Err(error),
    }
}

fn daemon_status_proxy_retryable(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<std::io::Error>().is_some())
}

fn fetch_daemon_status_direct(host: &str, port: u16, timeout: Duration) -> Result<DaemonStatus> {
    let address = format!("{host}:{port}");
    let mut stream = TcpStream::connect_timeout(
        &address
            .parse()
            .with_context(|| format!("invalid daemon status address '{address}'"))?,
        timeout,
    )
    .with_context(|| format!("failed to connect daemon status at {address}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .with_context(|| format!("failed to set read timeout for {address}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .with_context(|| format!("failed to set write timeout for {address}"))?;
    write!(
        stream,
        "GET /status HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .with_context(|| format!("failed to request daemon status at {address}"))?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .with_context(|| format!("failed to read daemon status from {address}"))?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .context("daemon status response is not HTTP")?;
    if !head.starts_with("HTTP/1.1 200 ") && !head.starts_with("HTTP/1.0 200 ") {
        bail!(
            "daemon status returned {}",
            head.lines().next().unwrap_or("unknown status")
        );
    }
    serde_json::from_str(body).context("failed to parse daemon status JSON")
}

fn fetch_daemon_status_via_proxy(host: &str, port: u16, timeout: Duration) -> Result<DaemonStatus> {
    let url = format!("http://{host}:{port}/status");
    let response = ureq::AgentBuilder::new()
        .timeout_connect(timeout)
        .timeout_read(timeout)
        .redirects(0)
        .try_proxy_from_env(true)
        .build()
        .get(&url)
        .call()
        .with_context(|| format!("failed to request daemon status through proxy at {url}"))?;
    let body = response
        .into_string()
        .context("failed to read proxied daemon status response")?;
    serde_json::from_str(&body).context("failed to parse proxied daemon status JSON")
}

fn join_ports(ports: &[u16]) -> String {
    ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn truncate_for_table(value: &str, max: usize) -> String {
    if value.len() <= max {
        value.to_string()
    } else {
        format!("{}...", &value[..max.saturating_sub(3)])
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = UNITS[0];
    for next in UNITS.iter().skip(1) {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    if unit == "B" {
        format!("{bytes}B")
    } else {
        format!("{value:.1}{unit}")
    }
}

pub(super) fn cmd_destroy(cli: &Cli, args: &DestroyArgs) -> Result<()> {
    let paths = context_paths(&cli.state_dir, &args.service);
    let existing_state = read_service_state_file(&paths.service_state)?;
    // The AppSpec is mutable and may have been moved after deployment.
    // Trustee membership is authoritative when present; the immutable mode
    // snapshot prevents a lost Trustee state file from becoming an unsafe
    // challenge fallback.
    let trustee_mode = trustee::service_uses_trustee(
        &cli.state_dir,
        &args.service,
        existing_state
            .as_ref()
            .map(|state| state.attestation_mode.as_str()),
    )?;
    if let Some(state) = existing_state.as_ref() {
        if state.phase == "deleted" && !deploy_runtime_state_present(&state.deploy) {
            if trustee_mode {
                trustee::cleanup_revoked_service(&cli.state_dir, &args.service)?;
            }
            println!("service '{}' is already deleted", args.service);
            return Ok(());
        }
    }
    if !paths.rendered_config.exists() {
        bail!(
            "no local state or rendered Shelter config for service '{}'",
            args.service
        );
    }

    let manifest = read_build_manifest(&paths.manifest)?;
    let mut shelter_args = vec![
        OsString::from("--work-dir"),
        manifest.shelter_work_dir.as_os_str().to_os_string(),
        OsString::from("destroy"),
        OsString::from(manifest.shelter_build_id.clone()),
    ];
    if let Some(terraform_dir) = existing_state
        .as_ref()
        .and_then(|state| state.deploy.terraform_dir.as_ref())
    {
        shelter_args.push(OsString::from("--terraform-dir"));
        shelter_args.push(terraform_dir.as_os_str().to_os_string());
    }
    shelter_args.push(OsString::from("--config"));
    shelter_args.push(paths.rendered_config.as_os_str().to_os_string());
    shelter_args.push(OsString::from("--auto-approve"));
    if trustee_mode {
        println!("[ca] revoking Trustee resource access before infrastructure deletion...");
        if !trustee::revoke_service(&cli.state_dir, &args.service)? {
            bail!(
                "Trustee service '{}' disappeared before access could be revoked; refusing infrastructure deletion",
                args.service
            );
        }
    }
    run_shelter(cli, &mut shelter_args)?;

    if trustee_mode {
        trustee::cleanup_revoked_service(&cli.state_dir, &args.service)?;
    }

    if let Some(mut state) = existing_state {
        state.phase = "deleted".to_string();
        state.generation += 1;
        clear_deploy_runtime_state(&mut state.deploy);
        write_local_service_state(&cli.state_dir, &state)?;
        sync_mesh(cli, &cli.state_dir, None)?;
        let services = read_service_states(&cli.state_dir)?;
        refresh_active_shelter_deploys(cli, &cli.state_dir, &services)?;
    }
    Ok(())
}

fn clear_deploy_runtime_state(deploy: &mut LocalDeployState) {
    deploy.run_id.clear();
    deploy.resource_name.clear();
    deploy.terraform_dir = None;
    deploy.image_source = None;
    deploy.image_import_name = None;
    deploy.bucket = None;
    deploy.instance_id = None;
    deploy.security_group_id = None;
    deploy.private_ip = None;
    deploy.public_ip = None;
    deploy.tee.clear();
    deploy.published_image_id = None;
}

fn deploy_runtime_state_present(deploy: &LocalDeployState) -> bool {
    !deploy.run_id.is_empty()
        || !deploy.resource_name.is_empty()
        || deploy.terraform_dir.is_some()
        || deploy.image_source.is_some()
        || deploy.image_import_name.is_some()
        || deploy.bucket.is_some()
        || deploy.instance_id.is_some()
        || deploy.security_group_id.is_some()
        || deploy.private_ip.is_some()
        || deploy.public_ip.is_some()
        || deploy.published_image_id.is_some()
        || !deploy.tee.is_empty()
}

pub(super) fn deploy_shelter_args(prepared: &PreparedConfig) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("--work-dir"),
        prepared.shelter_work_dir.as_os_str().to_os_string(),
        OsString::from("deploy"),
        OsString::from(prepared.shelter_build_id.clone()),
    ];
    if let Some(terraform_dir) = prepared.terraform_dir.as_ref() {
        args.push(OsString::from("--terraform-dir"));
        args.push(terraform_dir.as_os_str().to_os_string());
    }
    args.push(OsString::from("--config"));
    args.push(prepared.rendered_config.as_os_str().to_os_string());
    args.push(OsString::from("--auto-approve"));
    args
}

fn parse_peering_role(value: &str) -> Result<PeeringRole> {
    match value {
        "operator" => Ok(PeeringRole::Operator),
        "peer" => Ok(PeeringRole::Peer),
        other => bail!(
            "unsupported peering role '{}'; expected operator or peer",
            other
        ),
    }
}

fn peering_role_name(role: PeeringRole) -> &'static str {
    match role {
        PeeringRole::Operator => "operator",
        PeeringRole::Peer => "peer",
    }
}

fn parse_peering_scopes(values: &[String]) -> Result<Vec<PeeringScope>> {
    values
        .iter()
        .map(|value| parse_peering_scope(value))
        .collect()
}

fn parse_peering_scope(value: &str) -> Result<PeeringScope> {
    match value {
        "control" => Ok(PeeringScope::Control),
        "status" => Ok(PeeringScope::Status),
        "ssh" => Ok(PeeringScope::Ssh),
        "agent_card" | "agent-card" => Ok(PeeringScope::AgentCard),
        "connect" => Ok(PeeringScope::Connect),
        "mesh" => Ok(PeeringScope::Mesh),
        other => bail!(
            "unsupported peering scope '{}'; expected control,status,ssh,agent_card,connect,mesh",
            other
        ),
    }
}

fn peering_scope_name(scope: PeeringScope) -> &'static str {
    match scope {
        PeeringScope::Control => "control",
        PeeringScope::Status => "status",
        PeeringScope::Ssh => "ssh",
        PeeringScope::AgentCard => "agent_card",
        PeeringScope::Connect => "connect",
        PeeringScope::Mesh => "mesh",
    }
}

fn read_a2a_state_or_empty(state_dir: &Path) -> Result<A2aStateFile> {
    A2aStateFile::from_path(&a2a_state_path(state_dir))
}

fn write_a2a_state(state_dir: &Path, state: &A2aStateFile) -> Result<()> {
    state.validate()?;
    write_json_atomic(&a2a_state_path(state_dir), state)
}

fn find_a2a_peer<'a>(state: &'a A2aStateFile, alias_or_url: &str) -> Result<&'a A2aStatePeer> {
    state
        .peers
        .iter()
        .find(|peer| peer.url == alias_or_url || peer.alias.as_deref() == Some(alias_or_url))
        .with_context(|| format!("a2a peer '{}' does not exist", alias_or_url))
}

fn ensure_a2a_alias_available(
    state_dir: &Path,
    alias: &str,
    existing_url: Option<&str>,
) -> Result<()> {
    if read_service_states(state_dir)?
        .iter()
        .any(|service| service.service_id == alias && service.phase != "deleted")
    {
        bail!("a2a alias '{}' conflicts with a local service id", alias);
    }
    let state = read_a2a_state_or_empty(state_dir)?;
    if state.peers.iter().any(|peer| {
        peer.alias.as_deref() == Some(alias)
            && existing_url
                .map(|url| peer.url.as_str() != url)
                .unwrap_or(true)
    }) {
        bail!("a2a alias '{}' already exists", alias);
    }
    Ok(())
}

fn validate_a2a_scoped_services(state_dir: &Path, scoped_services: &[String]) -> Result<()> {
    if scoped_services.is_empty() {
        return Ok(());
    }
    let known = read_service_states(state_dir)?
        .into_iter()
        .filter(|service| service.phase != "deleted")
        .map(|service| service.service_id)
        .collect::<BTreeSet<_>>();
    for service in scoped_services {
        confidential_agent_core::a2a::validate_id("a2a scoped service", service)?;
        if !known.contains(service) {
            bail!("a2a scoped service '{}' does not exist locally", service);
        }
    }
    Ok(())
}

fn fetch_agent_card_preview(
    url: &str,
    signer: Option<&A2aSignerPin>,
) -> std::result::Result<A2aCliPreview, AgentCardFetchError> {
    let pin = signer.map(confidential_agent_core::agent_card_signing::AgentCardSignerPin::from);
    let card = fetch_agent_card_with_signer(url, pin.as_ref())?;
    let ext = confidential_extension(&card)
        .map_err(|err| AgentCardFetchError::SchemaValidation(err.to_string()))?;
    Ok(A2aCliPreview {
        fetched_at: current_utc_timestamp(),
        card_summary: A2aCardSummary {
            id: ext.id.clone(),
            public_ip: ext.public_ip.clone(),
            ports: ext.ports.iter().map(|port| port.port).collect(),
            signed: !card.signatures.is_empty(),
        },
        verified: true,
        lint: None,
    })
}

fn a2a_cli_preview_error(err: &AgentCardFetchError) -> A2aCliPreviewError {
    A2aCliPreviewError {
        checked_at: current_utc_timestamp(),
        kind: a2a_cli_preview_error_kind(err).to_string(),
        message: err.to_string(),
    }
}

pub(super) fn a2a_cli_preview_error_kind(err: &AgentCardFetchError) -> &'static str {
    match err {
        AgentCardFetchError::Transport(_) | AgentCardFetchError::HttpStatus { .. } => "unreachable",
        AgentCardFetchError::PublicIpHostMismatch { .. }
        | AgentCardFetchError::HostResolution { .. } => "host_mismatch",
        AgentCardFetchError::RekorUrlNotTrusted { .. } => "rekor_untrusted",
        AgentCardFetchError::SignatureMissing => "unsigned",
        AgentCardFetchError::SignatureVerification(_) => "signature_failed",
        AgentCardFetchError::InvalidUrl(_)
        | AgentCardFetchError::BodyTooLarge
        | AgentCardFetchError::InvalidContentType(_)
        | AgentCardFetchError::InvalidJson(_)
        | AgentCardFetchError::NotConfidentialAgent
        | AgentCardFetchError::LegacyConfidentialAgentCard
        | AgentCardFetchError::SchemaValidation(_) => "invalid",
    }
}

pub(super) fn sync_a2a_bundle(cli: &Cli, state_dir: &Path) -> Result<()> {
    let state = read_a2a_state_or_empty(state_dir)?;
    let bundle = render_a2a_bundle(&state)?;
    let bundle_path = a2a_bundle_path(state_dir);
    write_json_atomic(&bundle_path, &bundle)?;

    let services = read_service_states(state_dir)?;
    let mut delivered = Vec::new();
    for service in services.iter().filter(|service| service.phase == "active") {
        if trustee::service_uses_trustee(
            state_dir,
            &service.service_id,
            Some(&service.attestation_mode),
        )? {
            trustee::put_dynamic_resource(
                state_dir,
                &service.service_id,
                A2A_BUNDLE_RESOURCE,
                &bundle_path,
            )?;
        } else {
            let Some(target_ip) = service.deploy.preferred_injection_ip() else {
                bail!(
                    "service '{}' has no IP for a2a bundle injection",
                    service.service_id
                );
            };
            challenge_inject(
                cli,
                state_dir,
                target_ip,
                A2A_BUNDLE_RESOURCE,
                &bundle_path,
                &service.deploy.tee,
            )?;
        }
        delivered.push(service.service_id.clone());
    }
    if delivered.is_empty() {
        println!("no active services; a2a bundle written locally");
    } else {
        println!(
            "synced a2a bundle to active services: {}",
            delivered.join(", ")
        );
    }
    Ok(())
}

fn render_a2a_bundle(state: &A2aStateFile) -> Result<A2aBundle> {
    state.validate()?;
    let peers = state
        .peers
        .iter()
        .map(|peer| {
            let fingerprint_source = serde_json::json!({
                "alias": peer.alias,
                "url": peer.url,
                "scoped_services": peer.scoped_services,
                "signer": peer.signer,
            });
            Ok(A2aBundlePeer {
                alias: peer.alias.clone(),
                url: peer.url.clone(),
                scoped_services: peer.scoped_services.clone(),
                signer: peer.signer.clone(),
                fingerprint: sha256_bytes(&serde_json::to_vec(&fingerprint_source)?),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let bundle = A2aBundle { version: 2, peers };
    bundle.validate()?;
    Ok(bundle)
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(&hasher.finalize())
}

fn print_a2a_network_hint(state_dir: &Path, scoped_services: &[String]) -> Result<()> {
    let services = read_service_states(state_dir)?;
    let ips = services
        .iter()
        .filter(|service| service.phase == "active")
        .filter(|service| {
            scoped_services.is_empty() || scoped_services.contains(&service.service_id)
        })
        .filter_map(|service| service.deploy.public_ip.as_deref())
        .filter(|ip| !ip.trim().is_empty())
        .map(|ip| format!("{ip}/32"))
        .collect::<BTreeSet<_>>();
    if !ips.is_empty() {
        println!(
            "remote side must allow these caller service public IPs to access :{} and mesh ports: {}",
            AGENT_CARD_PORT,
            ips.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    Ok(())
}

fn migrate_security_peerings(
    security: &serde_yaml::Value,
    peerings: &mut PeeringsFile,
) -> Result<()> {
    let Some(mapping) = security.as_mapping() else {
        return Ok(());
    };
    if let Some(cidr) = mapping
        .get(serde_yaml::Value::String("allowed_cidr".to_string()))
        .and_then(|value| value.as_str())
    {
        if !peerings
            .peerings
            .iter()
            .any(|entry| entry.label == "migrated-operator")
        {
            peerings.peerings.push(PeeringEntry {
                label: "migrated-operator".to_string(),
                role: PeeringRole::Operator,
                cidr: cidr.to_string(),
                scope: Vec::new(),
                note: Some("migrated from deploy.security.allowed_cidr".to_string()),
                added_at: Some(current_utc_timestamp()),
                added_by: std::env::var("USER").ok(),
            });
        }
    }
    if let Some(values) = mapping
        .get(serde_yaml::Value::String("a2a_peer_cidrs".to_string()))
        .and_then(|value| value.as_sequence())
    {
        for (idx, value) in values.iter().enumerate() {
            let Some(cidr) = value.as_str() else {
                continue;
            };
            let label = format!("migrated-peer-{}", idx + 1);
            if peerings.peerings.iter().any(|entry| entry.label == label) {
                continue;
            }
            peerings.peerings.push(PeeringEntry {
                label,
                role: PeeringRole::Peer,
                cidr: cidr.to_string(),
                scope: Vec::new(),
                note: Some("migrated from deploy.security.a2a_peer_cidrs".to_string()),
                added_at: Some(current_utc_timestamp()),
                added_by: std::env::var("USER").ok(),
            });
        }
    }
    peerings.validate()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cli() -> Cli {
        Cli {
            command: Commands::Status(StatusArgs {
                service: None,
                json: false,
                live: false,
            }),
            shelter_bin: PathBuf::from("shelter"),
            state_dir: PathBuf::from("/tmp/confidential-agent-test-state"),
            tools_image: "confidential-agent-tools:test".to_string(),
        }
    }

    fn deploy_state_with_runtime() -> LocalDeployState {
        LocalDeployState {
            provider: "aliyun".to_string(),
            run_id: "run-1".to_string(),
            resource_name: "svc-run-1".to_string(),
            terraform_dir: Some(PathBuf::from("/tmp/tf")),
            image_source: Some(PathBuf::from("/tmp/image.qcow2")),
            image_import_name: Some("image-import".to_string()),
            bucket: Some("bucket".to_string()),
            instance_id: Some("i-123".to_string()),
            security_group_id: Some("sg-123".to_string()),
            private_ip: Some("10.0.0.10".to_string()),
            public_ip: Some("203.0.113.10".to_string()),
            tee: "tdx".to_string(),
            published_image_id: Some("m-123".to_string()),
        }
    }

    fn a2a_state_with_peer() -> A2aStateFile {
        A2aStateFile {
            version: 2,
            peers: vec![A2aStatePeer {
                alias: Some("peer-alpha".to_string()),
                url: "http://203.0.113.8:8089/.well-known/agent-card.json".to_string(),
                scoped_services: vec!["svc-a".to_string()],
                signer: Some(A2aSignerPin {
                    issuer: "https://issuer.example".to_string(),
                    subject: "peer@example.com".to_string(),
                }),
                added_at: "2026-06-11T00:00:00Z".to_string(),
                cli_preview: None,
                cli_preview_error: None,
            }],
        }
    }

    #[test]
    fn connect_command_rejects_invalid_option_combinations_before_external_work() {
        let cli = test_cli();
        let start_args = ConnectArgs {
            render_only: false,
            from_card: Some("http://one.example/card.json".to_string()),
            service: None,
            command: Some(ConnectCommands::Start(ConnectStartArgs {
                from_card: Some("http://two.example/card.json".to_string()),
                service: None,
                ready_json: PathBuf::from("connect-ready.json"),
                wait_ready: 1,
                log_file: None,
            })),
        };

        let err = cmd_connect(&cli, &start_args).unwrap_err();
        assert!(err.to_string().contains("conflicting --from-card values"));

        let stop_args = ConnectArgs {
            render_only: true,
            from_card: None,
            service: None,
            command: Some(ConnectCommands::Stop(ConnectStopArgs {
                ready_json: PathBuf::from("connect-ready.json"),
            })),
        };

        let err = cmd_connect(&cli, &stop_args).unwrap_err();
        assert!(err.to_string().contains("connect stop only accepts"));
    }

    #[test]
    fn merge_connect_option_accepts_absent_matching_and_single_sources() {
        assert_eq!(merge_connect_option("service", None, None).unwrap(), None);

        let top = "svc-a".to_string();
        assert_eq!(
            merge_connect_option("service", Some(&top), None).unwrap(),
            Some("svc-a")
        );

        let sub = "svc-a".to_string();
        assert_eq!(
            merge_connect_option("service", Some(&top), Some(&sub)).unwrap(),
            Some("svc-a")
        );

        let other = "svc-b".to_string();
        let err = merge_connect_option("service", Some(&top), Some(&other)).unwrap_err();
        assert!(err.to_string().contains("conflicting --service values"));
    }

    #[test]
    fn tng_launch_config_removes_cli_only_client_endpoints() {
        let config = serde_json::json!({
            "client_endpoints": [{"service": "svc-a"}],
            "control_interface": {"restful": {"host": "127.0.0.1", "port": 50000}},
            "add_ingress": []
        });

        let launch = tng_launch_config(&config);

        assert!(launch.get("client_endpoints").is_none());
        assert!(config.get("client_endpoints").is_some());
        assert_eq!(launch["control_interface"]["restful"]["port"], 50000);
    }

    #[test]
    fn connect_client_endpoints_validates_presence_shape_and_non_empty() {
        let config = serde_json::json!({
            "client_endpoints": [{
                "service": "svc-a",
                "guest_port": 18789,
                "local_host": "127.0.0.1",
                "local_port": 49152,
                "protocol": "http",
                "http_base_url": "http://127.0.0.1:49152"
            }]
        });

        let endpoints = connect_client_endpoints(&config).unwrap();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].service, "svc-a");
        assert_eq!(endpoints[0].guest_port, 18789);

        let err = connect_client_endpoints(&serde_json::json!({})).unwrap_err();
        assert!(err
            .to_string()
            .contains("connect config has no client_endpoints"));

        let err =
            connect_client_endpoints(&serde_json::json!({"client_endpoints": []})).unwrap_err();
        assert!(err.to_string().contains("no client endpoints"));

        let err = connect_client_endpoints(
            &serde_json::json!({"client_endpoints": [{"service": "svc-a"}]}),
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("connect config client_endpoints are invalid"));
    }

    #[test]
    fn guard_existing_connect_ready_allows_missing_invalid_and_stopped_files() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing.json");
        guard_existing_connect_ready(&missing).unwrap();

        let invalid = temp.path().join("invalid.json");
        fs::write(&invalid, "not-json").unwrap();
        guard_existing_connect_ready(&invalid).unwrap();

        let ready = ConnectReadyFile {
            schema: "confidential-agent/connect-ready/v1".to_string(),
            container_name: format!("ca-connect-unit-test-not-running-{}", std::process::id()),
            container_id: "container-id".to_string(),
            started_at: "2026-06-11T00:00:00Z".to_string(),
            client_endpoints: vec![ConnectClientEndpoint {
                service: "svc-a".to_string(),
                guest_port: 18789,
                local_host: "127.0.0.1".to_string(),
                local_port: 49152,
                protocol: "http".to_string(),
                http_base_url: "http://127.0.0.1:49152".to_string(),
            }],
        };
        let ready_path = temp.path().join("ready.json");
        fs::write(&ready_path, serde_json::to_string(&ready).unwrap()).unwrap();
        guard_existing_connect_ready(&ready_path).unwrap();
    }

    #[test]
    fn daemon_status_fetch_parses_local_http_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0_u8; 512];
            let read = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..read]);
            assert!(request.starts_with("GET /status HTTP/1.1"));

            let body = serde_json::json!({
                "schema": "confidential-agent/daemon-status/v1",
                "service_id": "svc-a",
                "phase": "running",
                "bootstrap_generation": 2,
                "mesh_generation": 3,
                "applied_resources": {},
                "mesh_fingerprint": "abc",
                "app_ready": true,
                "mesh_ready": true,
                "debug_ssh_ready": false,
                "a2a_peers": {},
                "last_error": null
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            stream.flush().unwrap();
            stream.shutdown(std::net::Shutdown::Write).unwrap();
        });

        let status = fetch_daemon_status_from("127.0.0.1", port, Duration::from_secs(1)).unwrap();

        assert_eq!(status.service_id, "svc-a");
        assert_eq!(status.bootstrap_generation, 2);
        assert!(status.app_ready);
        server.join().unwrap();
    }

    #[test]
    fn daemon_status_fetch_rejects_invalid_address_before_connecting() {
        let err = fetch_daemon_status_from("not a host", 8088, Duration::from_secs(1)).unwrap_err();

        assert!(err.to_string().contains("invalid daemon status address"));
    }

    #[test]
    fn daemon_status_proxy_retry_only_handles_transport_failures() {
        let transport = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused",
        ))
        .context("failed to connect daemon status");
        assert!(daemon_status_proxy_retryable(&transport));

        let response = anyhow::anyhow!("daemon status returned HTTP/1.1 503 Service Unavailable");
        assert!(!daemon_status_proxy_retryable(&response));
    }

    #[test]
    fn table_formatting_helpers_cover_boundaries() {
        assert_eq!(join_ports(&[]), "");
        assert_eq!(join_ports(&[8080, 8081]), "8080,8081");
        assert_eq!(truncate_for_table("short", 10), "short");
        assert_eq!(truncate_for_table("abcdefghij", 8), "abcde...");
        assert_eq!(format_bytes(42), "42B");
        assert_eq!(format_bytes(2048), "2.0KiB");
        assert_eq!(format_bytes(3 * 1024 * 1024), "3.0MiB");
        assert_eq!(format_unix_age(None), "-");

        let future = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60;
        assert_eq!(format_unix_age(Some(future)), "now");
    }

    #[test]
    fn deploy_runtime_state_helpers_clear_all_runtime_fields() {
        let mut deploy = deploy_state_with_runtime();
        assert!(deploy_runtime_state_present(&deploy));

        clear_deploy_runtime_state(&mut deploy);

        assert!(!deploy_runtime_state_present(&deploy));
        assert_eq!(deploy.provider, "aliyun");
        assert_eq!(deploy.run_id, "");
        assert_eq!(deploy.resource_name, "");
        assert_eq!(deploy.tee, "");
        assert!(deploy.public_ip.is_none());
        assert!(deploy.published_image_id.is_none());
    }

    #[test]
    fn peering_parsers_accept_documented_values_and_reject_unknowns() {
        assert_eq!(
            parse_peering_role("operator").unwrap(),
            PeeringRole::Operator
        );
        assert_eq!(parse_peering_role("peer").unwrap(), PeeringRole::Peer);
        assert_eq!(peering_role_name(PeeringRole::Operator), "operator");
        assert!(parse_peering_role("admin").is_err());

        let values = vec![
            "control".to_string(),
            "status".to_string(),
            "ssh".to_string(),
            "agent-card".to_string(),
            "connect".to_string(),
            "mesh".to_string(),
        ];
        let scopes = parse_peering_scopes(&values).unwrap();
        assert_eq!(scopes.len(), 6);
        assert_eq!(scopes[3], PeeringScope::AgentCard);
        assert_eq!(peering_scope_name(PeeringScope::AgentCard), "agent_card");
        assert!(parse_peering_scope("unknown").is_err());
    }

    #[test]
    fn a2a_state_helpers_round_trip_and_detect_alias_conflicts() {
        let temp = tempfile::tempdir().unwrap();
        let empty = read_a2a_state_or_empty(temp.path()).unwrap();
        assert_eq!(empty.version, 2);
        assert!(empty.peers.is_empty());

        let state = a2a_state_with_peer();
        write_a2a_state(temp.path(), &state).unwrap();
        let reloaded = read_a2a_state_or_empty(temp.path()).unwrap();
        assert_eq!(reloaded, state);

        let peer = find_a2a_peer(&reloaded, "peer-alpha").unwrap();
        assert_eq!(peer.scoped_services, vec!["svc-a"]);
        let peer = find_a2a_peer(&reloaded, &peer.url).unwrap();
        assert_eq!(peer.alias.as_deref(), Some("peer-alpha"));

        ensure_a2a_alias_available(temp.path(), "peer-alpha", Some(&peer.url)).unwrap();
        let err = ensure_a2a_alias_available(temp.path(), "peer-alpha", None).unwrap_err();
        assert!(err.to_string().contains("already exists"));
        assert!(find_a2a_peer(&reloaded, "missing").is_err());
    }

    #[test]
    fn render_a2a_bundle_hashes_peer_identity_fields() {
        let state = a2a_state_with_peer();

        let bundle = render_a2a_bundle(&state).unwrap();

        assert_eq!(bundle.version, 2);
        assert_eq!(bundle.peers.len(), 1);
        assert_eq!(bundle.peers[0].alias.as_deref(), Some("peer-alpha"));
        assert_eq!(bundle.peers[0].scoped_services, vec!["svc-a"]);
        assert_eq!(bundle.peers[0].fingerprint.len(), 64);

        let mut changed = state.clone();
        changed.peers[0].alias = Some("peer-beta".to_string());
        let changed_bundle = render_a2a_bundle(&changed).unwrap();
        assert_ne!(
            bundle.peers[0].fingerprint,
            changed_bundle.peers[0].fingerprint
        );
    }

    #[test]
    fn a2a_cli_preview_error_includes_kind_and_display_message() {
        let error = AgentCardFetchError::InvalidUrl("missing scheme".to_string());

        let preview = a2a_cli_preview_error(&error);

        assert_eq!(preview.kind, "invalid");
        assert!(preview.message.contains("invalid agent card URL"));
        assert!(!preview.checked_at.is_empty());
    }

    #[test]
    fn debug_ssh_helpers_build_expected_command_lines() {
        let argv = build_debug_ssh_argv(
            Path::new("/tmp/debug.key"),
            "203.0.113.10",
            &[OsString::from("-vv"), OsString::from("uptime")],
        );

        let rendered = argv
            .iter()
            .map(|value| value.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            rendered,
            vec![
                "ssh",
                "-i",
                "/tmp/debug.key",
                "root@203.0.113.10",
                "-vv",
                "uptime"
            ]
        );

        assert_eq!(non_empty_ip(Some(" 10.0.0.8 ")), Some("10.0.0.8"));
        assert_eq!(non_empty_ip(Some("   ")), None);
        assert_eq!(non_empty_ip(None), None);
    }
}
