use anyhow::{bail, Context, Result};
use confidential_agent_core::a2a::{A2aBundle, A2aBundlePeer};
use confidential_agent_core::agent_card::{agent_card_reference_values, confidential_extension};
use confidential_agent_core::agent_card_fetch::{
    fetch_agent_card_with_signer, AgentCardFetchError,
};
use confidential_agent_core::schema::{
    confidential_ports, AppliedResourceState, BootstrapConfig, DaemonA2aPeerStatus, DaemonStatus,
    GuestResource, MeshBundle, ServiceDirectory, ServiceDirectoryPort, ServiceDirectoryService,
    AGENT_CARD_PORT, BOOTSTRAP_SCHEMA_VERSION, DAEMON_STATUS_PORT, DAEMON_STATUS_SCHEMA_VERSION,
    MESH_SCHEMA_VERSION, SERVICE_DIRECTORY_SCHEMA_VERSION,
};
use confidential_agent_core::spec::EncryptionMode;
use confidential_agent_core::util::{hex_encode, rekor_payload, sha256_file};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{CString, OsString};
use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{chown, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::cli::{Cli, Commands, InitrdFetchArgs, RunArgs};
use crate::trustee::{self, RuntimeMode, TrusteeRuntime};

const DEFAULT_AA_SOCK: &str =
    "unix:///run/confidential-containers/attestation-agent/attestation-agent.sock";
const DEFAULT_TNG_CONFIG_PATH: &str = "/etc/tng/config.json";
const TNG_SERVICE: &str = "trusted-network-gateway.service";
const GATEWAY_SERVICE: &str = "cai-gateway.service";
const TNG_CONTROL_PORT: u16 = 50000;
const DEFAULT_GATEWAY_CONFIG_PATH: &str = "/etc/cai/gateway.json";
const TNG_EGRESS_PORT_BASE: u16 = 39000;
const GATEWAY_SERVER_PORT_BASE: u16 = 39200;
const TNG_INGRESS_PORT_BASE: u16 = 39400;
const DEFAULT_TNG_SO_MARK: u32 = 565;
const SERVICE_RESTART_MIN_INTERVAL_SEC: u64 = 30;
const ACTIVE_BOOTSTRAP_IDLE_POLL_SEC: u64 = 30;
const DEFAULT_POLICY_PATH: &str = "/opt/confidential-agent/policies/trustee-opa-default.rego";
const DEFAULT_DAEMON_STATE_PATH: &str = "/var/lib/confidential-agent/state.json";
const DEFAULT_DAEMON_STATUS_PATH: &str = "/run/confidential-agent/status.json";
const DEFAULT_SERVICE_DIRECTORY_PATH: &str = "/etc/cai/service-directory.json";
const DEFAULT_CACHE_DIR: &str = "/var/cache/confidential-agent";
const DEBUG_SSH_MARKER_PATH: &str = "/etc/confidential-agent/debug-ssh-enabled";
const DEBUG_AUTHORIZED_KEYS_PATH: &str = "/root/.ssh/authorized_keys";
const DEBUG_SSHD_DROPIN_DIR: &str = "/etc/systemd/system/sshd.service.d";
const DEBUG_SSHD_RUN_DIR: &str = "/run/sshd";
const MAX_RESOURCE_BYTES: u64 = 100 * 1024 * 1024;
const DEFAULT_AGENT_CARD_PATH: &str = "/opt/confidential-agent/agent-card.json";
const A2A_CACHE_TTL_MIN_SEC: u64 = 60;
const A2A_CACHE_TTL_MAX_SEC: u64 = 3600;
const A2A_FETCH_FAILURE_BACKOFF_SEC: u64 = 60;
const A2A_OVERDUE_REFRESH_RETRY_SEC: u64 = 1;
const HTTP_MAX_HEADER_BYTES: usize = 8 * 1024;
const HTTP_READ_CHUNK_BYTES: usize = 512;
const HTTP_IO_TIMEOUT: Duration = Duration::from_secs(5);
const HTTP_MAX_CONCURRENT_REQUESTS: usize = 64;
const HTTP_RATE_LIMIT_REQUESTS: usize = 120;
const HTTP_RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy)]
struct InitrdDeadline {
    started: Instant,
    expires_at: Option<Instant>,
}

impl InitrdDeadline {
    fn new(timeout_sec: u64) -> Result<Self> {
        let started = Instant::now();
        let expires_at = if timeout_sec == 0 {
            None
        } else {
            Some(
                started
                    .checked_add(Duration::from_secs(timeout_sec))
                    .context("initrd secret wait timeout is too large")?,
            )
        };
        Ok(Self {
            started,
            expires_at,
        })
    }

    fn remaining(&self, description: &str) -> Result<Option<Duration>> {
        let Some(expires_at) = self.expires_at else {
            return Ok(None);
        };
        let remaining = expires_at.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!(
                "timed out waiting for {description} after {}s",
                self.started.elapsed().as_secs()
            );
        }
        Ok(Some(remaining))
    }

    fn expired(&self) -> bool {
        self.expires_at
            .is_some_and(|expires_at| Instant::now() >= expires_at)
    }

    fn capped_sleep(&self, requested: Duration) -> Duration {
        self.expires_at
            .map(|expires_at| requested.min(expires_at.saturating_duration_since(Instant::now())))
            .unwrap_or(requested)
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyOutcome {
    Updated,
    Unchanged,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct DaemonState {
    bootstrap_generation: u64,
    #[serde(default)]
    mesh_generation: u64,
    applied_resources: BTreeMap<String, AppliedResourceState>,
    mesh_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mesh_started_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mesh_last_restart_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gateway_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gateway_started_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gateway_last_restart_unix: Option<u64>,
    #[serde(default)]
    a2a_cache: BTreeMap<String, A2aCachedPeer>,
    #[serde(default)]
    a2a_status: BTreeMap<String, DaemonA2aPeerStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct A2aCachedPeer {
    url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(default)]
    ports: Vec<A2aCachedPort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    public_ip: Option<String>,
    reference_values: Value,
    fetched_at_unix: u64,
    next_refresh_unix: u64,
    fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct A2aCachedPort {
    remote: u16,
    local: u16,
}

#[derive(Debug, Clone, Copy)]
struct RuntimeReadiness {
    app_ready: bool,
    mesh_ready: bool,
    debug_ssh_ready: bool,
}

fn env_path(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

fn tng_config_path() -> PathBuf {
    env_path("CA_TNG_CONFIG_PATH", DEFAULT_TNG_CONFIG_PATH)
}

fn gateway_config_path() -> PathBuf {
    env_path("CA_GATEWAY_CONFIG_PATH", DEFAULT_GATEWAY_CONFIG_PATH)
}

fn service_directory_path() -> PathBuf {
    env_path("CA_SERVICE_DIRECTORY_PATH", DEFAULT_SERVICE_DIRECTORY_PATH)
}

fn daemon_state_path() -> PathBuf {
    env_path("CA_DAEMON_STATE_PATH", DEFAULT_DAEMON_STATE_PATH)
}

fn daemon_status_path() -> PathBuf {
    env_path("CA_DAEMON_STATUS_PATH", DEFAULT_DAEMON_STATUS_PATH)
}

fn agent_card_path() -> PathBuf {
    env_path("CA_AGENT_CARD_PATH", DEFAULT_AGENT_CARD_PATH)
}

fn daemon_cache_dir() -> PathBuf {
    env_path("CA_DAEMON_CACHE_DIR", DEFAULT_CACHE_DIR)
}

pub(crate) fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Run(args) => run_daemon(args),
        Commands::InitrdFetch(args) => initrd_fetch(args),
        Commands::ApplyOnce(mut args) => {
            let trustee_runtime = trustee::load_rootfs_runtime()?;
            let runtime_mode = if let Some(runtime) = trustee_runtime.as_ref() {
                // The rootfs Attestation Agent is a different service
                // instance from the one used in initrd. Re-register the same
                // canonical runtime binding before any rootfs KBS request.
                runtime.register_aael()?;
                args.cdh_root = trustee::trusted_resource_root();
                fs::create_dir_all(&args.cdh_root).with_context(|| {
                    format!(
                        "failed to create Trustee resource root '{}'",
                        args.cdh_root.display()
                    )
                })?;
                set_mode(&args.cdh_root, 0o700)?;
                // initrd consumes bootstrap bytes directly and deliberately
                // does not stage them into a rootfs provider tree. Make this
                // one-shot command self-contained instead of assuming the
                // long-running daemon already populated that tree.
                runtime.fetch_to_trusted_root(&args.cdh_root, &args.bootstrap_resource)?;
                RuntimeMode::Trustee
            } else {
                RuntimeMode::Challenge
            };
            let bootstrap = read_bootstrap(&args, runtime_mode)?.with_context(|| {
                format!(
                    "bootstrap resource '{}' is not available",
                    args.bootstrap_resource
                )
            })?;
            if let Some(runtime) = trustee_runtime.as_ref() {
                refresh_trustee_bootstrap_resources(runtime, &args, &bootstrap);
                refresh_trustee_dynamic_resources(runtime, &args);
            }
            let mut state = read_daemon_state().unwrap_or_default();
            if apply_bootstrap(&args, &bootstrap, &mut state, true)? {
                sync_mesh(&args, &bootstrap, &mut state)?;
            }
            write_daemon_state(&state)?;
            Ok(())
        }
    }
}

fn initrd_fetch(args: InitrdFetchArgs) -> Result<()> {
    match initrd_fetch_inner(args) {
        Ok(()) => Ok(()),
        Err(err) => initrd_fail_closed(format!("{err:#}")),
    }
}

fn initrd_fetch_inner(args: InitrdFetchArgs) -> Result<()> {
    let deadline = InitrdDeadline::new(args.wait_timeout_sec)?;
    println!("confidential-agentd initrd fetch starting");
    fs::create_dir_all(&args.stage_dir)
        .with_context(|| format!("failed to create '{}'", args.stage_dir.display()))?;

    let (runtime_mode, trustee_runtime) = trustee::detect_initrd_runtime()?;
    let (bootstrap_bytes, disk_key) = if let Some(runtime) = trustee_runtime.as_ref() {
        // Trustee data never traverses the challenge/CDH injection tree.
        // Consume the attested one-shot response directly in initrd so an
        // untrusted writer cannot replace it between a write and a read.
        retry_initrd_operation(
            "Trustee runtime AAEL registration",
            &deadline,
            args.retry_interval_sec,
            |_| runtime.register_aael(),
        )?;
        (
            wait_for_trustee_resource(
                runtime,
                &args.bootstrap_resource,
                &deadline,
                args.retry_interval_sec,
            )?,
            wait_for_trustee_resource(
                runtime,
                &args.disk_key_resource,
                &deadline,
                args.retry_interval_sec,
            )?,
        )
    } else {
        let bootstrap_path = args.cdh_root.join(&args.bootstrap_resource);
        let bootstrap = wait_for_resource(&bootstrap_path, &deadline, args.retry_interval_sec)
            .with_context(|| {
                format!(
                    "bootstrap resource '{}' is not available",
                    args.bootstrap_resource
                )
            })?;
        let disk_key_path = args.cdh_root.join(&args.disk_key_resource);
        let disk_key = wait_for_resource(&disk_key_path, &deadline, args.retry_interval_sec)
            .with_context(|| {
                format!(
                    "disk key resource '{}' is not available",
                    args.disk_key_resource
                )
            })?;
        (bootstrap, disk_key)
    };
    let bootstrap: BootstrapConfig =
        serde_json::from_slice(&bootstrap_bytes).context("failed to parse bootstrap config")?;
    validate_bootstrap(&bootstrap, runtime_mode)?;

    let staged_key = args.stage_dir.join("disk_key");
    fs::write(&staged_key, disk_key)
        .with_context(|| format!("failed to write '{}'", staged_key.display()))?;
    set_mode(&staged_key, 0o600)?;
    if let Some(runtime) = trustee_runtime.as_ref() {
        runtime.persist_for_rootfs()?;
    }

    println!(
        "confidential-agentd initrd fetch complete for service {} mode={}",
        bootstrap.service_id,
        match runtime_mode {
            RuntimeMode::Challenge => "challenge",
            RuntimeMode::Trustee => "trustee",
        }
    );
    Ok(())
}

fn wait_for_resource(path: &Path, deadline: &InitrdDeadline, interval_sec: u64) -> Result<Vec<u8>> {
    retry_initrd_operation(
        &format!("initrd resource '{}'", path.display()),
        deadline,
        interval_sec,
        |_| match fs::read(path) {
            Ok(bytes) if !bytes.is_empty() => Ok(bytes),
            Ok(_) => bail!("resource is empty"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                bail!("resource does not exist")
            }
            Err(err) => Err(err).with_context(|| format!("failed to read '{}'", path.display())),
        },
    )
}

fn wait_for_trustee_resource(
    runtime: &TrusteeRuntime,
    logical_path: &str,
    deadline: &InitrdDeadline,
    interval_sec: u64,
) -> Result<Vec<u8>> {
    retry_initrd_operation(
        &format!("Trustee resource '{logical_path}'"),
        deadline,
        interval_sec,
        |remaining| runtime.fetch_resource_with_timeout(logical_path, remaining),
    )
}

fn retry_initrd_operation<T, F>(
    description: &str,
    deadline: &InitrdDeadline,
    interval_sec: u64,
    mut operation: F,
) -> Result<T>
where
    F: FnMut(Option<Duration>) -> Result<T>,
{
    let interval = Duration::from_secs(interval_sec);
    let mut attempt = 0u64;
    loop {
        attempt += 1;
        let remaining = deadline.remaining(description)?;
        match operation(remaining) {
            Ok(value) => return Ok(value),
            Err(err) => {
                if deadline.expired() {
                    return Err(err).with_context(|| {
                        format!(
                            "timed out waiting for {description} after {}s",
                            deadline.elapsed().as_secs()
                        )
                    });
                }
                eprintln!(
                    "waiting for {description} attempt={attempt} elapsed={}s: {err:#}",
                    deadline.elapsed().as_secs()
                );
                thread::sleep(deadline.capped_sleep(interval));
            }
        }
    }
}

fn initrd_fail_closed(reason: String) -> Result<()> {
    eprintln!("confidential-agentd initrd fetch failed: {reason}");
    if initrd_poweroff_enabled() {
        let systemctl =
            std::env::var_os("CA_SYSTEMCTL_BIN").unwrap_or_else(|| OsString::from("systemctl"));
        let _ = Command::new(systemctl)
            .arg("--no-block")
            .arg("poweroff")
            .status();
    }
    bail!(reason)
}

fn initrd_poweroff_enabled() -> bool {
    std::env::var_os("CA_SKIP_INITRD_POWEROFF").is_none()
        && (Path::new("/etc/initrd-release").exists()
            || std::env::var_os("CA_FORCE_INITRD_POWEROFF").is_some())
}

fn run_daemon(mut args: RunArgs) -> Result<()> {
    println!("confidential-agentd starting");
    let trustee_runtime = trustee::load_rootfs_runtime()?;
    let runtime_mode = if let Some(runtime) = trustee_runtime.as_ref() {
        // Do not assume the initrd Attestation Agent's in-memory AAEL
        // survived switch_root. Duplicate events with the same canonical
        // digest are accepted by the resource policy.
        runtime.register_aael()?;
        // The challenge injection service can continue to exist in the
        // mode-neutral image, but Trustee mode never consumes its file tree.
        args.cdh_root = trustee::trusted_resource_root();
        fs::create_dir_all(&args.cdh_root).with_context(|| {
            format!(
                "failed to create Trustee resource root '{}'",
                args.cdh_root.display()
            )
        })?;
        set_mode(&args.cdh_root, 0o700)?;
        RuntimeMode::Trustee
    } else {
        RuntimeMode::Challenge
    };
    let http_limits = Arc::new(HttpServerLimits::default());
    start_http_server(
        &args.status_listen,
        HttpServerKind::Status,
        http_limits.clone(),
    )?;
    start_http_server(
        &args.agent_card_listen,
        HttpServerKind::AgentCard,
        http_limits,
    )?;
    let mut resource_watcher = ResourceWatcher::new(
        &args.cdh_root,
        [
            Path::new(&args.bootstrap_resource),
            Path::new(&args.mesh_resource),
            Path::new(&args.a2a_bundle_resource),
        ],
    )?;
    let mut state = read_daemon_state().unwrap_or_default();
    let mut active_bootstrap: Option<BootstrapConfig> = None;
    loop {
        if let Some(runtime) = trustee_runtime.as_ref() {
            if let Err(err) =
                runtime.fetch_to_trusted_root(&args.cdh_root, &args.bootstrap_resource)
            {
                eprintln!("Trustee bootstrap refresh failed: {err:#}");
            }
        }
        match read_bootstrap(&args, runtime_mode)? {
            Some(bootstrap) => {
                if let Some(runtime) = trustee_runtime.as_ref() {
                    refresh_trustee_bootstrap_resources(runtime, &args, &bootstrap);
                }
                resource_watcher.add_resource_paths(
                    bootstrap
                        .resources
                        .iter()
                        .map(|resource| Path::new(&resource.resource_path)),
                )?;
                let resources_ready = match bootstrap_resources_ready(&bootstrap, &state) {
                    Ok(ready) => ready,
                    Err(err) => {
                        eprintln!("resource readiness check failed: {err:#}");
                        false
                    }
                };
                let bootstrap_changed = active_bootstrap.as_ref() != Some(&bootstrap);
                let ready = if resources_ready && !bootstrap_changed {
                    let readiness = ensure_runtime_ready(&bootstrap, false);
                    if let Err(err) = write_status(status_for(
                        &bootstrap,
                        &state,
                        "resources-applied",
                        readiness,
                    )) {
                        eprintln!("daemon status write failed: {err:#}");
                    }
                    true
                } else {
                    match apply_bootstrap(&args, &bootstrap, &mut state, false) {
                        Ok(ready) => ready,
                        Err(err) => {
                            eprintln!("resource apply failed: {err:#}");
                            false
                        }
                    }
                };
                if ready {
                    active_bootstrap = Some(bootstrap);
                }
            }
            None => {
                eprintln!(
                    "waiting for bootstrap resource at {}",
                    args.cdh_root.join(&args.bootstrap_resource).display()
                );
            }
        }

        if let Some(bootstrap) = active_bootstrap.as_ref() {
            if let Some(runtime) = trustee_runtime.as_ref() {
                refresh_trustee_dynamic_resources(runtime, &args);
            }
            if let Err(err) = sync_mesh(&args, bootstrap, &mut state) {
                eprintln!("mesh sync failed: {err:#}");
            }
        }
        if let Err(err) = write_daemon_state(&state) {
            eprintln!("daemon state write failed: {err:#}");
        }

        let refresh_timeout = next_resource_refresh_timeout(
            &state,
            active_bootstrap.is_some(),
            trustee_runtime.is_some(),
        );
        match resource_watcher.wait(refresh_timeout)? {
            ResourceWatcherWait::Event | ResourceWatcherWait::Timeout => {}
        }
    }
}

fn next_resource_refresh_timeout(
    state: &DaemonState,
    bootstrap_active: bool,
    trustee_enabled: bool,
) -> Option<Duration> {
    if !bootstrap_active {
        return trustee_enabled.then(|| Duration::from_secs(5));
    }
    let idle_poll = Duration::from_secs(ACTIVE_BOOTSTRAP_IDLE_POLL_SEC);
    Some(
        next_a2a_refresh_timeout(state, true)
            .map(|timeout| timeout.min(idle_poll))
            .unwrap_or(idle_poll),
    )
}

fn refresh_trustee_bootstrap_resources(
    runtime: &TrusteeRuntime,
    args: &RunArgs,
    bootstrap: &BootstrapConfig,
) {
    for resource in &bootstrap.resources {
        let staged = args.cdh_root.join(&resource.resource_path);
        if !resource.mutable
            && resource.sha256.as_ref().is_some_and(|expected| {
                staged.is_file()
                    && sha256_file(&staged)
                        .map(|actual| &actual == expected)
                        .unwrap_or(false)
            })
        {
            continue;
        }
        if let Err(err) = runtime.fetch_to_trusted_root(&args.cdh_root, &resource.resource_path) {
            eprintln!(
                "Trustee resource '{}' refresh failed: {err:#}",
                resource.resource_path
            );
        }
    }
}

fn refresh_trustee_dynamic_resources(runtime: &TrusteeRuntime, args: &RunArgs) {
    for resource_path in [&args.mesh_resource, &args.a2a_bundle_resource] {
        if let Err(err) = runtime.fetch_to_trusted_root(&args.cdh_root, resource_path) {
            eprintln!("Trustee dynamic resource '{resource_path}' refresh failed: {err:#}");
        }
    }
}

fn next_a2a_refresh_timeout(state: &DaemonState, bootstrap_active: bool) -> Option<Duration> {
    if !bootstrap_active {
        return None;
    }
    let next_refresh = state
        .a2a_cache
        .values()
        .map(|peer| peer.next_refresh_unix)
        .min()?;
    let now = now_unix();
    if next_refresh <= now {
        Some(Duration::from_secs(A2A_OVERDUE_REFRESH_RETRY_SEC))
    } else {
        Some(Duration::from_secs(next_refresh - now))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResourceWatcherWait {
    Event,
    Timeout,
}

#[derive(Debug)]
struct ResourceWatcher {
    fd: RawFd,
    cdh_root: PathBuf,
    resource_paths: BTreeSet<PathBuf>,
    watches: BTreeMap<RawFd, PathBuf>,
    watched_dirs: BTreeMap<PathBuf, RawFd>,
    read_buf: Vec<u8>,
}

impl ResourceWatcher {
    fn new<I, P>(cdh_root: &Path, resource_paths: I) -> Result<Self>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let metadata = fs::metadata(cdh_root)
            .with_context(|| format!("cdh root '{}' is not available", cdh_root.display()))?;
        if !metadata.is_dir() {
            bail!("cdh root '{}' is not a directory", cdh_root.display());
        }

        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("failed to initialize inotify");
        }

        let mut watcher = Self {
            fd,
            cdh_root: cdh_root.to_path_buf(),
            resource_paths: BTreeSet::new(),
            watches: BTreeMap::new(),
            watched_dirs: BTreeMap::new(),
            read_buf: vec![0; 16 * 1024],
        };
        for path in resource_paths {
            watcher.resource_paths.insert(path.as_ref().to_path_buf());
        }
        if let Err(err) = watcher.reconcile_watches() {
            drop(watcher);
            return Err(err);
        }
        Ok(watcher)
    }

    fn add_resource_paths<I, P>(&mut self, paths: I) -> Result<()>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        for path in paths {
            self.resource_paths.insert(path.as_ref().to_path_buf());
        }
        Ok(())
    }

    fn wait(&mut self, timeout: Option<Duration>) -> Result<ResourceWatcherWait> {
        if self.reconcile_watches()? {
            return Ok(ResourceWatcherWait::Event);
        }
        let timeout_ms = poll_timeout_ms(timeout);
        loop {
            let mut pollfd = libc::pollfd {
                fd: self.fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
            if ready == 0 {
                return Ok(ResourceWatcherWait::Timeout);
            }
            if ready < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err).context("inotify poll failed");
            }
            if pollfd.revents & libc::POLLNVAL != 0 {
                bail!("inotify poll failed: watcher file descriptor is invalid");
            }
            if pollfd.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                bail!("inotify poll failed with revents={}", pollfd.revents);
            }
            if pollfd.revents & libc::POLLIN == 0 {
                continue;
            }

            if self.drain_events()? {
                self.reconcile_watches()?;
                return Ok(ResourceWatcherWait::Event);
            }
        }
    }

    fn drain_events(&mut self) -> Result<bool> {
        let mut saw_event = false;
        loop {
            let read = unsafe {
                libc::read(
                    self.fd,
                    self.read_buf.as_mut_ptr().cast(),
                    self.read_buf.len(),
                )
            };
            if read < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    return Ok(saw_event);
                }
                return Err(err).context("failed to read inotify events");
            }
            if read == 0 {
                return Ok(saw_event);
            }

            saw_event = true;
            self.process_events(read as usize);
        }
    }

    fn process_events(&mut self, bytes_read: usize) {
        let event_size = std::mem::size_of::<libc::inotify_event>();
        let mut offset = 0;
        while offset + event_size <= bytes_read {
            let event = unsafe {
                std::ptr::read_unaligned(
                    self.read_buf
                        .as_ptr()
                        .add(offset)
                        .cast::<libc::inotify_event>(),
                )
            };
            if event.mask & libc::IN_Q_OVERFLOW != 0 {
                eprintln!(
                    "inotify event queue overflow; treating as resource wake and reconciling watches"
                );
            }
            if event.mask & (libc::IN_IGNORED | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0 {
                self.remove_watch_id(event.wd);
            }
            offset += event_size + event.len as usize;
        }
    }

    fn reconcile_watches(&mut self) -> Result<bool> {
        let desired = self.existing_watch_dirs()?;
        let mut changed = false;
        let stale = self
            .watched_dirs
            .keys()
            .filter(|path| !desired.contains(*path))
            .cloned()
            .collect::<Vec<_>>();
        for path in stale {
            self.remove_watch_path(&path)?;
            changed = true;
        }
        for path in desired {
            if !self.watched_dirs.contains_key(&path) {
                self.add_watch(&path)?;
                changed = true;
            }
        }
        Ok(changed)
    }

    fn existing_watch_dirs(&self) -> Result<BTreeSet<PathBuf>> {
        let metadata = fs::metadata(&self.cdh_root)
            .with_context(|| format!("cdh root '{}' is not available", self.cdh_root.display()))?;
        if !metadata.is_dir() {
            bail!("cdh root '{}' is not a directory", self.cdh_root.display());
        }

        let mut dirs = BTreeSet::new();
        dirs.insert(self.cdh_root.clone());
        for resource_path in &self.resource_paths {
            let target = self.cdh_root.join(resource_path);
            let Some(parent) = target.parent() else {
                continue;
            };
            let Ok(relative_parent) = parent.strip_prefix(&self.cdh_root) else {
                continue;
            };
            let mut current = self.cdh_root.clone();
            for component in relative_parent.components() {
                current.push(component.as_os_str());
                match fs::metadata(&current) {
                    Ok(metadata) if metadata.is_dir() => {
                        dirs.insert(current.clone());
                    }
                    Ok(_) => break,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
                    Err(err) => {
                        return Err(err)
                            .with_context(|| format!("failed to stat '{}'", current.display()));
                    }
                }
            }
        }
        Ok(dirs)
    }

    fn add_watch(&mut self, path: &Path) -> Result<()> {
        let c_path = path_to_cstring(path)?;
        let mask = resource_watch_mask() | libc::IN_ONLYDIR;
        loop {
            let wd = unsafe { libc::inotify_add_watch(self.fd, c_path.as_ptr(), mask) };
            if wd >= 0 {
                self.watches.insert(wd, path.to_path_buf());
                self.watched_dirs.insert(path.to_path_buf(), wd);
                return Ok(());
            }
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err)
                .with_context(|| format!("failed to add inotify watch for '{}'", path.display()));
        }
    }

    fn remove_watch_id(&mut self, wd: RawFd) {
        if let Some(path) = self.watches.remove(&wd) {
            if self.watched_dirs.get(&path).copied() == Some(wd) {
                self.watched_dirs.remove(&path);
            }
        }
    }

    fn remove_watch_path(&mut self, path: &Path) -> Result<()> {
        let Some(wd) = self.watched_dirs.remove(path) else {
            return Ok(());
        };
        self.watches.remove(&wd);
        loop {
            let removed = unsafe { libc::inotify_rm_watch(self.fd, wd) };
            if removed == 0 {
                return Ok(());
            }
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if err.raw_os_error() == Some(libc::EINVAL) {
                return Ok(());
            }
            return Err(err).with_context(|| {
                format!("failed to remove inotify watch for '{}'", path.display())
            });
        }
    }
}

impl Drop for ResourceWatcher {
    fn drop(&mut self) {
        let _ = unsafe { libc::close(self.fd) };
    }
}

fn resource_watch_mask() -> u32 {
    libc::IN_CREATE
        | libc::IN_MODIFY
        | libc::IN_CLOSE_WRITE
        | libc::IN_MOVED_TO
        | libc::IN_MOVED_FROM
        | libc::IN_DELETE
        | libc::IN_DELETE_SELF
        | libc::IN_MOVE_SELF
}

fn path_to_cstring(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("path '{}' contains an interior NUL byte", path.display()))
}

fn poll_timeout_ms(timeout: Option<Duration>) -> libc::c_int {
    match timeout {
        Some(timeout) => {
            let millis = timeout.as_millis();
            millis.min(libc::c_int::MAX as u128) as libc::c_int
        }
        None => -1,
    }
}

#[derive(Debug, Clone, Copy)]
enum HttpServerKind {
    Status,
    AgentCard,
}

#[derive(Debug)]
struct HttpServerLimits {
    active_requests: Arc<AtomicUsize>,
    max_concurrent_requests: usize,
    rate_limiter: Mutex<FixedWindowRateLimiter>,
}

impl HttpServerLimits {
    fn new(
        max_concurrent_requests: usize,
        max_peer_requests: usize,
        peer_window: Duration,
    ) -> Self {
        Self {
            active_requests: Arc::new(AtomicUsize::new(0)),
            max_concurrent_requests,
            rate_limiter: Mutex::new(FixedWindowRateLimiter::new(max_peer_requests, peer_window)),
        }
    }

    fn try_acquire_request(&self) -> Option<ActiveRequestGuard> {
        let mut current = self.active_requests.load(Ordering::Relaxed);
        loop {
            if current >= self.max_concurrent_requests {
                return None;
            }
            match self.active_requests.compare_exchange_weak(
                current,
                current + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(ActiveRequestGuard {
                        active_requests: self.active_requests.clone(),
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }

    fn allow_peer(&self, peer: IpAddr) -> bool {
        let mut rate_limiter = self
            .rate_limiter
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        rate_limiter.allow(peer, Instant::now())
    }
}

impl Default for HttpServerLimits {
    fn default() -> Self {
        Self::new(
            HTTP_MAX_CONCURRENT_REQUESTS,
            HTTP_RATE_LIMIT_REQUESTS,
            HTTP_RATE_LIMIT_WINDOW,
        )
    }
}

struct ActiveRequestGuard {
    active_requests: Arc<AtomicUsize>,
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        self.active_requests.fetch_sub(1, Ordering::Release);
    }
}

#[derive(Debug)]
struct FixedWindowRateLimiter {
    max_requests: usize,
    window: Duration,
    peers: HashMap<IpAddr, PeerRateWindow>,
}

impl FixedWindowRateLimiter {
    fn new(max_requests: usize, window: Duration) -> Self {
        Self {
            max_requests,
            window,
            peers: HashMap::new(),
        }
    }

    fn allow(&mut self, peer: IpAddr, now: Instant) -> bool {
        let window = self.window;
        self.peers.retain(|_, rate| {
            now.checked_duration_since(rate.window_started)
                .is_some_and(|elapsed| elapsed < window)
        });

        if self.max_requests == 0 {
            return false;
        }

        let rate = self.peers.entry(peer).or_insert(PeerRateWindow {
            window_started: now,
            count: 0,
        });
        if rate.count >= self.max_requests {
            return false;
        }
        rate.count += 1;
        true
    }
}

#[derive(Debug)]
struct PeerRateWindow {
    window_started: Instant,
    count: usize,
}

fn start_http_server(
    listen: &str,
    kind: HttpServerKind,
    limits: Arc<HttpServerLimits>,
) -> Result<()> {
    let listener = TcpListener::bind(listen)
        .with_context(|| format!("failed to bind daemon {:?} API on {listen}", kind))?;
    println!("confidential-agentd {:?} API listening on {listen}", kind);
    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let limits = limits.clone();
                    if let Err(err) = handle_accepted_http_stream(stream, kind, limits) {
                        eprintln!("daemon {:?} API accepted connection failed: {err:#}", kind);
                    }
                }
                Err(err) => eprintln!("daemon {:?} API accept failed: {err}", kind),
            }
        }
    });
    Ok(())
}

enum HttpAcceptOutcome {
    Spawned(thread::JoinHandle<()>),
    Rejected,
}

fn handle_accepted_http_stream(
    stream: TcpStream,
    kind: HttpServerKind,
    limits: Arc<HttpServerLimits>,
) -> Result<HttpAcceptOutcome> {
    configure_http_stream(&stream)?;
    let peer = stream
        .peer_addr()
        .context("failed to resolve daemon API peer address")?
        .ip();

    if !limits.allow_peer(peer) {
        write_http_response(
            stream,
            "429 Too Many Requests",
            "application/json",
            r#"{"error":"too many requests"}"#,
        )?;
        return Ok(HttpAcceptOutcome::Rejected);
    }

    let Some(active_request) = limits.try_acquire_request() else {
        write_http_response(
            stream,
            "503 Service Unavailable",
            "application/json",
            r#"{"error":"too many active requests"}"#,
        )?;
        return Ok(HttpAcceptOutcome::Rejected);
    };

    let handle = thread::Builder::new()
        .name(format!("confidential-agentd-{kind:?}-request"))
        .spawn(move || {
            if let Err(err) = handle_http_request(stream, kind, active_request) {
                eprintln!("daemon {:?} API request failed: {err:#}", kind);
            }
        })
        .context("failed to spawn daemon API request thread")?;
    Ok(HttpAcceptOutcome::Spawned(handle))
}

fn configure_http_stream(stream: &TcpStream) -> Result<()> {
    stream
        .set_read_timeout(Some(HTTP_IO_TIMEOUT))
        .context("failed to set daemon API read timeout")?;
    stream
        .set_write_timeout(Some(HTTP_IO_TIMEOUT))
        .context("failed to set daemon API write timeout")?;
    Ok(())
}

#[derive(Debug)]
struct HttpRequestHead {
    method: String,
    path: String,
}

#[derive(Debug)]
enum HttpRequestError {
    TooLarge,
    BadRequest,
    PayloadTooLarge,
    Io(std::io::Error),
}

fn handle_http_request(
    mut stream: TcpStream,
    kind: HttpServerKind,
    active_request: ActiveRequestGuard,
) -> Result<()> {
    let _active_request = active_request;

    let request = match read_http_request_head(&mut stream) {
        Ok(request) => request,
        Err(HttpRequestError::TooLarge) => {
            return write_http_response(
                stream,
                "431 Request Header Fields Too Large",
                "application/json",
                r#"{"error":"request headers too large"}"#,
            );
        }
        Err(HttpRequestError::PayloadTooLarge) => {
            return write_http_response(
                stream,
                "413 Payload Too Large",
                "application/json",
                r#"{"error":"request body not allowed"}"#,
            );
        }
        Err(HttpRequestError::BadRequest) => {
            return write_http_response(
                stream,
                "400 Bad Request",
                "application/json",
                r#"{"error":"bad request"}"#,
            );
        }
        Err(HttpRequestError::Io(err)) => {
            return Err(err).context("failed to read daemon API request");
        }
    };

    if request.method != "GET" {
        return write_http_response_with_headers(
            stream,
            "405 Method Not Allowed",
            "application/json",
            &[("Allow", "GET")],
            r#"{"error":"method not allowed"}"#,
        );
    }

    match kind {
        HttpServerKind::Status => match request.path.as_str() {
            "/status" => serve_status_file(stream),
            "/health" => {
                write_http_response(stream, "200 OK", "application/json", r#"{"status":"ok"}"#)
            }
            _ => write_http_response(
                stream,
                "404 Not Found",
                "application/json",
                r#"{"error":"not found"}"#,
            ),
        },
        HttpServerKind::AgentCard => match request.path.as_str() {
            "/.well-known/agent-card.json" => serve_agent_card(stream),
            "/health" => {
                write_http_response(stream, "200 OK", "application/json", r#"{"status":"ok"}"#)
            }
            _ => write_http_response(
                stream,
                "404 Not Found",
                "application/json",
                r#"{"error":"not found"}"#,
            ),
        },
    }
}

fn read_http_request_head(
    stream: &mut TcpStream,
) -> std::result::Result<HttpRequestHead, HttpRequestError> {
    let mut request = Vec::with_capacity(HTTP_READ_CHUNK_BYTES);
    let mut chunk = [0u8; HTTP_READ_CHUNK_BYTES];

    loop {
        if let Some(header_end) = find_http_header_end(&request) {
            return parse_http_request_head(&request[..header_end]);
        }
        if request.len() >= HTTP_MAX_HEADER_BYTES {
            return Err(HttpRequestError::TooLarge);
        }

        let read_len = (HTTP_MAX_HEADER_BYTES - request.len()).min(HTTP_READ_CHUNK_BYTES);
        match stream.read(&mut chunk[..read_len]) {
            Ok(0) => return Err(HttpRequestError::BadRequest),
            Ok(read) => request.extend_from_slice(&chunk[..read]),
            Err(err) => return Err(HttpRequestError::Io(err)),
        }
    }
}

fn find_http_header_end(request: &[u8]) -> Option<usize> {
    request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

fn parse_http_request_head(
    request: &[u8],
) -> std::result::Result<HttpRequestHead, HttpRequestError> {
    let request = std::str::from_utf8(request).map_err(|_| HttpRequestError::BadRequest)?;
    let request = request
        .strip_suffix("\r\n\r\n")
        .ok_or(HttpRequestError::BadRequest)?;
    let mut lines = request.split("\r\n");
    let first_line = lines.next().ok_or(HttpRequestError::BadRequest)?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next().ok_or(HttpRequestError::BadRequest)?;
    let path = parts.next().ok_or(HttpRequestError::BadRequest)?;
    let version = parts.next().ok_or(HttpRequestError::BadRequest)?;
    if parts.next().is_some() || !version.starts_with("HTTP/") || !path.starts_with('/') {
        return Err(HttpRequestError::BadRequest);
    }

    let method = method.to_string();
    let path = path.to_string();
    let mut content_length = 0u64;
    let mut has_transfer_encoding = false;

    for line in lines {
        if line.is_empty() {
            return Err(HttpRequestError::BadRequest);
        }
        let (name, value) = line.split_once(':').ok_or(HttpRequestError::BadRequest)?;
        let name = name.trim();
        if name.is_empty() {
            return Err(HttpRequestError::BadRequest);
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            has_transfer_encoding = true;
        }
        if name.eq_ignore_ascii_case("content-length") {
            let parsed = value
                .trim()
                .parse()
                .map_err(|_| HttpRequestError::BadRequest)?;
            content_length = content_length.max(parsed);
        }
    }

    if has_transfer_encoding {
        return Err(HttpRequestError::BadRequest);
    }
    if content_length > 0 {
        return Err(HttpRequestError::PayloadTooLarge);
    }

    Ok(HttpRequestHead { method, path })
}

fn serve_status_file(stream: TcpStream) -> Result<()> {
    let path = daemon_status_path();
    match fs::read_to_string(&path) {
        Ok(body) => write_http_response(stream, "200 OK", "application/json", &body),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => write_http_response(
            stream,
            "503 Service Unavailable",
            "application/json",
            r#"{"error":"daemon status is not ready"}"#,
        ),
        Err(err) => Err(err).with_context(|| format!("failed to read '{}'", path.display())),
    }
}

fn serve_agent_card(stream: TcpStream) -> Result<()> {
    let path = agent_card_path();
    match fs::read_to_string(&path) {
        Ok(body) => write_http_response(stream, "200 OK", "application/json", &body),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => write_http_response(
            stream,
            "404 Not Found",
            "application/json",
            r#"{"error":"agent card not configured"}"#,
        ),
        Err(err) => Err(err).with_context(|| format!("failed to read '{}'", path.display())),
    }
}

fn write_http_response(
    stream: TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> Result<()> {
    write_http_response_with_headers(stream, status, content_type, &[], body)
}

fn write_http_response_with_headers(
    mut stream: TcpStream,
    status: &str,
    content_type: &str,
    extra_headers: &[(&str, &str)],
    body: &str,
) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n"
    )
    .context("failed to write daemon API response")?;
    for (name, value) in extra_headers {
        write!(stream, "{name}: {value}\r\n").context("failed to write daemon API response")?;
    }
    write!(
        stream,
        "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .context("failed to write daemon API response")
}

fn bootstrap_resources_ready(bootstrap: &BootstrapConfig, state: &DaemonState) -> Result<bool> {
    if state.bootstrap_generation != bootstrap.generation {
        return Ok(false);
    }
    for resource in &bootstrap.resources {
        let Some(applied) = state.applied_resources.get(&resource.id) else {
            return Ok(false);
        };
        if applied.target != resource.target
            || applied.mode != resource.mode
            || applied.owner != resource.owner
            || applied.group != resource.group
            || applied.mutable != resource.mutable
        {
            return Ok(false);
        }
        if !resource.mutable {
            if let Some(expected) = &resource.sha256 {
                if applied.sha256 != *expected {
                    return Ok(false);
                }
            }
            if !resource_target_matches(resource, &applied.sha256)? {
                return Ok(false);
            }
        } else if !resource_target_metadata_matches(resource)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn read_bootstrap(args: &RunArgs, runtime_mode: RuntimeMode) -> Result<Option<BootstrapConfig>> {
    let path = args.cdh_root.join(&args.bootstrap_resource);
    if !path.exists() || path.metadata()?.len() == 0 {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read bootstrap '{}'", path.display()))?;
    let bootstrap: BootstrapConfig =
        serde_json::from_str(&content).context("failed to parse bootstrap config")?;
    validate_bootstrap(&bootstrap, runtime_mode)?;
    Ok(Some(bootstrap))
}

fn validate_bootstrap(bootstrap: &BootstrapConfig, runtime_mode: RuntimeMode) -> Result<()> {
    if bootstrap.schema != BOOTSTRAP_SCHEMA_VERSION {
        bail!(
            "unsupported bootstrap schema '{}'; expected '{}'",
            bootstrap.schema,
            BOOTSTRAP_SCHEMA_VERSION
        );
    }
    if bootstrap.service_id.trim().is_empty() {
        bail!("bootstrap service_id must not be empty");
    }
    let expected_mode = match runtime_mode {
        RuntimeMode::Challenge => "challenge",
        RuntimeMode::Trustee => "trustee",
    };
    if bootstrap.mode != expected_mode {
        bail!(
            "bootstrap mode '{}' does not match runtime mode '{}'",
            bootstrap.mode,
            expected_mode
        );
    }
    Ok(())
}

fn apply_bootstrap(
    args: &RunArgs,
    bootstrap: &BootstrapConfig,
    state: &mut DaemonState,
    fail_missing: bool,
) -> Result<bool> {
    struct PlannedResource<'a> {
        resource: &'a GuestResource,
        source: PathBuf,
        digest: String,
        update: ResourceUpdate,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ResourceUpdate {
        None,
        WriteContent,
        MetadataOnly,
    }

    let mut missing_required = Vec::new();
    let mut planned_resources = Vec::new();
    let resource_ids = bootstrap
        .resources
        .iter()
        .map(|resource| resource.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    state
        .applied_resources
        .retain(|resource_id, _| resource_ids.contains(resource_id));
    for resource in &bootstrap.resources {
        let source = args.cdh_root.join(&resource.resource_path);
        let metadata = match source.metadata() {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if fail_missing || resource.required {
                    missing_required.push(resource.id.clone());
                }
                eprintln!(
                    "waiting for resource '{}' at '{}'",
                    resource.id,
                    source.display()
                );
                continue;
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to stat resource '{}'", source.display()));
            }
        };
        if !metadata.is_file() {
            bail!(
                "resource '{}' at '{}' is not a regular file",
                resource.id,
                source.display()
            );
        }
        if metadata.len() == 0 {
            if fail_missing || resource.required {
                missing_required.push(resource.id.clone());
            }
            eprintln!(
                "waiting for resource '{}' at '{}'",
                resource.id,
                source.display()
            );
            continue;
        }
        if metadata.len() > MAX_RESOURCE_BYTES {
            bail!(
                "resource '{}' at '{}' is {} bytes, exceeding maximum {} bytes",
                resource.id,
                source.display(),
                metadata.len(),
                MAX_RESOURCE_BYTES
            );
        }

        let digest = sha256_file(&source)?;
        if let Some(expected) = &resource.sha256 {
            if expected != &digest {
                bail!(
                    "resource '{}' digest mismatch: expected {}, got {}",
                    resource.id,
                    expected,
                    digest
                );
            }
        }
        let update = if resource.mutable {
            match state.applied_resources.get(&resource.id) {
                None => ResourceUpdate::WriteContent,
                Some(_) if !resource.target.exists() => ResourceUpdate::WriteContent,
                Some(_) if !resource_target_metadata_matches(resource)? => {
                    ResourceUpdate::MetadataOnly
                }
                Some(_) => ResourceUpdate::None,
            }
        } else if resource_target_matches(resource, &digest)? {
            ResourceUpdate::None
        } else {
            ResourceUpdate::WriteContent
        };
        planned_resources.push(PlannedResource {
            resource,
            source,
            digest,
            update,
        });
    }

    if !missing_required.is_empty() {
        let mut status_state = state.clone();
        status_state.bootstrap_generation = bootstrap.generation;
        write_status(status_for(
            bootstrap,
            &status_state,
            "waiting-resources",
            RuntimeReadiness {
                app_ready: false,
                mesh_ready: false,
                debug_ssh_ready: ensure_debug_ssh_ready(),
            },
        ))?;
        if fail_missing {
            bail!(
                "missing required resources: {}",
                missing_required.join(", ")
            );
        }
        return Ok(false);
    }

    let app_service_prepared = if planned_resources
        .iter()
        .any(|planned| planned.update == ResourceUpdate::WriteContent)
    {
        prepare_app_service_for_resource_apply(bootstrap)?
    } else {
        false
    };
    let apply_result = (|| -> Result<()> {
        for planned in planned_resources {
            let outcome = match planned.update {
                ResourceUpdate::None => ApplyOutcome::Unchanged,
                ResourceUpdate::WriteContent => {
                    apply_resource_once(planned.resource, &planned.source, &planned.digest)?
                }
                ResourceUpdate::MetadataOnly => {
                    apply_resource_metadata_for_resource(planned.resource)?;
                    ApplyOutcome::Updated
                }
            };
            if outcome == ApplyOutcome::Updated {
                println!(
                    "applied resource '{}' to '{}'",
                    planned.resource.id,
                    planned.resource.target.display()
                );
            }
            let recorded_digest = if planned.resource.mutable {
                match planned.update {
                    ResourceUpdate::None => state
                        .applied_resources
                        .get(&planned.resource.id)
                        .map(|applied| applied.sha256.clone())
                        .unwrap_or_else(|| planned.digest.clone()),
                    ResourceUpdate::WriteContent => {
                        // For mutable resources this digest is status/audit data only;
                        // readiness intentionally validates metadata, not content.
                        sha256_file(&planned.resource.target)?
                    }
                    ResourceUpdate::MetadataOnly => state
                        .applied_resources
                        .get(&planned.resource.id)
                        .map(|applied| applied.sha256.clone())
                        .unwrap_or_else(|| planned.digest.clone()),
                }
            } else {
                planned.digest.clone()
            };
            state.applied_resources.insert(
                planned.resource.id.clone(),
                applied_resource_state(planned.resource, &recorded_digest),
            );
        }
        Ok(())
    })();
    if let Err(err) = apply_result {
        if app_service_prepared {
            restore_app_service_after_resource_apply_failure(bootstrap);
        }
        return Err(err);
    }

    state.bootstrap_generation = bootstrap.generation;
    let readiness = ensure_runtime_ready(bootstrap, false);
    write_status(status_for(bootstrap, state, "resources-applied", readiness))?;

    // Write agent card file for the status server to serve
    if let Some(card) = &bootstrap.agent_card {
        let path = agent_card_path();
        let card_dir = path.parent().context("agent card path has no parent")?;
        fs::create_dir_all(card_dir)
            .with_context(|| format!("failed to create '{}'", card_dir.display()))?;
        let card_json = serde_json::to_string_pretty(card)?;
        fs::write(&path, card_json)
            .with_context(|| format!("failed to write '{}'", path.display()))?;
        println!("agent card written to {}", path.display());
    }

    Ok(true)
}

fn applied_resource_state(resource: &GuestResource, digest: &str) -> AppliedResourceState {
    AppliedResourceState {
        sha256: digest.to_string(),
        target: resource.target.clone(),
        owner: resource.owner.clone(),
        group: resource.group.clone(),
        mode: resource.mode.clone(),
        mutable: resource.mutable,
    }
}

fn resource_target_matches(resource: &GuestResource, expected_sha256: &str) -> Result<bool> {
    validate_resource_target_path(&resource.target)?;
    if !resource.target.exists() {
        return Ok(false);
    }
    if sha256_file(&resource.target)? != expected_sha256 {
        return Ok(false);
    }
    let desired_mode = parse_mode(&resource.mode)?;
    let desired_uid = match resource.owner.as_deref() {
        Some(owner) => Some(resolve_user_id(owner)?),
        None => None,
    };
    let desired_gid = match resource.group.as_deref() {
        Some(group) => Some(resolve_group_id(group)?),
        None => None,
    };
    resource_metadata_matches(&resource.target, desired_mode, desired_uid, desired_gid)
}

fn resource_target_metadata_matches(resource: &GuestResource) -> Result<bool> {
    validate_resource_target_path(&resource.target)?;
    if !resource.target.exists() {
        return Ok(false);
    }
    let desired_mode = parse_mode(&resource.mode)?;
    let desired_uid = match resource.owner.as_deref() {
        Some(owner) => Some(resolve_user_id(owner)?),
        None => None,
    };
    let desired_gid = match resource.group.as_deref() {
        Some(group) => Some(resolve_group_id(group)?),
        None => None,
    };
    resource_metadata_matches(&resource.target, desired_mode, desired_uid, desired_gid)
}

fn apply_resource_metadata_for_resource(resource: &GuestResource) -> Result<()> {
    validate_resource_target_path(&resource.target)?;
    let desired_mode = parse_mode(&resource.mode)?;
    let desired_uid = match resource.owner.as_deref() {
        Some(owner) => Some(resolve_user_id(owner)?),
        None => None,
    };
    let desired_gid = match resource.group.as_deref() {
        Some(group) => Some(resolve_group_id(group)?),
        None => None,
    };
    apply_resource_metadata(&resource.target, desired_mode, desired_uid, desired_gid)
}

fn apply_resource_once(
    resource: &GuestResource,
    source: &Path,
    source_sha256: &str,
) -> Result<ApplyOutcome> {
    validate_resource_target_path(&resource.target)?;
    let desired_mode = parse_mode(&resource.mode)?;
    let desired_uid = match resource.owner.as_deref() {
        Some(owner) => Some(resolve_user_id(owner)?),
        None => None,
    };
    let desired_gid = match resource.group.as_deref() {
        Some(group) => Some(resolve_group_id(group)?),
        None => None,
    };
    let target_metadata = match fs::symlink_metadata(&resource.target) {
        Ok(metadata) => Some(metadata),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "failed to stat resource target '{}'",
                    resource.target.display()
                )
            });
        }
    };
    if matches!(target_metadata.as_ref(), Some(metadata) if metadata.is_file())
        && sha256_file(&resource.target)? == source_sha256
    {
        if !resource_metadata_matches(&resource.target, desired_mode, desired_uid, desired_gid)? {
            apply_resource_metadata(&resource.target, desired_mode, desired_uid, desired_gid)?;
            return Ok(ApplyOutcome::Updated);
        }
        return Ok(ApplyOutcome::Unchanged);
    }

    let parent = resource
        .target
        .parent()
        .with_context(|| format!("target '{}' has no parent", resource.target.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create '{}'", parent.display()))?;

    let tmp = resource.target.with_extension("confidential-agent.tmp");
    reject_symlink_path_components(&tmp)
        .with_context(|| format!("invalid resource temporary path '{}'", tmp.display()))?;
    match fs::symlink_metadata(&tmp) {
        Ok(_) => {
            fs::remove_file(&tmp)
                .with_context(|| format!("failed to remove stale '{}'", tmp.display()))?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| format!("failed to stat '{}'", tmp.display()));
        }
    }
    if let Err(err) = copy_resource_to_new_file(source, &tmp) {
        let _ = fs::remove_file(&tmp);
        return Err(err).with_context(|| {
            format!(
                "failed to copy resource '{}' to '{}'",
                source.display(),
                resource.target.display()
            )
        });
    }
    apply_resource_metadata(&tmp, desired_mode, desired_uid, desired_gid)?;
    fs::rename(&tmp, &resource.target)
        .with_context(|| format!("failed to replace '{}'", resource.target.display()))?;
    Ok(ApplyOutcome::Updated)
}

fn copy_resource_to_new_file(source: &Path, target: &Path) -> Result<()> {
    let mut source_file =
        fs::File::open(source).with_context(|| format!("failed to open '{}'", source.display()))?;
    let mut target_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(target)
        .with_context(|| format!("failed to create '{}'", target.display()))?;
    std::io::copy(&mut source_file, &mut target_file).with_context(|| {
        format!(
            "failed to copy '{}' into '{}'",
            source.display(),
            target.display()
        )
    })?;
    target_file
        .sync_all()
        .with_context(|| format!("failed to sync '{}'", target.display()))?;
    Ok(())
}

fn validate_resource_target_path(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        bail!(
            "resource target '{}' must be an absolute path",
            path.display()
        );
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!(
            "resource target '{}' must not contain parent-directory components",
            path.display()
        );
    }
    reject_symlink_path_components(path)
        .with_context(|| format!("invalid resource target '{}'", path.display()))
}

fn reject_symlink_path_components(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("path component '{}' is a symlink", current.display());
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
            Err(err) => {
                return Err(err).with_context(|| format!("failed to stat '{}'", current.display()));
            }
        }
    }
    Ok(())
}

fn resource_metadata_matches(
    path: &Path,
    desired_mode: u32,
    desired_uid: Option<u32>,
    desired_gid: Option<u32>,
) -> Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Ok(false);
    }
    if metadata.permissions().mode() & 0o777 != desired_mode {
        return Ok(false);
    }
    if let Some(uid) = desired_uid {
        if metadata.uid() != uid {
            return Ok(false);
        }
    }
    if let Some(gid) = desired_gid {
        if metadata.gid() != gid {
            return Ok(false);
        }
    }
    Ok(true)
}

fn apply_resource_metadata(
    path: &Path,
    desired_mode: u32,
    desired_uid: Option<u32>,
    desired_gid: Option<u32>,
) -> Result<()> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("failed to open '{}' for metadata update", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to stat '{}' for metadata update", path.display()))?;
    if !metadata.is_file() {
        bail!("resource target '{}' is not a regular file", path.display());
    }
    let fd = file.as_raw_fd();
    if desired_uid.is_some() || desired_gid.is_some() {
        let uid = desired_uid
            .map(|uid| uid as libc::uid_t)
            .unwrap_or(!0 as libc::uid_t);
        let gid = desired_gid
            .map(|gid| gid as libc::gid_t)
            .unwrap_or(!0 as libc::gid_t);
        let rc = unsafe { libc::fchown(fd, uid, gid) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("failed to fchown '{}'", path.display()));
        }
    }
    let rc = unsafe { libc::fchmod(fd, desired_mode as libc::mode_t) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to fchmod '{}'", path.display()));
    }
    Ok(())
}

fn resolve_user_id(owner: &str) -> Result<u32> {
    if let Ok(uid) = owner.parse::<u32>() {
        return Ok(uid);
    }
    resolve_name_id(Path::new("/etc/passwd"), owner, 2)
        .with_context(|| format!("failed to resolve resource owner '{owner}'"))
}

fn resolve_group_id(group: &str) -> Result<u32> {
    if let Ok(gid) = group.parse::<u32>() {
        return Ok(gid);
    }
    resolve_name_id(Path::new("/etc/group"), group, 2)
        .with_context(|| format!("failed to resolve resource group '{group}'"))
}

fn resolve_name_id(path: &Path, name: &str, id_field: usize) -> Result<u32> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read '{}'", path.display()))?;
    for line in content.lines() {
        let mut fields = line.split(':');
        let Some(entry_name) = fields.next() else {
            continue;
        };
        if entry_name != name {
            continue;
        }
        let Some(id) = fields.nth(id_field.saturating_sub(1)) else {
            break;
        };
        return id
            .parse::<u32>()
            .with_context(|| format!("invalid id '{}' for '{}'", id, name));
    }
    bail!("'{}' not found in '{}'", name, path.display())
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn sync_mesh(args: &RunArgs, bootstrap: &BootstrapConfig, state: &mut DaemonState) -> Result<()> {
    let bundle_path = args.cdh_root.join(&args.mesh_resource);
    let has_bundle =
        bundle_path.exists() && bundle_path.metadata().map(|m| m.len() > 0).unwrap_or(false);
    let a2a_bundle = read_a2a_bundle(args)?;
    let has_a2a_peers = a2a_bundle
        .as_ref()
        .map(|bundle| !bundle.peers.is_empty())
        .unwrap_or(false);
    if a2a_bundle.is_none() {
        state.a2a_cache.clear();
        state.a2a_status.clear();
        state.last_error = None;
    }

    if !has_bundle && !has_a2a_peers {
        state.mesh_generation = 0;
        let config = empty_tng_config();
        let directory = ServiceDirectory {
            schema: SERVICE_DIRECTORY_SCHEMA_VERSION.to_string(),
            services: BTreeMap::new(),
        };
        write_service_directory(&directory)?;
        let gateway_cfg = empty_gateway_config(bootstrap)?;
        sync_gateway_service(&gateway_cfg, state)?;
        let _ = sync_tng_service(&config, state, &[])?;
        let readiness = ensure_runtime_ready(bootstrap, false);
        write_status(status_for(bootstrap, state, "waiting-mesh", readiness))?;
        return Ok(());
    }

    let (mut config, mut directory, gateway_cfg) = if has_bundle {
        let bundle_content = fs::read_to_string(&bundle_path)
            .with_context(|| format!("failed to read mesh bundle '{}'", bundle_path.display()))?;
        let bundle: MeshBundle =
            serde_json::from_str(&bundle_content).context("invalid mesh bundle JSON")?;
        if bundle.schema != MESH_SCHEMA_VERSION {
            bail!(
                "unsupported mesh bundle schema '{}'; expected '{}'",
                bundle.schema,
                MESH_SCHEMA_VERSION
            );
        }
        let cache_dir = daemon_cache_dir();
        fs::create_dir_all(&cache_dir)?;
        fs::write(
            cache_dir.join("mesh-bundle.json"),
            serde_json::to_string_pretty(&bundle)?,
        )?;
        let plan = runtime_port_plan(&bundle, &bootstrap.service_id)?;
        let dir = service_directory(&bundle, &bootstrap.service_id, &plan);
        let cfg = tng_config(&bundle, &bootstrap.service_id, &plan)?;
        let gateway_cfg = gateway_config(&bundle, &bootstrap.service_id, bootstrap, &plan)?;
        state.mesh_generation = bundle.generation;
        (cfg, dir, gateway_cfg)
    } else {
        // No mesh bundle; start with empty config that has egress for self ports
        state.mesh_generation = 0;
        let confidential_port_set = confidential_ports(&bootstrap.ports, &bootstrap.connect)
            .into_iter()
            .collect::<BTreeSet<_>>();
        let connect_encryption: BTreeMap<u16, EncryptionMode> = bootstrap
            .connect
            .iter()
            .map(|cp| (cp.port(), cp.encryption()))
            .collect();
        let egress = bootstrap
            .ports
            .iter()
            .enumerate()
            .map(|(idx, port)| {
                let encryption = connect_encryption
                    .get(port)
                    .cloned()
                    .unwrap_or(EncryptionMode::RatsTls);
                let mut route = tng_egress_protocol(encryption);
                let obj = route.as_object_mut().expect("protocol config is an object");
                obj.insert("netfilter".into(), json!({
                    "capture_dst": { "port": port },
                    "capture_local_traffic": false,
                    "listen_port": 39000 + idx as u16,
                }));
                obj.insert("attest".into(), tng_attest_config());
                if confidential_port_set.contains(port) {
                    obj.insert("verify".into(), tng_verify_config(Value::Array(Vec::new())));
                }
                route
            })
            .collect::<Vec<_>>();
        let mut cfg = empty_tng_config();
        cfg["add_egress"] = Value::Array(egress);
        let dir = ServiceDirectory {
            schema: SERVICE_DIRECTORY_SCHEMA_VERSION.to_string(),
            services: BTreeMap::new(),
        };
        (cfg, dir, empty_gateway_config(bootstrap)?)
    };

    if let Some(a2a_bundle) = a2a_bundle.as_ref() {
        let (peer_ingress, peer_directory) = a2a_tng_ingress(
            a2a_bundle,
            &bootstrap.service_id,
            &bootstrap.ports,
            &directory,
            state,
        );
        if let Some(ingress_arr) = config.get_mut("add_ingress").and_then(|v| v.as_array_mut()) {
            ingress_arr.extend(peer_ingress);
        }
        for (id, service) in peer_directory {
            directory.services.insert(id, service);
        }
    }

    write_service_directory(&directory)?;
    sync_gateway_service(&gateway_cfg, state)?;
    let mesh_ready = sync_tng_service(&config, state, &service_directory_local_ports(&directory))?;

    let readiness = ensure_runtime_ready(bootstrap, mesh_ready);
    let phase = if !readiness.app_ready {
        "starting-app"
    } else if !readiness.mesh_ready {
        "starting-mesh"
    } else {
        "running"
    };
    write_status(status_for(bootstrap, state, phase, readiness))?;

    Ok(())
}

fn empty_tng_config() -> Value {
    json!({
        "control_interface": { "restful": { "host": "127.0.0.1", "port": TNG_CONTROL_PORT } },
        "add_egress": [],
        "add_ingress": [],
    })
}

fn write_service_directory(directory: &ServiceDirectory) -> Result<()> {
    let directory_path = service_directory_path();
    if let Some(parent) = directory_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&directory_path, serde_json::to_string_pretty(directory)?)?;
    Ok(())
}

fn sync_tng_service(config: &Value, state: &mut DaemonState, ready_ports: &[u16]) -> Result<bool> {
    let fingerprint = sha256_bytes(serde_json::to_vec(config)?.as_slice());
    let tng_path = tng_config_path();
    let changed = write_json_if_changed(&tng_path, config)?;
    let mut restarted = false;
    if changed || state.mesh_started_fingerprint.as_deref() != Some(&fingerprint) {
        restart_service(TNG_SERVICE)?;
        state.mesh_started_fingerprint = Some(fingerprint.clone());
        state.mesh_last_restart_unix = Some(now_unix());
        restarted = true;
    }

    let mesh_ready = ensure_tng_service_active(restarted, &mut state.mesh_last_restart_unix)
        && mesh_ports_ready(ready_ports);
    if mesh_ready {
        state.mesh_fingerprint = Some(fingerprint);
    } else {
        state.mesh_fingerprint = None;
    }
    Ok(mesh_ready)
}

#[derive(Debug, Clone)]
struct RuntimePortPlan {
    self_routes: Vec<SelfGatewayRoute>,
    peer_routes: Vec<PeerGatewayRoute>,
}

#[derive(Debug, Clone)]
struct SelfGatewayRoute {
    port: u16,
    tng_egress_port: u16,
    gateway_port: u16,
    protocol: String,
}

#[derive(Debug, Clone)]
struct PeerGatewayRoute {
    service_id: String,
    host: String,
    remote_port: u16,
    local_port: u16,
    tng_ingress_port: u16,
    protocol: String,
    connect_mode: bool,
}

fn runtime_port_plan(bundle: &MeshBundle, self_id: &str) -> Result<RuntimePortPlan> {
    let service = bundle
        .services
        .get(self_id)
        .with_context(|| format!("service '{self_id}' is not present in mesh bundle"))?;
    if service.phase != "active" {
        bail!("service '{self_id}' is not active in mesh bundle");
    }

    let mut used = BTreeSet::from([TNG_CONTROL_PORT, DAEMON_STATUS_PORT, AGENT_CARD_PORT]);
    for service in bundle
        .services
        .values()
        .filter(|service| service.phase == "active")
    {
        used.extend(service.ports.iter().copied());
    }

    let self_mcp_ports = service.mcp_ports.iter().copied().collect::<BTreeSet<_>>();
    let mut self_ports = service.ports.clone();
    self_ports.sort_unstable();
    self_ports.dedup();
    let mut self_routes = Vec::new();
    for port in self_ports {
        self_routes.push(SelfGatewayRoute {
            port,
            tng_egress_port: allocate_internal_port(TNG_EGRESS_PORT_BASE, &mut used)?,
            gateway_port: allocate_internal_port(GATEWAY_SERVER_PORT_BASE, &mut used)?,
            protocol: if self_mcp_ports.contains(&port) {
                "mcp".to_string()
            } else {
                "raw".to_string()
            },
        });
    }

    let mut peer_routes = Vec::new();
    let mut peers = bundle
        .services
        .iter()
        .filter(|(id, service)| id.as_str() != self_id && service.phase == "active")
        .collect::<Vec<_>>();
    peers.sort_by(|(left, _), (right, _)| left.cmp(right));
    for (service_id, service) in peers {
        let host = service
            .public_ip
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                service
                    .private_ip
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
            })
            .with_context(|| format!("active peer service '{service_id}' has no reachable IP"))?
            .to_string();
        let connect_ports = service.connect.iter().map(|cp| cp.port()).collect::<BTreeSet<_>>();
        let mcp_ports = service.mcp_ports.iter().copied().collect::<BTreeSet<_>>();
        let mut ports = service.ports.clone();
        ports.sort_unstable();
        ports.dedup();
        for port in ports {
            peer_routes.push(PeerGatewayRoute {
                service_id: service_id.clone(),
                host: host.clone(),
                remote_port: port,
                local_port: port,
                tng_ingress_port: allocate_internal_port(TNG_INGRESS_PORT_BASE, &mut used)?,
                protocol: if mcp_ports.contains(&port) {
                    "mcp".to_string()
                } else {
                    "raw".to_string()
                },
                connect_mode: connect_ports.contains(&port),
            });
        }
    }

    Ok(RuntimePortPlan {
        self_routes,
        peer_routes,
    })
}

fn allocate_internal_port(base: u16, used: &mut BTreeSet<u16>) -> Result<u16> {
    for port in base..=60999 {
        if used.insert(port) {
            return Ok(port);
        }
    }
    bail!("no available internal gateway/TNG port from {base}")
}

fn service_directory(
    bundle: &MeshBundle,
    self_id: &str,
    plan: &RuntimePortPlan,
) -> ServiceDirectory {
    let mut services = BTreeMap::new();
    for (id, service) in &bundle.services {
        if id == self_id || service.phase != "active" {
            continue;
        }
        let ports = plan
            .peer_routes
            .iter()
            .filter(|route| route.service_id == *id)
            .map(|route| ServiceDirectoryPort {
                address: "127.0.0.1".to_string(),
                port: if route.connect_mode {
                    route.tng_ingress_port
                } else {
                    route.local_port
                },
                mode: Some(if route.connect_mode {
                    "connect".to_string()
                } else {
                    "mesh".to_string()
                }),
                protocol: if route.connect_mode {
                    None
                } else {
                    Some(route.protocol.clone())
                },
            })
            .collect::<Vec<_>>();
        if ports.is_empty() {
            continue;
        }
        services.insert(id.clone(), ServiceDirectoryService { ports });
    }

    ServiceDirectory {
        schema: SERVICE_DIRECTORY_SCHEMA_VERSION.to_string(),
        services,
    }
}

fn service_directory_local_ports(directory: &ServiceDirectory) -> Vec<u16> {
    let mut ports = directory
        .services
        .values()
        .flat_map(|service| service.ports.iter().map(|port| port.port))
        .collect::<Vec<_>>();
    ports.sort_unstable();
    ports.dedup();
    ports
}

fn tng_attest_config() -> Value {
    json!({
        "model": "background_check",
        "aa_type": "uds",
        "aa_addr": DEFAULT_AA_SOCK,
    })
}

fn tng_egress_protocol(encryption: EncryptionMode) -> Value {
    match encryption {
        EncryptionMode::RatsTls => json!({"rats_tls": {"multiplex": false}}),
        EncryptionMode::Ohttp => json!({"ohttp": {"key": {"source": "self_generated"}}}),
    }
}

fn tng_verify_config(reference_values: Value) -> Value {
    json!({
        "as_type": "builtin",
        "attestation_policy": {
            "type": "path",
            "path": DEFAULT_POLICY_PATH,
        },
        "reference_values": reference_values,
    })
}

fn mesh_peer_reference_values(bundle: &MeshBundle, self_id: &str) -> Result<Value> {
    let mut values = Vec::new();
    let mut peers = bundle
        .services
        .iter()
        .filter(|(id, service)| id.as_str() != self_id && service.phase == "active")
        .collect::<Vec<_>>();
    peers.sort_by(|(left, _), (right, _)| left.cmp(right));
    for (service_id, _) in peers {
        let peer_values = tng_reference_values(bundle, service_id)?;
        if let Value::Array(peer_values) = peer_values {
            values.extend(peer_values);
        } else {
            values.push(peer_values);
        }
    }
    Ok(Value::Array(values))
}

fn tng_config(bundle: &MeshBundle, self_id: &str, plan: &RuntimePortPlan) -> Result<Value> {
    let mut egress = Vec::new();
    let service = bundle
        .services
        .get(self_id)
        .with_context(|| format!("service '{self_id}' is not present in mesh bundle"))?;
    if service.phase != "active" {
        bail!("service '{self_id}' is not active in mesh bundle");
    }
    let confidential_port_set = confidential_ports(&service.ports, &service.connect)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let connect_encryption: BTreeMap<u16, EncryptionMode> = service
        .connect
        .iter()
        .map(|cp| (cp.port(), cp.encryption()))
        .collect();
    let peer_reference_values = if confidential_port_set.is_empty() {
        None
    } else {
        Some(mesh_peer_reference_values(bundle, self_id)?)
    };
    for route_plan in &plan.self_routes {
        let encryption = connect_encryption
            .get(&route_plan.port)
            .cloned()
            .unwrap_or(EncryptionMode::RatsTls);
        let mut route = tng_egress_protocol(encryption);
        let obj = route.as_object_mut().expect("protocol config is an object");
        obj.insert("netfilter".into(), json!({
            "capture_dst": {"port": route_plan.port},
            "capture_local_traffic": false,
            "listen_port": route_plan.tng_egress_port,
            "so_mark": DEFAULT_TNG_SO_MARK,
        }));
        obj.insert("attest".into(), tng_attest_config());
        if confidential_port_set.contains(&route_plan.port) {
            obj.insert("verify".into(), tng_verify_config(
                peer_reference_values
                    .as_ref()
                    .cloned()
                    .unwrap_or_else(|| Value::Array(Vec::new())),
            ));
        }
        egress.push(route);
    }

    let mut ingress = Vec::new();
    for route_plan in &plan.peer_routes {
        let reference_values = tng_reference_values(bundle, &route_plan.service_id)?;
        let mut entry = json!({
            "rats_tls": {
                "multiplex": true,
            },
            "mapping": {
                "in": {
                    "host": "127.0.0.1",
                    "port": route_plan.tng_ingress_port,
                },
                "out": {
                    "host": route_plan.host,
                    "port": route_plan.remote_port,
                },
            },
            "verify": tng_verify_config(reference_values),
        });
        if !route_plan.connect_mode {
            entry["attest"] = tng_attest_config();
        }
        ingress.push(entry);
    }

    Ok(json!({
        "control_interface": {
            "restful": {
                "host": "127.0.0.1",
                "port": 50000,
            }
        },
        "add_egress": egress,
        "add_ingress": ingress,
    }))
}

fn gateway_config(
    bundle: &MeshBundle,
    self_id: &str,
    bootstrap: &BootstrapConfig,
    plan: &RuntimePortPlan,
) -> Result<Value> {
    let identity = bootstrap
        .gateway_identity
        .as_ref()
        .context("bootstrap is missing gateway_identity")?;
    let self_service = bundle
        .services
        .get(self_id)
        .with_context(|| format!("service '{self_id}' is not present in mesh bundle"))?;
    let mut trusted_services = serde_json::Map::new();
    for (service_id, service) in bundle
        .services
        .iter()
        .filter(|(_, service)| service.phase == "active")
    {
        let public_key = service.gateway_public_key.as_ref().with_context(|| {
            format!("active service '{service_id}' is missing gateway_public_key")
        })?;
        trusted_services.insert(
            service_id.clone(),
            json!({
                "public_key": public_key,
            }),
        );
    }
    let client_routes = plan
        .peer_routes
        .iter()
        .filter(|route| !route.connect_mode)
        .map(|route| {
            json!({
                "listen_host": "127.0.0.1",
                "listen_port": route.local_port,
                "tng_host": "127.0.0.1",
                "tng_port": route.tng_ingress_port,
                "target_service": route.service_id,
                "target_port": route.remote_port,
            })
        })
        .collect::<Vec<_>>();
    let self_mesh_ports = confidential_ports(&self_service.ports, &self_service.connect)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let server_routes = plan
        .self_routes
        .iter()
        .filter(|route| self_mesh_ports.contains(&route.port))
        .map(|route| {
            json!({
                "listen_host": "127.0.0.1",
                "listen_port": route.gateway_port,
                "upstream_host": "127.0.0.1",
                "upstream_port": route.port,
                "protocol": route.protocol,
                "audit_path": format!("/var/lib/cai-gateway/audit-{}.jsonl", route.port),
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "schema": "confidential-agent/gateway-config/v1",
        "service_id": self_id,
        "identity": {
            "public_key": identity.public_key,
            "private_key": identity.private_key,
        },
        "mesh_generation": bundle.generation,
        "token_ttl_sec": 60,
        "so_mark": DEFAULT_TNG_SO_MARK,
        "trusted_services": trusted_services,
        "client_routes": client_routes,
        "server_routes": server_routes,
    }))
}

fn empty_gateway_config(bootstrap: &BootstrapConfig) -> Result<Value> {
    let mut config = json!({
        "schema": "confidential-agent/gateway-config/v1",
        "service_id": bootstrap.service_id,
        "mesh_generation": 0,
        "token_ttl_sec": 60,
        "so_mark": DEFAULT_TNG_SO_MARK,
        "trusted_services": {},
        "client_routes": [],
        "server_routes": [],
    });
    if let Some(identity) = bootstrap.gateway_identity.as_ref() {
        config["identity"] = json!({
            "public_key": identity.public_key,
            "private_key": identity.private_key,
        });
    }
    Ok(config)
}

fn sync_gateway_service(config: &Value, state: &mut DaemonState) -> Result<()> {
    let fingerprint = sha256_bytes(serde_json::to_vec(config)?.as_slice());
    let path = gateway_config_path();
    let changed = write_json_if_changed(&path, config)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).with_context(|| {
        format!(
            "failed to set private gateway config mode on '{}'",
            path.display()
        )
    })?;
    let mut restarted = false;
    if changed || state.gateway_started_fingerprint.as_deref() != Some(&fingerprint) {
        restart_service(GATEWAY_SERVICE)?;
        state.gateway_started_fingerprint = Some(fingerprint.clone());
        state.gateway_last_restart_unix = Some(now_unix());
        restarted = true;
    }
    let gateway_ready =
        ensure_gateway_service_active(restarted, &mut state.gateway_last_restart_unix)
            && gateway_ports_ready(config);
    if gateway_ready {
        state.gateway_fingerprint = Some(fingerprint);
    } else {
        state.gateway_fingerprint = None;
    }
    Ok(())
}

fn gateway_ports_ready(config: &Value) -> bool {
    gateway_config_listen_ports(config)
        .iter()
        .all(|port| local_tcp_port_ready(*port))
}

fn gateway_config_listen_ports(config: &Value) -> Vec<u16> {
    let mut ports = ["client_routes", "server_routes"]
        .into_iter()
        .flat_map(|key| {
            config
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|route| {
            route
                .get("listen_port")
                .and_then(Value::as_u64)
                .and_then(|port| u16::try_from(port).ok())
        })
        .collect::<Vec<_>>();
    ports.sort_unstable();
    ports.dedup();
    ports
}

fn tng_reference_values(bundle: &MeshBundle, service_id: &str) -> Result<Value> {
    if let Some(sample) = bundle.reference_values.get(service_id) {
        return Ok(json!([
            {
                "type": "sample",
                "payload": {
                    "type": "inline",
                    "content": sample,
                },
            }
        ]));
    }

    if let Some(rekor) = bundle.rekor_reference_values.get(service_id) {
        return Ok(json!([
            {
                "type": "slsa",
                "payload": {
                    "type": "inline",
                    "content": rekor_payload(rekor)?,
                },
            }
        ]));
    }

    bail!("missing reference values for active peer service '{service_id}'")
}

fn read_a2a_bundle(args: &RunArgs) -> Result<Option<A2aBundle>> {
    let path = args.cdh_root.join(&args.a2a_bundle_resource);
    if !path.exists() || path.metadata()?.len() == 0 {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read a2a bundle '{}'", path.display()))?;
    let bundle: A2aBundle = serde_json::from_str(&content).context("invalid a2a bundle JSON")?;
    bundle.validate()?;
    let cache_dir = daemon_cache_dir();
    fs::create_dir_all(&cache_dir)?;
    fs::write(
        cache_dir.join("a2a-bundle.json"),
        serde_json::to_string_pretty(&bundle)?,
    )?;
    Ok(Some(bundle))
}

fn a2a_tng_ingress(
    bundle: &A2aBundle,
    self_id: &str,
    reserved_local_ports: &[u16],
    directory: &ServiceDirectory,
    state: &mut DaemonState,
) -> (Vec<Value>, BTreeMap<String, ServiceDirectoryService>) {
    let mut ingress = Vec::new();
    let mut peer_directory = BTreeMap::new();
    let mut used_local_ports = reserved_local_ports
        .iter()
        .copied()
        .chain(service_directory_local_ports(directory))
        .collect::<BTreeSet<_>>();
    let mut active_keys = BTreeSet::new();
    state.a2a_status.clear();
    state.last_error = None;

    for peer in bundle
        .peers
        .iter()
        .filter(|peer| a2a_peer_scoped_to_service(peer, self_id))
    {
        let key = a2a_peer_key(peer);
        active_keys.insert(key.clone());
        match resolve_a2a_peer(peer, &mut used_local_ports, state) {
            Ok(resolved) => {
                if directory.services.contains_key(&resolved.id)
                    || peer_directory.contains_key(&resolved.id)
                {
                    let error = format!(
                        "a2a peer '{}' directory id conflicts with an existing service",
                        resolved.id
                    );
                    state.last_error = Some(error.clone());
                    state.a2a_status.insert(
                        key,
                        DaemonA2aPeerStatus {
                            url: peer.url.clone(),
                            id: Some(resolved.id),
                            state: "error".to_string(),
                            last_fetch_unix: Some(now_unix()),
                            last_success_unix: None,
                            error: Some(error),
                            ports: Vec::new(),
                        },
                    );
                    continue;
                }

                let mut dir_ports = Vec::new();
                for port in &resolved.ports {
                    ingress.push(json!({
                        "rats_tls": {
                            "multiplex": true,
                        },
                        "mapping": {
                            "in": {
                                "host": "127.0.0.1",
                                "port": port.local,
                            },
                            "out": {
                                "host": resolved.public_ip,
                                "port": port.remote,
                            },
                        },
                        "verify": {
                            "as_type": "builtin",
                            "attestation_policy": {
                                "type": "path",
                                "path": DEFAULT_POLICY_PATH,
                            },
                            "reference_values": resolved.reference_values,
                        },
                    }));
                    dir_ports.push(ServiceDirectoryPort {
                        address: "127.0.0.1".to_string(),
                        port: port.local,
                        mode: Some("connect".to_string()),
                        protocol: None,
                    });
                }
                peer_directory.insert(resolved.id, ServiceDirectoryService { ports: dir_ports });
            }
            Err(err) => {
                let error = err.to_string();
                state.last_error = Some(error.clone());
                state
                    .a2a_status
                    .entry(key)
                    .or_insert_with(|| DaemonA2aPeerStatus {
                        url: peer.url.clone(),
                        id: peer.alias.clone(),
                        state: "error".to_string(),
                        last_fetch_unix: Some(now_unix()),
                        last_success_unix: None,
                        error: Some(error),
                        ports: Vec::new(),
                    });
            }
        }
    }
    state.a2a_cache.retain(|key, _| active_keys.contains(key));
    (ingress, peer_directory)
}

#[derive(Debug, Clone)]
struct ResolvedA2aPeer {
    id: String,
    public_ip: String,
    ports: Vec<A2aCachedPort>,
    reference_values: Value,
}

fn resolve_a2a_peer(
    peer: &A2aBundlePeer,
    used_local_ports: &mut BTreeSet<u16>,
    state: &mut DaemonState,
) -> Result<ResolvedA2aPeer> {
    let key = a2a_peer_key(peer);
    let now = now_unix();
    let can_use_cache = state
        .a2a_cache
        .get(&key)
        .map(|cached| cached.fingerprint == peer.fingerprint && now < cached.next_refresh_unix)
        .unwrap_or(false);
    if can_use_cache {
        let cached = state
            .a2a_cache
            .get_mut(&key)
            .expect("cache existence checked above");
        if !cached_peer_is_resolvable(cached) {
            let error = cached
                .error
                .clone()
                .unwrap_or_else(|| "recent a2a peer fetch failed".to_string());
            state.a2a_status.insert(
                key.clone(),
                DaemonA2aPeerStatus {
                    url: peer.url.clone(),
                    id: peer.alias.clone(),
                    state: "error".to_string(),
                    last_fetch_unix: Some(cached.fetched_at_unix),
                    last_success_unix: None,
                    error: Some(error.clone()),
                    ports: Vec::new(),
                },
            );
            bail!(
                "a2a peer '{}' is in fetch backoff until {}: {}",
                peer.alias.as_deref().unwrap_or(&peer.url),
                cached.next_refresh_unix,
                error
            );
        }
        ensure_cached_ports_available(cached, used_local_ports)?;
        let resolved = resolved_from_cache(cached)?;
        let state_name = if cached.error.is_some() {
            "stale"
        } else {
            "ok"
        };
        state.a2a_status.insert(
            key,
            DaemonA2aPeerStatus {
                url: peer.url.clone(),
                id: Some(resolved.id.clone()),
                state: state_name.to_string(),
                last_fetch_unix: Some(cached.fetched_at_unix),
                last_success_unix: Some(cached.fetched_at_unix),
                error: cached.error.clone(),
                ports: resolved.ports.iter().map(|port| port.local).collect(),
            },
        );
        return Ok(resolved);
    }

    let signer = peer
        .signer
        .as_ref()
        .map(confidential_agent_core::agent_card_signing::AgentCardSignerPin::from);
    match fetch_agent_card_with_signer(&peer.url, signer.as_ref()) {
        Ok(card) => {
            let ext = confidential_extension(&card)?;
            let reference_values = agent_card_reference_values(&card)?;
            let mut remote_ports = ext.ports.iter().map(|port| port.port).collect::<Vec<_>>();
            remote_ports.sort_unstable();
            remote_ports.dedup();
            let mut ports = Vec::new();
            for remote in remote_ports {
                let local = allocate_a2a_local_port(remote, used_local_ports)?;
                ports.push(A2aCachedPort { remote, local });
            }
            let id = peer.alias.clone().unwrap_or_else(|| ext.id.clone());
            let cached = A2aCachedPeer {
                url: peer.url.clone(),
                alias: peer.alias.clone(),
                id: Some(id.clone()),
                ports: ports.clone(),
                public_ip: Some(ext.public_ip.clone()),
                reference_values: reference_values.clone(),
                fetched_at_unix: now,
                next_refresh_unix: now + a2a_cache_ttl_sec(ext.cache_ttl_sec),
                fingerprint: peer.fingerprint.clone(),
                error: None,
            };
            state.a2a_cache.insert(key.clone(), cached);
            state.a2a_status.insert(
                key,
                DaemonA2aPeerStatus {
                    url: peer.url.clone(),
                    id: Some(id.clone()),
                    state: "ok".to_string(),
                    last_fetch_unix: Some(now),
                    last_success_unix: Some(now),
                    error: None,
                    ports: ports.iter().map(|port| port.local).collect(),
                },
            );
            Ok(ResolvedA2aPeer {
                id,
                public_ip: ext.public_ip.clone(),
                ports,
                reference_values,
            })
        }
        Err(fetch_err) => {
            let stale_allowed = a2a_fetch_error_allows_stale(&fetch_err);
            let err = anyhow::Error::new(fetch_err);
            let error = err.to_string();
            if let Some(cached) = state.a2a_cache.get_mut(&key) {
                let last_success_unix = if cached_peer_is_resolvable(cached) {
                    Some(cached.fetched_at_unix)
                } else {
                    None
                };
                cached.next_refresh_unix = now + A2A_FETCH_FAILURE_BACKOFF_SEC;
                cached.error = Some(error.clone());
                cached.url = peer.url.clone();
                cached.alias = peer.alias.clone();
                cached.fingerprint = peer.fingerprint.clone();

                if stale_allowed && cached_peer_is_resolvable(cached) {
                    ensure_cached_ports_available(cached, used_local_ports)?;
                    let resolved = resolved_from_cache(cached)?;
                    state.a2a_status.insert(
                        key,
                        DaemonA2aPeerStatus {
                            url: peer.url.clone(),
                            id: Some(resolved.id.clone()),
                            state: "stale".to_string(),
                            last_fetch_unix: Some(now),
                            last_success_unix: Some(cached.fetched_at_unix),
                            error: Some(error),
                            ports: resolved.ports.iter().map(|port| port.local).collect(),
                        },
                    );
                    return Ok(resolved);
                }

                if !stale_allowed {
                    *cached = negative_a2a_cache(peer, now, error.clone());
                } else {
                    cached.fetched_at_unix = now;
                }
                state.a2a_status.insert(
                    key.clone(),
                    DaemonA2aPeerStatus {
                        url: peer.url.clone(),
                        id: peer.alias.clone(),
                        state: "error".to_string(),
                        last_fetch_unix: Some(now),
                        last_success_unix,
                        error: Some(error.clone()),
                        ports: Vec::new(),
                    },
                );
            } else {
                state
                    .a2a_cache
                    .insert(key.clone(), negative_a2a_cache(peer, now, error.clone()));
                state.a2a_status.insert(
                    key.clone(),
                    DaemonA2aPeerStatus {
                        url: peer.url.clone(),
                        id: peer.alias.clone(),
                        state: "error".to_string(),
                        last_fetch_unix: Some(now),
                        last_success_unix: None,
                        error: Some(error.clone()),
                        ports: Vec::new(),
                    },
                );
            }
            bail!(
                "failed to fetch a2a peer '{}' and no cached AgentCard is available: {}",
                peer.alias.as_deref().unwrap_or(&peer.url),
                error
            )
        }
    }
}

fn a2a_fetch_error_allows_stale(err: &AgentCardFetchError) -> bool {
    match err {
        AgentCardFetchError::Transport(_) => true,
        AgentCardFetchError::HttpStatus { status, .. } => *status >= 500,
        _ => false,
    }
}

fn resolved_from_cache(cached: &A2aCachedPeer) -> Result<ResolvedA2aPeer> {
    Ok(ResolvedA2aPeer {
        id: cached
            .alias
            .clone()
            .or_else(|| cached.id.clone())
            .context("cached a2a peer has no id")?,
        public_ip: cached
            .public_ip
            .clone()
            .context("cached a2a peer has no public_ip")?,
        ports: cached.ports.clone(),
        reference_values: cached.reference_values.clone(),
    })
}

fn cached_peer_is_resolvable(cached: &A2aCachedPeer) -> bool {
    cached.id.is_some()
        && cached.public_ip.is_some()
        && !cached.ports.is_empty()
        && !cached.reference_values.is_null()
}

fn negative_a2a_cache(peer: &A2aBundlePeer, now: u64, error: String) -> A2aCachedPeer {
    A2aCachedPeer {
        url: peer.url.clone(),
        alias: peer.alias.clone(),
        id: None,
        ports: Vec::new(),
        public_ip: None,
        reference_values: Value::Null,
        fetched_at_unix: now,
        next_refresh_unix: now + A2A_FETCH_FAILURE_BACKOFF_SEC,
        fingerprint: peer.fingerprint.clone(),
        error: Some(error),
    }
}

fn a2a_cache_ttl_sec(declared: u64) -> u64 {
    declared.clamp(A2A_CACHE_TTL_MIN_SEC, A2A_CACHE_TTL_MAX_SEC)
}

fn ensure_cached_ports_available(
    cached: &mut A2aCachedPeer,
    used_local_ports: &mut BTreeSet<u16>,
) -> Result<()> {
    if cached
        .ports
        .iter()
        .all(|port| !used_local_ports.contains(&port.local))
    {
        for port in &cached.ports {
            used_local_ports.insert(port.local);
        }
        return Ok(());
    }

    for port in &mut cached.ports {
        port.local = allocate_a2a_local_port(port.remote, used_local_ports)?;
    }
    Ok(())
}

fn allocate_a2a_local_port(preferred: u16, used: &mut BTreeSet<u16>) -> Result<u16> {
    if preferred != 0 && !used.contains(&preferred) {
        used.insert(preferred);
        return Ok(preferred);
    }
    for port in 18000..=60999 {
        if !used.contains(&port) {
            used.insert(port);
            return Ok(port);
        }
    }
    bail!("no available local port for a2a peer")
}

fn a2a_peer_scoped_to_service(peer: &A2aBundlePeer, service_id: &str) -> bool {
    peer.scoped_services.is_empty() || peer.scoped_services.iter().any(|id| id == service_id)
}

fn a2a_peer_key(peer: &A2aBundlePeer) -> String {
    peer.alias.clone().unwrap_or_else(|| peer.url.clone())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn write_json_if_changed(path: &Path, value: &Value) -> Result<bool> {
    let new_content = serde_json::to_string_pretty(value)?;
    if let Ok(old_content) = fs::read_to_string(path) {
        let old: Option<Value> = serde_json::from_str(&old_content).ok();
        if old.as_ref() == Some(value) {
            return Ok(false);
        }
    }

    write_file_atomic(path, new_content.as_bytes())?;
    Ok(true)
}

fn read_daemon_state() -> Result<DaemonState> {
    let path = daemon_state_path();
    if !path.exists() {
        return Ok(DaemonState::default());
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read '{}'", path.display()))?;
    serde_json::from_str(&content).context("failed to parse daemon state")
}

fn write_daemon_state(state: &DaemonState) -> Result<()> {
    let path = daemon_state_path();
    write_json_atomic(&path, state)
}

fn write_status(status: DaemonStatus) -> Result<()> {
    let path = daemon_status_path();
    write_json_atomic(&path, &status)
}

fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let content = serde_json::to_vec_pretty(value)?;
    write_file_atomic(path, &content)
}

fn write_file_atomic(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create '{}'", parent.display()))?;
    }
    let existing_metadata = path.metadata().ok();
    let tmp = path.with_extension("confidential-agent.tmp");
    fs::write(&tmp, content).with_context(|| format!("failed to write '{}'", tmp.display()))?;
    if let Some(metadata) = existing_metadata {
        fs::set_permissions(&tmp, fs::Permissions::from_mode(metadata.mode() & 0o7777))
            .with_context(|| format!("failed to preserve mode on '{}'", tmp.display()))?;
        let tmp_metadata = tmp
            .metadata()
            .with_context(|| format!("failed to stat '{}'", tmp.display()))?;
        if tmp_metadata.uid() != metadata.uid() || tmp_metadata.gid() != metadata.gid() {
            chown(&tmp, Some(metadata.uid()), Some(metadata.gid()))
                .with_context(|| format!("failed to preserve owner on '{}'", tmp.display()))?;
        }
    }
    fs::rename(&tmp, path).with_context(|| format!("failed to replace '{}'", path.display()))
}

fn status_for(
    bootstrap: &BootstrapConfig,
    state: &DaemonState,
    phase: &str,
    readiness: RuntimeReadiness,
) -> DaemonStatus {
    DaemonStatus {
        schema: DAEMON_STATUS_SCHEMA_VERSION.to_string(),
        service_id: bootstrap.service_id.clone(),
        phase: phase.to_string(),
        bootstrap_generation: state.bootstrap_generation,
        mesh_generation: state.mesh_generation,
        applied_resources: state.applied_resources.clone(),
        mesh_fingerprint: state.mesh_fingerprint.clone(),
        app_ready: readiness.app_ready,
        mesh_ready: readiness.mesh_ready,
        debug_ssh_ready: readiness.debug_ssh_ready,
        a2a_peers: state.a2a_status.clone(),
        last_error: state.last_error.clone(),
    }
}

fn ensure_runtime_ready(bootstrap: &BootstrapConfig, mesh_ready: bool) -> RuntimeReadiness {
    RuntimeReadiness {
        app_ready: ensure_app_service_ready(bootstrap),
        mesh_ready,
        debug_ssh_ready: ensure_debug_ssh_ready(),
    }
}

fn start_service(service: &str) -> Result<()> {
    if std::env::var_os("CA_SKIP_SYSTEMCTL").is_some() {
        return Ok(());
    }
    let status = Command::new("systemctl")
        .arg("start")
        .arg("--no-block")
        .arg(service)
        .status()
        .with_context(|| format!("failed to start service '{}'", service))?;
    if !status.success() {
        bail!("systemctl start '{}' failed with {}", service, status);
    }
    Ok(())
}

fn prepare_app_service_for_resource_apply(bootstrap: &BootstrapConfig) -> Result<bool> {
    let Some(service) = bootstrap.app_service.as_deref() else {
        return Ok(false);
    };
    if bootstrap.resources.is_empty() || std::env::var_os("CA_SKIP_SYSTEMCTL").is_some() {
        return Ok(false);
    }
    let was_active = service_is_active(service)
        .with_context(|| format!("failed to inspect service '{service}' before resource apply"))?;
    let output = Command::new("systemctl")
        .arg("stop")
        .arg(service)
        .output()
        .with_context(|| format!("failed to stop service '{}' before resource apply", service))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if systemctl_reports_missing_unit(&output.status, &stderr) {
            eprintln!(
                "app service '{}' is not loaded; continuing resource apply before first start",
                service
            );
            return Ok(false);
        }
        bail!("systemctl stop '{}' failed with {}", service, output.status);
    }
    let _ = Command::new("systemctl")
        .arg("reset-failed")
        .arg(service)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    Ok(was_active)
}

fn restore_app_service_after_resource_apply_failure(bootstrap: &BootstrapConfig) {
    let Some(service) = bootstrap.app_service.as_deref() else {
        return;
    };
    if let Err(err) = start_service(service) {
        eprintln!("app service restart after resource apply failure failed: {err:#}");
    }
}

fn systemctl_reports_missing_unit(status: &std::process::ExitStatus, stderr: &str) -> bool {
    if status.code() == Some(5) {
        return true;
    }
    let stderr = stderr.to_ascii_lowercase();
    stderr.contains("not loaded") || stderr.contains("could not be found")
}

fn ensure_app_service_ready(bootstrap: &BootstrapConfig) -> bool {
    let Some(service) = bootstrap.app_service.as_deref() else {
        return true;
    };
    if let Err(err) = start_service(service) {
        eprintln!("app service start failed: {err:#}");
        return false;
    }
    match service_is_active(service) {
        Ok(true) => app_ports_ready(&bootstrap.ports),
        Ok(false) => false,
        Err(err) => {
            eprintln!("app service status check failed: {err:#}");
            false
        }
    }
}

fn app_ports_ready(ports: &[u16]) -> bool {
    ports.iter().all(|port| local_tcp_port_ready(*port))
}

fn mesh_ports_ready(peer_ports: &[u16]) -> bool {
    local_tcp_port_ready(TNG_CONTROL_PORT)
        && peer_ports.iter().all(|port| local_tcp_port_ready(*port))
}

fn ensure_tng_service_active(just_restarted: bool, last_restart_unix: &mut Option<u64>) -> bool {
    ensure_service_active(TNG_SERVICE, "TNG", just_restarted, last_restart_unix)
}

fn ensure_gateway_service_active(
    just_restarted: bool,
    last_restart_unix: &mut Option<u64>,
) -> bool {
    ensure_service_active(
        GATEWAY_SERVICE,
        "gateway",
        just_restarted,
        last_restart_unix,
    )
}

fn ensure_service_active(
    service: &str,
    label: &str,
    just_restarted: bool,
    last_restart_unix: &mut Option<u64>,
) -> bool {
    match service_is_active(service) {
        Ok(true) => true,
        Ok(false) => {
            if just_restarted || restart_backoff_active(*last_restart_unix) {
                return false;
            }
            if let Err(err) = restart_service(service) {
                eprintln!("{label} service restart failed: {err:#}");
                return false;
            }
            *last_restart_unix = Some(now_unix());
            match service_is_active(service) {
                Ok(active) => active,
                Err(err) => {
                    eprintln!("{label} service status check failed after restart: {err:#}");
                    false
                }
            }
        }
        Err(err) => {
            eprintln!("{label} service status check failed: {err:#}");
            false
        }
    }
}

fn restart_backoff_active(last_restart_unix: Option<u64>) -> bool {
    let Some(last_restart_unix) = last_restart_unix else {
        return false;
    };
    now_unix().saturating_sub(last_restart_unix) < SERVICE_RESTART_MIN_INTERVAL_SEC
}

fn local_tcp_port_ready(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

fn ensure_debug_ssh_ready() -> bool {
    ensure_debug_ssh_ready_for(
        Path::new(DEBUG_SSH_MARKER_PATH),
        Path::new(DEBUG_AUTHORIZED_KEYS_PATH),
        Path::new(DEBUG_SSHD_DROPIN_DIR),
        Path::new(DEBUG_SSHD_RUN_DIR),
    )
}

fn ensure_debug_ssh_ready_for(
    marker: &Path,
    authorized_keys: &Path,
    dropin_dir: &Path,
    run_dir: &Path,
) -> bool {
    if !marker.exists() {
        return false;
    }
    if !authorized_keys.exists()
        || authorized_keys
            .metadata()
            .map(|metadata| metadata.len() == 0)
            .unwrap_or(true)
    {
        return false;
    }
    if std::env::var_os("CA_SKIP_SYSTEMCTL").is_some() {
        return true;
    }
    match systemd_unit_exists("sshd.service") {
        Ok(true) => {}
        Ok(false) => return false,
        Err(err) => {
            eprintln!("debug ssh systemd unit check failed: {err:#}");
            return false;
        }
    }
    if let Err(err) = prepare_debug_sshd_runtime(dropin_dir, run_dir) {
        eprintln!("debug ssh runtime prepare failed: {err:#}");
        return false;
    }
    match service_is_active("sshd.service") {
        Ok(true) => true,
        Ok(false) => {
            if let Err(err) = restart_service("sshd.service") {
                eprintln!("debug ssh service restart failed: {err:#}");
                return false;
            }
            match service_is_active("sshd.service") {
                Ok(active) => active,
                Err(err) => {
                    eprintln!("debug ssh service status check failed: {err:#}");
                    false
                }
            }
        }
        Err(err) => {
            eprintln!("debug ssh service status check failed: {err:#}");
            false
        }
    }
}

fn systemd_unit_exists(service: &str) -> Result<bool> {
    let output = Command::new("systemctl")
        .arg("list-unit-files")
        .arg(service)
        .arg("--no-legend")
        .output()
        .with_context(|| format!("failed to list systemd unit '{service}'"))?;
    Ok(output.status.success() && !output.stdout.is_empty())
}

fn prepare_debug_sshd_runtime(dropin_dir: &Path, run_dir: &Path) -> Result<()> {
    let keygen = Command::new("ssh-keygen")
        .arg("-A")
        .status()
        .context("failed to generate SSH host keys")?;
    if !keygen.success() {
        bail!("ssh-keygen -A failed with {}", keygen);
    }
    fs::create_dir_all(run_dir)
        .with_context(|| format!("failed to create '{}'", run_dir.display()))?;
    fs::create_dir_all(dropin_dir)
        .with_context(|| format!("failed to create '{}'", dropin_dir.display()))?;
    fs::write(
        dropin_dir.join("10-confidential-agent-debug.conf"),
        "[Service]\nExecStartPre=/usr/bin/mkdir -p /run/sshd\nExecStartPre=/usr/bin/ssh-keygen -A\n",
    )
    .with_context(|| format!("failed to write sshd drop-in under '{}'", dropin_dir.display()))?;
    Ok(())
}

fn service_is_active(service: &str) -> Result<bool> {
    if std::env::var_os("CA_SKIP_SYSTEMCTL").is_some() {
        return Ok(true);
    }
    let status = Command::new("systemctl")
        .arg("is-active")
        .arg("--quiet")
        .arg(service)
        .status()
        .with_context(|| format!("failed to check systemd service '{service}'"))?;
    Ok(status.success())
}

fn restart_service(service: &str) -> Result<()> {
    if std::env::var_os("CA_SKIP_SYSTEMCTL").is_some() {
        return Ok(());
    }
    let reload_status = Command::new("systemctl")
        .arg("daemon-reload")
        .status()
        .with_context(|| "failed to reload systemd manager configuration")?;
    if !reload_status.success() {
        bail!("systemctl daemon-reload failed with {}", reload_status);
    }
    let reset_status = Command::new("systemctl")
        .arg("reset-failed")
        .arg(service)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to reset failed state for service '{}'", service))?;
    let _ = reset_status;
    let status = Command::new("systemctl")
        .arg("restart")
        .arg(service)
        .status()
        .with_context(|| format!("failed to restart service '{}'", service))?;
    if !status.success() {
        bail!("systemctl restart '{}' failed with {}", service, status);
    }
    Ok(())
}

fn parse_mode(mode: &str) -> Result<u32> {
    let trimmed = mode.trim();
    let trimmed = trimmed.strip_prefix("0o").unwrap_or(trimmed);
    let parsed =
        u32::from_str_radix(trimmed, 8).with_context(|| format!("invalid file mode '{}'", mode))?;
    if parsed > 0o7777 {
        bail!("file mode '{}' exceeds maximum 0o7777", mode);
    }
    Ok(parsed)
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    hex_encode(digest.as_ref())
}

#[cfg(test)]
mod tests;
