//! Host convenience workflows share the normal init/deploy/connect state machine.
use super::*;

pub(super) fn up(cli: &Cli, args: &UpArgs) -> Result<()> {
    let (spec_path, stage) = match &args.spec {
        Some(path) => (path.clone(), None),
        None => init::create_project(cli, &args.init)?,
    };
    let spec = AgentSpec::from_path(&spec_path)?;
    let paths = context_paths(&cli.state_dir, &spec.service.id);
    let state = read_service_state_file(&paths.service_state)?;
    if stage == Some(InitStage::Build) {
        return commands::cmd_build(
            cli,
            &BuildArgs {
                spec: spec_path,
                render_only: false,
            },
        );
    }
    if spec.deploy.image.is_none() && state.is_none() {
        commands::cmd_build(
            cli,
            &BuildArgs {
                spec: spec_path.clone(),
                render_only: false,
            },
        )?;
    }
    let active = state.as_ref().is_some_and(|state| state.phase == "active");
    if !active {
        configure_credentials(args.init.non_interactive, &spec.deploy.region)?;
        let resume = state
            .as_ref()
            .is_some_and(|state| matches!(state.phase.as_str(), "deployed" | "activating"));
        if !resume {
            let cidr = match &args.deployer_cidr {
                Some(cidr) => cidr.clone(),
                None => format!(
                    "{}/32",
                    resolve_direct_egress_ip()?
                        .context("cannot determine deployment host IP; pass --deployer-cidr")?
                ),
            };
            configure_peerings(&cli.state_dir, &args.allowed_cidr, &cidr)?;
            // In-process handoff keeps credentials and project secrets out of argv.
            std::env::set_var("CA_DEPLOYER_CIDR", &cidr);
        }
        commands::cmd_deploy(
            cli,
            &DeployArgs {
                spec: spec_path,
                resume,
                skip_inject: false,
                render_only: false,
                skip_peering_check: false,
                skip_signature_verification: args.init.skip_signature_verification,
                image_id: args.init.image_id.clone(),
            },
        )?;
    }
    if !args.no_connect && !spec.service.connect.is_empty() {
        let ready_json = paths.service_dir.join("connect-ready.json");
        if ready_json.exists() {
            commands::cmd_connect_stop(&ConnectStopArgs {
                ready_json: ready_json.clone(),
            })?;
        }
        commands::cmd_connect_start(
            cli,
            &ConnectStartArgs {
                instance_id: None,
                from_card: None,
                service: Some(spec.service.id.clone()),
                ready_json,
                wait_ready: args.wait_ready,
                log_file: None,
            },
            None,
            Some(&spec.service.id),
        )?;
    }
    println!("[ca] up completed: service={}", spec.service.id);
    Ok(())
}

fn configure_credentials(non_interactive: bool, region: &str) -> Result<()> {
    if cloud_credentials::load_if_configured()?.is_some() {
        return Ok(());
    }
    if non_interactive {
        return cloud_credentials::load().map(|_| ());
    }
    let key = init::prompt_secret("Aliyun AccessKey ID")?;
    let secret = init::prompt_secret("Aliyun AccessKey secret")?;
    let directory =
        PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".aliyun");
    ensure_private_dir(&directory)?;
    let config = serde_json::json!({"current": "default", "profiles": [{
        "name": "default", "mode": "AK", "region_id": region,
        "access_key_id": key, "access_key_secret": secret,
    }]});
    write_private_file(
        &directory.join("config.json"),
        &serde_json::to_vec_pretty(&config)?,
        0o600,
    )
}

fn configure_peerings(state_dir: &Path, allowed: &[String], deployer: &str) -> Result<()> {
    with_state_dir_lock(state_dir, || {
        let mut peerings = read_peerings_or_empty(state_dir)?;
        for cidr in allowed
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(deployer))
        {
            if peerings
                .peerings
                .iter()
                .any(|entry| entry.cidr == cidr && entry.role == PeeringRole::Operator)
            {
                continue;
            }
            peerings.peerings.push(PeeringEntry {
                label: format!("operator-{}", cidr.replace(['.', '/'], "-")),
                role: PeeringRole::Operator,
                cidr: cidr.to_owned(),
                scope: Vec::new(),
                note: None,
                added_at: None,
                added_by: None,
            });
        }
        peerings.validate()?;
        peerings.write_to_path(&peerings_path(state_dir))
    })
}

pub(super) fn cleanup(cli: &Cli, args: &CleanupArgs) -> Result<()> {
    let services = match &args.service {
        Some(service) => vec![service.clone()],
        None => read_service_states(&cli.state_dir)?
            .into_iter()
            .filter(|state| !matches!(state.phase.as_str(), "built" | "deleted"))
            .map(|state| state.service_id)
            .collect(),
    };
    for service in services {
        println!("[ca] cleanup: {service}");
        if args.dry_run {
            continue;
        }
        let ready_json = context_paths(&cli.state_dir, &service)
            .service_dir
            .join("connect-ready.json");
        if ready_json.exists() {
            commands::cmd_connect_stop(&ConnectStopArgs { ready_json })?;
        }
        commands::cmd_destroy(cli, &DestroyArgs { service })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_networks_keep_custom_entries_and_reuse_the_host_entry() {
        let root = tempfile::tempdir().unwrap();
        configure_peerings(root.path(), &["203.0.113.0/24".into()], "198.51.100.7/32").unwrap();
        configure_peerings(root.path(), &[], "198.51.100.7/32").unwrap();
        let peers = read_peerings_or_empty(root.path()).unwrap();
        assert_eq!(peers.peerings.len(), 2);
        assert!(peers
            .control_cidrs_contain("203.0.113.42".parse().unwrap())
            .unwrap());
        assert!(peers
            .control_cidrs_contain("198.51.100.7".parse().unwrap())
            .unwrap());
    }
}
