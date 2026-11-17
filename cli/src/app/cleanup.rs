//! Destroy instances selected from the local state directory.
use super::*;

pub(super) fn run(cli: &Cli, args: &CleanupArgs) -> Result<()> {
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
