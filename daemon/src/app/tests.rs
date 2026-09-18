use super::*;
use confidential_agent_core::schema::GatewayIdentity;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

static ENV_LOCK: Mutex<()> = Mutex::new(());

const TEST_BOOTSTRAP_RESOURCE: &str = "default/local-resources/cagent_bootstrap_config";
const TEST_MESH_RESOURCE: &str = "default/local-resources/cagent_mesh_bundle";
const TEST_A2A_BUNDLE_RESOURCE: &str = "default/local-resources/cagent_a2a_bundle";
const DAEMON_ENV_VARS: &[&str] = &[
    "CA_TNG_CONFIG_PATH",
    "CA_GATEWAY_CONFIG_PATH",
    "CA_SERVICE_DIRECTORY_PATH",
    "CA_DAEMON_STATUS_PATH",
    "CA_AGENT_CARD_PATH",
    "CA_DAEMON_CACHE_DIR",
    "CA_DAEMON_STATE_PATH",
    "CA_SKIP_SYSTEMCTL",
    "PATH",
];

struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn new(vars: &[&'static str]) -> Self {
        let lock = ENV_LOCK.lock().unwrap();
        let saved = vars
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();
        Self { _lock: lock, saved }
    }

    fn set(&self, name: &'static str, value: impl AsRef<OsStr>) {
        std::env::set_var(name, value);
    }

    fn set_path(&self, name: &'static str, path: &Path) {
        self.set(name, path.as_os_str());
    }

    fn remove(&self, name: &'static str) {
        std::env::remove_var(name);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in self.saved.iter().rev() {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

struct NoTeeTestEnv {
    _guard: EnvGuard,
    tng_config: PathBuf,
    gateway_config: PathBuf,
    service_directory: PathBuf,
    status: PathBuf,
    agent_card: PathBuf,
    cache_dir: PathBuf,
    state: PathBuf,
}

#[test]
fn http_parser_accepts_headers_larger_than_1024_bytes() {
    let filler = "a".repeat(1500);
    let request = format!("GET /health HTTP/1.1\r\nHost: localhost\r\nX-Fill: {filler}\r\n\r\n");

    let parsed = parse_http_request_head(request.as_bytes()).unwrap();

    assert_eq!(parsed.method, "GET");
    assert_eq!(parsed.path, "/health");
}

#[test]
fn http_parser_rejects_invalid_utf8_and_body_headers() {
    assert!(matches!(
        parse_http_request_head(b"GET /health HTTP/1.1\r\nX-Bad: \xff\r\n\r\n"),
        Err(HttpRequestError::BadRequest)
    ));
    assert!(matches!(
        parse_http_request_head(b"GET /health HTTP/1.1\r\nContent-Length: 1\r\n\r\n"),
        Err(HttpRequestError::PayloadTooLarge)
    ));
    assert!(matches!(
        parse_http_request_head(b"GET /health HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"),
        Err(HttpRequestError::BadRequest)
    ));
    assert!(matches!(
        parse_http_request_head(b"GET /health\r\n\r\n"),
        Err(HttpRequestError::BadRequest)
    ));
}

#[test]
fn fixed_window_rate_limiter_limits_and_resets_per_peer() {
    let mut limiter = FixedWindowRateLimiter::new(2, Duration::from_millis(100));
    let peer = std::net::IpAddr::from([127, 0, 0, 1]);
    let other_peer = std::net::IpAddr::from([127, 0, 0, 2]);
    let now = Instant::now();

    assert!(limiter.allow(peer, now));
    assert!(limiter.allow(peer, now + Duration::from_millis(1)));
    assert!(!limiter.allow(peer, now + Duration::from_millis(2)));
    assert!(limiter.allow(other_peer, now + Duration::from_millis(2)));
    assert!(limiter.allow(peer, now + Duration::from_millis(100)));
}

#[test]
fn http_active_request_guard_releases_counter() {
    let limits = HttpServerLimits::new(1, HTTP_RATE_LIMIT_REQUESTS, HTTP_RATE_LIMIT_WINDOW);
    let guard = limits.try_acquire_request().unwrap();

    assert!(limits.try_acquire_request().is_none());
    drop(guard);
    assert!(limits.try_acquire_request().is_some());
}

#[test]
fn http_accept_path_rejects_concurrency_exhaustion_without_spawning_handler() {
    let limits = Arc::new(HttpServerLimits::new(
        1,
        HTTP_RATE_LIMIT_REQUESTS,
        HTTP_RATE_LIMIT_WINDOW,
    ));
    let held_slot = limits.try_acquire_request().unwrap();

    let (response, spawned_handler) = http_roundtrip_with_accept_outcome(
        HttpServerKind::Status,
        limits.clone(),
        b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );

    assert!(!spawned_handler);
    assert!(response.starts_with("HTTP/1.1 503 Service Unavailable"));
    assert!(limits.try_acquire_request().is_none());
    drop(held_slot);
    assert!(limits.try_acquire_request().is_some());
}

#[test]
fn http_server_accepts_valid_request_larger_than_1024_bytes() {
    let filler = "a".repeat(1500);
    let request = format!("GET /health HTTP/1.1\r\nHost: localhost\r\nX-Fill: {filler}\r\n\r\n");

    let response = http_roundtrip(HttpServerKind::Status, request.as_bytes());

    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.contains(r#"{"status":"ok"}"#));
}

#[test]
fn http_server_rejects_over_limit_headers_with_431() {
    let mut request = b"GET /health HTTP/1.1\r\nX-Fill: ".to_vec();
    request.resize(HTTP_MAX_HEADER_BYTES, b'a');

    let response = http_roundtrip(HttpServerKind::Status, &request);

    assert!(response.starts_with("HTTP/1.1 431 Request Header Fields Too Large"));
}

#[test]
fn http_server_rejects_non_get_with_allow_header() {
    let response = http_roundtrip(
        HttpServerKind::Status,
        b"POST /health HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );

    assert!(response.starts_with("HTTP/1.1 405 Method Not Allowed"));
    assert!(response.contains("\r\nAllow: GET\r\n"));
}

#[test]
fn http_server_rejects_body_headers() {
    let content_length_response = http_roundtrip(
        HttpServerKind::Status,
        b"GET /health HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1\r\n\r\nx",
    );
    let transfer_encoding_response = http_roundtrip(
        HttpServerKind::Status,
        b"GET /health HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n",
    );

    assert!(content_length_response.starts_with("HTTP/1.1 413 Payload Too Large"));
    assert!(transfer_encoding_response.starts_with("HTTP/1.1 400 Bad Request"));
}

#[test]
fn http_server_rejects_unknown_path_with_404() {
    let response = http_roundtrip(
        HttpServerKind::AgentCard,
        b"GET /status HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );

    assert!(response.starts_with("HTTP/1.1 404 Not Found"));
}

#[test]
fn http_server_rate_limit_is_shared_across_kinds() {
    let limits = Arc::new(HttpServerLimits::new(64, 2, Duration::from_secs(60)));
    let request = b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n";

    let first = http_roundtrip_with_limits(HttpServerKind::Status, limits.clone(), request);
    let second = http_roundtrip_with_limits(HttpServerKind::AgentCard, limits.clone(), request);
    let third = http_roundtrip_with_limits(HttpServerKind::Status, limits, request);

    assert!(first.starts_with("HTTP/1.1 200 OK"));
    assert!(second.starts_with("HTTP/1.1 200 OK"));
    assert!(third.starts_with("HTTP/1.1 429 Too Many Requests"));
}

#[test]
fn resource_watcher_rejects_missing_cdh_root() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing-cdh");

    let err = ResourceWatcher::new(
        &missing,
        [Path::new("default/local-resources/cagent_bootstrap_config")],
    )
    .unwrap_err();

    assert!(format!("{err:#}").contains("cdh root"));
}

#[test]
fn resource_watcher_wakes_on_directory_chain_creation_and_atomic_rename() {
    let temp = tempfile::tempdir().unwrap();
    let cdh_root = temp.path().join("cdh");
    fs::create_dir(&cdh_root).unwrap();
    let resource = Path::new("default/local-resources/cagent_bootstrap_config");
    let resource_path = cdh_root.join(resource);
    let mut watcher = ResourceWatcher::new(&cdh_root, [resource]).unwrap();

    fs::create_dir(cdh_root.join("default")).unwrap();
    assert_eq!(
        watcher.wait(Some(Duration::from_secs(1))).unwrap(),
        ResourceWatcherWait::Event
    );

    fs::create_dir(cdh_root.join("default/local-resources")).unwrap();
    assert_eq!(
        watcher.wait(Some(Duration::from_secs(1))).unwrap(),
        ResourceWatcherWait::Event
    );

    let staged = temp.path().join("bootstrap.next");
    fs::write(&staged, "{}").unwrap();
    fs::rename(&staged, &resource_path).unwrap();
    assert_eq!(
        watcher.wait(Some(Duration::from_secs(1))).unwrap(),
        ResourceWatcherWait::Event
    );
    assert_eq!(fs::read_to_string(resource_path).unwrap(), "{}");
}

#[test]
fn resource_watcher_times_out_without_events() {
    let temp = tempfile::tempdir().unwrap();
    let cdh_root = temp.path().join("cdh");
    fs::create_dir(&cdh_root).unwrap();
    let mut watcher = ResourceWatcher::new(
        &cdh_root,
        [Path::new("default/local-resources/cagent_bootstrap_config")],
    )
    .unwrap();

    assert_eq!(
        watcher.wait(Some(Duration::from_millis(50))).unwrap(),
        ResourceWatcherWait::Timeout
    );
}

#[test]
fn overdue_a2a_refresh_timeout_is_throttled() {
    let mut state = DaemonState::default();
    let now = now_unix();
    state.a2a_cache.insert(
        "peer".to_string(),
        A2aCachedPeer {
            url: "http://127.0.0.1/.well-known/agent-card.json".to_string(),
            alias: None,
            id: None,
            ports: Vec::new(),
            public_ip: None,
            reference_values: json!({}),
            fetched_at_unix: now.saturating_sub(120),
            next_refresh_unix: now.saturating_sub(1),
            fingerprint: "fingerprint".to_string(),
            error: None,
        },
    );

    assert_eq!(
        next_a2a_refresh_timeout(&state, true),
        Some(Duration::from_secs(A2A_OVERDUE_REFRESH_RETRY_SEC))
    );
    assert_eq!(next_a2a_refresh_timeout(&state, false), None);
}

#[test]
fn active_bootstrap_polls_even_without_a2a_peers() {
    let state = DaemonState::default();

    assert_eq!(next_resource_refresh_timeout(&state, false, false), None);
    assert_eq!(
        next_resource_refresh_timeout(&state, true, false),
        Some(Duration::from_secs(ACTIVE_BOOTSTRAP_IDLE_POLL_SEC))
    );
}

#[test]
fn active_bootstrap_uses_earlier_a2a_refresh_timeout() {
    let mut state = DaemonState::default();
    let now = now_unix();
    state.a2a_cache.insert(
        "peer".to_string(),
        A2aCachedPeer {
            url: "http://127.0.0.1/.well-known/agent-card.json".to_string(),
            alias: None,
            id: None,
            ports: Vec::new(),
            public_ip: None,
            reference_values: json!({}),
            fetched_at_unix: now.saturating_sub(120),
            next_refresh_unix: now.saturating_sub(1),
            fingerprint: "fingerprint".to_string(),
            error: None,
        },
    );

    assert_eq!(
        next_resource_refresh_timeout(&state, true, false),
        Some(Duration::from_secs(A2A_OVERDUE_REFRESH_RETRY_SEC))
    );
}

#[test]
fn active_bootstrap_uses_future_a2a_refresh_before_idle_poll() {
    let mut state = DaemonState::default();
    let now = now_unix();
    state.a2a_cache.insert(
        "peer".to_string(),
        A2aCachedPeer {
            url: "http://127.0.0.1/.well-known/agent-card.json".to_string(),
            alias: None,
            id: None,
            ports: Vec::new(),
            public_ip: None,
            reference_values: json!({}),
            fetched_at_unix: now,
            next_refresh_unix: now + 15,
            fingerprint: "fingerprint".to_string(),
            error: None,
        },
    );

    assert_eq!(
        next_resource_refresh_timeout(&state, true, false),
        Some(Duration::from_secs(15))
    );
}

#[test]
fn active_bootstrap_caps_late_a2a_refresh_to_idle_poll() {
    let mut state = DaemonState::default();
    let now = now_unix();
    state.a2a_cache.insert(
        "peer".to_string(),
        A2aCachedPeer {
            url: "http://127.0.0.1/.well-known/agent-card.json".to_string(),
            alias: None,
            id: None,
            ports: Vec::new(),
            public_ip: None,
            reference_values: json!({}),
            fetched_at_unix: now,
            next_refresh_unix: now + ACTIVE_BOOTSTRAP_IDLE_POLL_SEC + 60,
            fingerprint: "fingerprint".to_string(),
            error: None,
        },
    );

    assert_eq!(
        next_resource_refresh_timeout(&state, true, false),
        Some(Duration::from_secs(ACTIVE_BOOTSTRAP_IDLE_POLL_SEC))
    );
}

#[test]
fn parses_octal_modes() {
    assert_eq!(parse_mode("0600").unwrap(), 0o600);
    assert_eq!(parse_mode("0o644").unwrap(), 0o644);
}

#[test]
fn rejects_excessive_octal_modes() {
    assert!(parse_mode("0o7777").is_ok());
    let err = parse_mode("0o10000").unwrap_err();
    assert!(err.to_string().contains("exceeds maximum 0o7777"));
}

#[test]
fn service_directory_includes_peer_connect_and_mesh_ports() {
    let bundle: MeshBundle = serde_json::from_value(json!({
        "schema": "confidential-agent/mesh-bundle/v1",
        "generation": 1,
        "updated_at": 0,
        "services": {
            "self": {
                "phase": "active",
                "ports": [18789],
                "connect": [18789]
            },
            "peer": {
                "phase": "active",
                "public_ip": "39.105.93.168",
                "ports": [3001, 3002],
                "connect": [3002],
                "mcp_ports": [3001]
            },
            "connect-only": {
                "phase": "active",
                "public_ip": "39.105.93.169",
                "ports": [4001],
                "connect": [4001]
            },
            "old": {
                "phase": "deleted",
                "ports": [9090],
                "connect": []
            }
        },
        "reference_values": {},
        "rekor_reference_values": {}
    }))
    .unwrap();

    let plan = runtime_port_plan(&bundle, "self").unwrap();
    let directory = serde_json::to_value(service_directory(&bundle, "self", &plan)).unwrap();

    assert!(directory["services"].get("self").is_none());
    assert!(directory["services"].get("old").is_none());
    assert_eq!(
        directory["services"]["connect-only"]["ports"][0]["port"],
        39400
    );
    assert_eq!(
        directory["services"]["connect-only"]["ports"][0]["mode"],
        "connect"
    );
    assert!(directory["services"]["connect-only"]["ports"][0]
        .get("protocol")
        .is_none());
    assert_eq!(
        directory["services"]["peer"]["ports"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(directory["services"]["peer"]["ports"][0]["port"], 3001);
    assert_eq!(directory["services"]["peer"]["ports"][0]["mode"], "mesh");
    assert_eq!(directory["services"]["peer"]["ports"][0]["protocol"], "mcp");
    assert_eq!(directory["services"]["peer"]["ports"][1]["port"], 39402);
    assert_eq!(directory["services"]["peer"]["ports"][1]["mode"], "connect");
    assert!(directory["services"]["peer"]["ports"][1]
        .get("protocol")
        .is_none());
}

#[test]
fn client_only_service_can_discover_and_call_mesh_peer() {
    let bundle: MeshBundle = serde_json::from_value(json!({
        "schema": "confidential-agent/mesh-bundle/v1",
        "generation": 1,
        "updated_at": 0,
        "services": {
            "job": {
                "phase": "active",
                "ports": [],
                "connect": [],
                "gateway_public_key": "job-pub"
            },
            "mcp": {
                "phase": "active",
                "public_ip": "39.105.93.168",
                "ports": [3001],
                "connect": [],
                "mcp_ports": [3001],
                "gateway_public_key": "mcp-pub"
            }
        },
        "reference_values": {
            "mcp": {"measurement.uki.SHA-384": ["abc123"]}
        },
        "rekor_reference_values": {}
    }))
    .unwrap();
    let bootstrap = test_bootstrap_config("job");
    let plan = runtime_port_plan(&bundle, "job").unwrap();

    assert!(plan.self_routes.is_empty());
    assert_eq!(plan.peer_routes.len(), 1);
    let directory = serde_json::to_value(service_directory(&bundle, "job", &plan)).unwrap();
    assert_eq!(directory["services"]["mcp"]["ports"][0]["port"], 3001);
    assert_eq!(directory["services"]["mcp"]["ports"][0]["mode"], "mesh");
    assert_eq!(directory["services"]["mcp"]["ports"][0]["protocol"], "mcp");

    let gateway = gateway_config(&bundle, "job", &bootstrap, &plan).unwrap();
    assert!(gateway["server_routes"].as_array().unwrap().is_empty());
    assert_eq!(gateway["client_routes"].as_array().unwrap().len(), 1);
    assert_eq!(gateway["client_routes"][0]["target_service"], "mcp");
    assert_eq!(gateway["client_routes"][0]["target_port"], 3001);
    assert_eq!(gateway["trusted_services"]["job"]["public_key"], "job-pub");
    assert_eq!(gateway["trusted_services"]["mcp"]["public_key"], "mcp-pub");
}

#[test]
fn restart_service_reloads_systemd_before_touching_tng_unit() {
    let env = EnvGuard::new(&["PATH", "CA_SKIP_SYSTEMCTL"]);
    let temp = tempfile::tempdir().unwrap();
    let fake_systemctl = temp.path().join("systemctl");
    let log_path = temp.path().join("systemctl.log");
    fs::write(
        &fake_systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 0\n",
            log_path.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    env.set(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    env.remove("CA_SKIP_SYSTEMCTL");

    restart_service("trusted-network-gateway.service").unwrap();

    let log = fs::read_to_string(log_path).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(commands[0], "daemon-reload");
    assert_eq!(commands[1], "reset-failed trusted-network-gateway.service");
    assert_eq!(commands[2], "restart trusted-network-gateway.service");
}

#[test]
fn restart_service_ignores_reset_failed_for_never_loaded_tng_unit() {
    let env = EnvGuard::new(&["PATH", "CA_SKIP_SYSTEMCTL"]);
    let temp = tempfile::tempdir().unwrap();
    let fake_systemctl = temp.path().join("systemctl");
    let log_path = temp.path().join("systemctl.log");
    fs::write(
            &fake_systemctl,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = reset-failed ]; then exit 1; fi\nexit 0\n",
                log_path.display()
            ),
        )
        .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    env.set(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    env.remove("CA_SKIP_SYSTEMCTL");

    let result = restart_service("trusted-network-gateway.service");

    result.unwrap();
    let log = fs::read_to_string(log_path).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(commands[0], "daemon-reload");
    assert_eq!(commands[1], "reset-failed trusted-network-gateway.service");
    assert_eq!(commands[2], "restart trusted-network-gateway.service");
}

#[test]
fn app_service_ready_requires_systemd_active_state() {
    let env = EnvGuard::new(&["PATH", "CA_SKIP_SYSTEMCTL"]);
    let temp = tempfile::tempdir().unwrap();
    let fake_systemctl = temp.path().join("systemctl");
    let log_path = temp.path().join("systemctl.log");
    fs::write(
            &fake_systemctl,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = is-active ]; then exit 3; fi\nexit 0\n",
                log_path.display()
            ),
        )
        .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    env.set(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    env.remove("CA_SKIP_SYSTEMCTL");
    let bootstrap = BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
        generation: 1,
        service_id: "openclaw".to_string(),
        mode: "challenge".to_string(),
        ports: Vec::new(),
        connect: Vec::new(),
        mcp_ports: Vec::new(),
        gateway_identity: None,
        resources: Vec::new(),
        app_service: Some("cai-openclaw-gateway.service".to_string()),
        peers: Vec::new(),
        agent_card: None,
    };

    let ready = ensure_app_service_ready(&bootstrap);

    assert!(!ready);
    let log = fs::read_to_string(log_path).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(commands[0], "start --no-block cai-openclaw-gateway.service");
    assert_eq!(
        commands[1],
        "is-active --quiet cai-openclaw-gateway.service"
    );
}

#[test]
fn app_service_ready_requires_service_port_to_accept_connections() {
    let env = EnvGuard::new(&["PATH", "CA_SKIP_SYSTEMCTL"]);
    let temp = tempfile::tempdir().unwrap();
    let fake_systemctl = temp.path().join("systemctl");
    let log_path = temp.path().join("systemctl.log");
    fs::write(
        &fake_systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 0\n",
            log_path.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    env.set(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    env.remove("CA_SKIP_SYSTEMCTL");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let ready_port = listener.local_addr().unwrap().port();
    let mut bootstrap = BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
        generation: 1,
        service_id: "openclaw".to_string(),
        mode: "challenge".to_string(),
        ports: vec![ready_port],
        connect: Vec::new(),
        mcp_ports: Vec::new(),
        gateway_identity: None,
        resources: Vec::new(),
        app_service: Some("cai-openclaw-gateway.service".to_string()),
        peers: Vec::new(),
        agent_card: None,
    };

    assert!(ensure_app_service_ready(&bootstrap));
    drop(listener);
    bootstrap.ports = vec![ready_port];
    assert!(!ensure_app_service_ready(&bootstrap));
}

#[test]
fn debug_ssh_ready_restarts_sshd_when_authorized_keys_exist() {
    let env = EnvGuard::new(&["PATH", "CA_SKIP_SYSTEMCTL"]);
    let temp = tempfile::tempdir().unwrap();
    let auth_keys = temp.path().join("root/.ssh/authorized_keys");
    fs::create_dir_all(auth_keys.parent().unwrap()).unwrap();
    fs::write(&auth_keys, "ssh-ed25519 AAAA test\n").unwrap();
    let marker = temp.path().join("etc/confidential-agent/debug-ssh-enabled");
    fs::create_dir_all(marker.parent().unwrap()).unwrap();
    fs::write(&marker, "1\n").unwrap();
    let fake_systemctl = temp.path().join("systemctl");
    let fake_keygen = temp.path().join("ssh-keygen");
    let systemctl_log = temp.path().join("systemctl.log");
    let keygen_log = temp.path().join("ssh-keygen.log");
    let active_marker = temp.path().join("sshd.active");
    fs::write(
            &fake_systemctl,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  'list-unit-files sshd.service --no-legend') echo 'sshd.service enabled'; exit 0 ;;\n  'is-active --quiet sshd.service') [ -f '{}' ] && exit 0 || exit 3 ;;\n  'restart sshd.service') touch '{}'; exit 0 ;;\nesac\nexit 0\n",
                systemctl_log.display(),
                active_marker.display(),
                active_marker.display()
            ),
        )
        .unwrap();
    fs::write(
        &fake_keygen,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 0\n",
            keygen_log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&fake_keygen, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    env.set(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    env.remove("CA_SKIP_SYSTEMCTL");
    let dropin_dir = temp.path().join("sshd.service.d");
    let run_dir = temp.path().join("run/sshd");

    let ready = ensure_debug_ssh_ready_for(&marker, &auth_keys, &dropin_dir, &run_dir);

    assert!(ready);
    assert!(dropin_dir.join("10-confidential-agent-debug.conf").exists());
    assert!(run_dir.exists());
    assert_eq!(fs::read_to_string(keygen_log).unwrap(), "-A\n");
    let log = fs::read_to_string(systemctl_log).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(commands[0], "list-unit-files sshd.service --no-legend");
    assert_eq!(commands[1], "is-active --quiet sshd.service");
    assert_eq!(commands[2], "daemon-reload");
    assert_eq!(commands[3], "reset-failed sshd.service");
    assert_eq!(commands[4], "restart sshd.service");
    assert_eq!(commands[5], "is-active --quiet sshd.service");
}

#[test]
fn tng_config_adds_egress_for_self_ports() {
    let bundle: MeshBundle = serde_json::from_value(json!({
        "schema": "confidential-agent/mesh-bundle/v1",
        "generation": 1,
        "updated_at": 0,
        "services": {
            "self": {
                "phase": "active",
                "ports": [18789],
                "connect": [18789]
            }
        },
        "reference_values": {},
        "rekor_reference_values": {}
    }))
    .unwrap();

    let plan = runtime_port_plan(&bundle, "self").unwrap();
    let config = tng_config(&bundle, "self", &plan).unwrap();

    assert_eq!(
        config["add_egress"][0]["netfilter"]["capture_dst"]["port"],
        18789
    );
    assert_eq!(config["add_egress"][0]["netfilter"]["listen_port"], 39000);
    assert_eq!(config["add_egress"][0]["attest"]["aa_type"], "uds");
    assert_eq!(config["add_egress"][0]["rats_tls"]["multiplex"], true);
    assert!(config["add_egress"][0].get("verify").is_none());
    assert_eq!(config["add_ingress"].as_array().unwrap().len(), 0);
}

#[test]
fn tng_config_adds_verify_for_confidential_self_ports() {
    let bundle: MeshBundle = serde_json::from_value(json!({
        "schema": "confidential-agent/mesh-bundle/v1",
        "generation": 1,
        "updated_at": 0,
        "services": {
            "self": {
                "phase": "active",
                "private_ip": "10.0.1.10",
                "public_ip": "47.95.242.63",
                "ports": [18789, 18800],
                "connect": [18789]
            },
            "peer": {
                "phase": "active",
                "private_ip": "10.0.1.11",
                "public_ip": "39.105.93.168",
                "ports": [3001],
                "connect": []
            },
            "connect-only-peer": {
                "phase": "active",
                "private_ip": "10.0.1.12",
                "public_ip": "39.105.93.169",
                "ports": [4001],
                "connect": [4001]
            }
        },
        "reference_values": {
            "peer": {"measurement.uki.SHA-384": ["abc123"]},
            "connect-only-peer": {"measurement.uki.SHA-384": ["def456"]}
        },
        "rekor_reference_values": {}
    }))
    .unwrap();

    let plan = runtime_port_plan(&bundle, "self").unwrap();
    let config = tng_config(&bundle, "self", &plan).unwrap();

    assert!(config["add_egress"][0].get("verify").is_none());
    assert_eq!(
        config["add_egress"][1]["netfilter"]["capture_dst"]["port"],
        18800
    );
    assert_eq!(config["add_egress"][1]["verify"]["as_type"], "builtin");
    assert_eq!(
        config["add_egress"][1]["verify"]["reference_values"][0]["type"],
        "sample"
    );
    assert_eq!(
        config["add_egress"][1]["verify"]["reference_values"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn tng_reference_values_prefers_sample_when_rekor_is_also_present() {
    let bundle: MeshBundle = serde_json::from_value(json!({
        "schema": "confidential-agent/mesh-bundle/v1",
        "generation": 1,
        "updated_at": 0,
        "services": {
            "peer": {
                "phase": "active",
                "private_ip": "10.0.1.11",
                "public_ip": "39.105.93.168",
                "ports": [3001],
                "connect": [3001]
            }
        },
        "reference_values": {
            "peer": {"measurement.uki.SHA-384": ["sample-rv"]}
        },
        "rekor_reference_values": {
            "peer": {
                "artifact_id": "peer-disk",
                "artifact_version": "20260514000000",
                "artifact_type": "uki",
                "rekor_url": "https://rekor.sigstore.dev",
                "rv_name": "measurement.uki.SHA-384"
            }
        }
    }))
    .unwrap();

    let values = tng_reference_values(&bundle, "peer").unwrap();

    assert_eq!(values[0]["type"], "sample");
    assert_eq!(
        values[0]["payload"]["content"]["measurement.uki.SHA-384"][0],
        "sample-rv"
    );
}

#[test]
fn tng_config_adds_mode_specific_ingress_for_peer_ports() {
    let bundle: MeshBundle = serde_json::from_value(json!({
        "schema": "confidential-agent/mesh-bundle/v1",
        "generation": 1,
        "updated_at": 0,
        "services": {
            "openclaw": {
                "phase": "active",
                "private_ip": "10.0.1.10",
                "public_ip": "47.95.242.63",
                "ports": [18789],
                "connect": [18789]
            },
            "mcp": {
                "phase": "active",
                "private_ip": "10.0.1.11",
                "public_ip": "39.105.93.168",
                "ports": [3001, 3002],
                "connect": [3002]
            },
            "remote-connect": {
                "phase": "active",
                "private_ip": "10.0.1.12",
                "public_ip": "39.105.93.169",
                "ports": [4001],
                "connect": [4001]
            }
        },
        "reference_values": {
            "mcp": {"measurement.uki.SHA-384": ["abc123"]},
            "remote-connect": {"measurement.uki.SHA-384": ["def456"]}
        },
        "rekor_reference_values": {}
    }))
    .unwrap();

    let plan = runtime_port_plan(&bundle, "openclaw").unwrap();
    let config = tng_config(&bundle, "openclaw", &plan).unwrap();

    assert_eq!(config["add_ingress"][0]["mapping"]["in"]["port"], 39400);
    assert_eq!(
        config["add_ingress"][0]["mapping"]["out"]["host"],
        "39.105.93.168"
    );
    assert_eq!(config["add_ingress"][0]["mapping"]["out"]["port"], 3001);
    assert_eq!(config["add_ingress"][0]["verify"]["as_type"], "builtin");
    assert_eq!(
        config["add_ingress"][0]["verify"]["attestation_policy"]["path"],
        DEFAULT_POLICY_PATH
    );
    assert!(config["add_ingress"][0]["verify"].get("policy").is_none());
    assert!(config["add_ingress"][0]["verify"]
        .get("policy_ids")
        .is_none());
    assert_eq!(config["add_ingress"][0]["rats_tls"]["multiplex"], true);
    assert_eq!(config["add_egress"][0]["rats_tls"]["multiplex"], true);
    assert_eq!(
        config["add_ingress"][0]["verify"]["reference_values"][0]["type"],
        "sample"
    );
    assert_eq!(config["add_ingress"][0]["attest"]["aa_type"], "uds");
    assert_eq!(config["add_ingress"][1]["mapping"]["in"]["port"], 39401);
    assert_eq!(config["add_ingress"][1]["mapping"]["out"]["port"], 3002);
    assert_eq!(config["add_ingress"][1]["verify"]["as_type"], "builtin");
    assert!(config["add_ingress"][1].get("attest").is_none());
    assert_eq!(config["add_ingress"][2]["mapping"]["in"]["port"], 39402);
    assert_eq!(config["add_ingress"][2]["mapping"]["out"]["port"], 4001);
    assert_eq!(config["add_ingress"][2]["verify"]["as_type"], "builtin");
    assert!(config["add_ingress"][2].get("attest").is_none());
    assert_eq!(config["add_ingress"].as_array().unwrap().len(), 3);
}

#[test]
fn gateway_config_routes_only_confidential_mesh_ports() {
    let bundle: MeshBundle = serde_json::from_value(json!({
        "schema": "confidential-agent/mesh-bundle/v1",
        "generation": 1,
        "updated_at": 0,
        "services": {
            "self": {
                "phase": "active",
                "private_ip": "10.0.1.10",
                "public_ip": "47.95.242.63",
                "ports": [18789, 18800, 18801],
                "connect": [18789],
                "mcp_ports": [18801],
                "gateway_public_key": "self-pub"
            },
            "peer": {
                "phase": "active",
                "private_ip": "10.0.1.11",
                "public_ip": "39.105.93.168",
                "ports": [3001, 3002, 3003],
                "connect": [3002],
                "mcp_ports": [3001],
                "gateway_public_key": "peer-pub"
            },
            "connect-only": {
                "phase": "active",
                "private_ip": "10.0.1.12",
                "public_ip": "39.105.93.169",
                "ports": [4001],
                "connect": [4001],
                "gateway_public_key": "connect-only-pub"
            }
        },
        "reference_values": {
            "peer": {"measurement.uki.SHA-384": ["abc123"]},
            "connect-only": {"measurement.uki.SHA-384": ["def456"]}
        },
        "rekor_reference_values": {}
    }))
    .unwrap();
    let bootstrap = test_bootstrap_config("self");
    let plan = runtime_port_plan(&bundle, "self").unwrap();

    let config = gateway_config(&bundle, "self", &bootstrap, &plan).unwrap();

    let server_routes = config["server_routes"].as_array().unwrap();
    assert_eq!(server_routes.len(), 2);
    assert_eq!(server_routes[0]["upstream_port"], 18800);
    assert_eq!(server_routes[0]["protocol"], "raw");
    assert_eq!(server_routes[1]["upstream_port"], 18801);
    assert_eq!(server_routes[1]["protocol"], "mcp");

    let client_routes = config["client_routes"].as_array().unwrap();
    assert_eq!(client_routes.len(), 2);
    assert_eq!(client_routes[0]["listen_port"], 3001);
    assert_eq!(client_routes[0]["target_port"], 3001);
    assert_eq!(client_routes[0]["tng_port"], 39401);
    assert_eq!(client_routes[1]["listen_port"], 3003);
    assert_eq!(client_routes[1]["target_port"], 3003);
    assert_eq!(client_routes[1]["tng_port"], 39403);
    assert_eq!(config["trusted_services"]["peer"]["public_key"], "peer-pub");
    assert_eq!(
        config["trusted_services"]["connect-only"]["public_key"],
        "connect-only-pub"
    );
}

#[test]
fn empty_gateway_config_allows_missing_gateway_identity() {
    let mut bootstrap = test_bootstrap_config("connect-only");
    bootstrap.gateway_identity = None;

    let config = empty_gateway_config(&bootstrap).unwrap();

    assert!(config.get("identity").is_none());
    assert!(config["client_routes"].as_array().unwrap().is_empty());
    assert!(config["server_routes"].as_array().unwrap().is_empty());
}

#[test]
fn resource_apply_is_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::write(&source, "v1").unwrap();
    let digest_v1 = sha256_file(&source).unwrap();
    let resource = GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: Some(digest_v1.clone()),
    };

    let first = apply_resource_once(&resource, &source, &digest_v1).unwrap();
    let second = apply_resource_once(&resource, &source, &digest_v1).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    let mode_fix = apply_resource_once(&resource, &source, &digest_v1).unwrap();
    fs::write(&source, "v2").unwrap();
    let digest_v2 = sha256_file(&source).unwrap();
    let third = apply_resource_once(&resource, &source, &digest_v2).unwrap();

    assert_eq!(first, ApplyOutcome::Updated);
    assert_eq!(second, ApplyOutcome::Unchanged);
    assert_eq!(mode_fix, ApplyOutcome::Updated);
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(third, ApplyOutcome::Updated);
}

#[test]
fn resource_apply_removes_stale_tmp_before_replace() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    let stale_tmp = target.with_extension("confidential-agent.tmp");
    fs::write(&source, "managed").unwrap();
    fs::write(&stale_tmp, "stale").unwrap();
    let digest = sha256_file(&source).unwrap();
    let resource = GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: Some(digest.clone()),
    };

    let outcome = apply_resource_once(&resource, &source, &digest).unwrap();

    assert_eq!(outcome, ApplyOutcome::Updated);
    assert_eq!(fs::read_to_string(target).unwrap(), "managed");
    assert!(!stale_tmp.exists());
}

#[test]
fn resource_apply_rejects_symlink_target() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    let linked = temp.path().join("linked");
    fs::write(&source, "managed").unwrap();
    fs::write(&linked, "managed").unwrap();
    fs::set_permissions(&linked, fs::Permissions::from_mode(0o644)).unwrap();
    symlink(&linked, &target).unwrap();
    let digest = sha256_file(&source).unwrap();
    let resource = GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: Some(digest.clone()),
    };

    let err = apply_resource_once(&resource, &source, &digest).unwrap_err();

    assert!(err.to_string().contains("invalid resource target"));
    assert!(fs::symlink_metadata(&target)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::metadata(&linked).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[test]
fn resource_apply_rejects_stale_tmp_symlink() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    let stale_tmp = target.with_extension("confidential-agent.tmp");
    let linked = temp.path().join("linked");
    fs::write(&source, "managed").unwrap();
    fs::write(&linked, "outside").unwrap();
    symlink(&linked, &stale_tmp).unwrap();
    let digest = sha256_file(&source).unwrap();
    let resource = GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: Some(digest.clone()),
    };

    let err = apply_resource_once(&resource, &source, &digest).unwrap_err();

    assert!(err.to_string().contains("invalid resource temporary path"));
    assert_eq!(fs::read_to_string(&linked).unwrap(), "outside");
    assert!(fs::symlink_metadata(&stale_tmp)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn resource_apply_rejects_mutable_symlink_target() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    let linked = temp.path().join("linked");
    fs::write(&source, "managed").unwrap();
    fs::write(&linked, "managed").unwrap();
    symlink(&linked, &target).unwrap();
    let digest = sha256_file(&source).unwrap();
    let resource = GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: true,
        sha256: Some(digest.clone()),
    };

    let err = apply_resource_once(&resource, &source, &digest).unwrap_err();

    assert!(err.to_string().contains("invalid resource target"));
    assert!(fs::symlink_metadata(&target)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn resource_apply_rejects_symlink_parent() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let real_parent = temp.path().join("real-parent");
    let link_parent = temp.path().join("link-parent");
    let target = link_parent.join("target");
    fs::write(&source, "managed").unwrap();
    fs::create_dir(&real_parent).unwrap();
    symlink(&real_parent, &link_parent).unwrap();
    let digest = sha256_file(&source).unwrap();
    let resource = GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: Some(digest.clone()),
    };

    let err = apply_resource_once(&resource, &source, &digest).unwrap_err();

    assert!(err.to_string().contains("invalid resource target"));
    assert!(!real_parent.join("target").exists());
}

#[test]
fn resource_apply_supports_numeric_owner_and_group() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::write(&source, "owned").unwrap();
    let digest = sha256_file(&source).unwrap();
    let resource = GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: Some(digest.clone()),
        owner: Some("65534".to_string()),
        group: Some("65534".to_string()),
    };

    let outcome = apply_resource_once(&resource, &source, &digest).unwrap();

    assert_eq!(outcome, ApplyOutcome::Updated);
    let metadata = fs::metadata(target).unwrap();
    assert_eq!(metadata.uid(), 65534);
    assert_eq!(metadata.gid(), 65534);
}

#[test]
fn bootstrap_reapplies_resource_when_target_content_drifted() {
    let temp = tempfile::tempdir().unwrap();
    let cdh_root = temp.path().join("cdh");
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "managed-v1").unwrap();
    fs::write(&target, "application-mutated").unwrap();
    let digest = sha256_file(&source).unwrap();
    let bootstrap = BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
        generation: 1,
        service_id: "openclaw".to_string(),
        mode: "challenge".to_string(),
        ports: Vec::new(),
        connect: Vec::new(),
        mcp_ports: Vec::new(),
        gateway_identity: None,
        resources: vec![GuestResource {
            id: "config".to_string(),
            resource_path: "default/local-resources/config".to_string(),
            target: target.clone(),
            owner: None,
            group: None,
            mode: "0600".to_string(),
            required: true,
            mutable: false,
            sha256: Some(digest.clone()),
        }],
        app_service: None,
        peers: Vec::new(),
        agent_card: None,
    };
    let args = RunArgs {
        cdh_root,
        bootstrap_resource: "default/local-resources/cagent_bootstrap_config".to_string(),
        mesh_resource: "default/local-resources/cagent_mesh_bundle".to_string(),
        a2a_bundle_resource: "default/local-resources/cagent_a2a_bundle".to_string(),
        poll_interval_sec: 5,
        status_listen: "127.0.0.1:0".to_string(),
        agent_card_listen: "127.0.0.1:0".to_string(),
    };
    let mut state = DaemonState::default();
    state.applied_resources.insert(
        "config".to_string(),
        AppliedResourceState {
            sha256: digest,
            target: target.clone(),
            owner: None,
            group: None,
            mode: "0600".to_string(),
            mutable: false,
        },
    );

    apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap();

    assert_eq!(fs::read_to_string(target).unwrap(), "managed-v1");
}

#[test]
fn bootstrap_seeds_mutable_resource_when_target_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let _env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    write_test_file(&source, b"seed-config");
    let digest = sha256_file(&source).unwrap();
    let mut bootstrap = test_bootstrap_config("hermes-agent");
    bootstrap.resources.push(GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: true,
        sha256: Some(digest.clone()),
    });
    let mut state = DaemonState::default();

    apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), "seed-config");
    assert!(state.applied_resources["config"].mutable);
    assert_eq!(state.applied_resources["config"].sha256, digest);
}

#[test]
fn bootstrap_does_not_reapply_mutable_resource_when_target_content_drifted() {
    let temp = tempfile::tempdir().unwrap();
    let _env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    write_test_file(&source, b"seed-config");
    write_test_file(&target, b"application-mutated");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    let source_digest = sha256_file(&source).unwrap();
    let target_digest = sha256_file(&target).unwrap();
    let mut bootstrap = test_bootstrap_config("hermes-agent");
    bootstrap.generation = 2;
    bootstrap.resources.push(GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: true,
        sha256: Some(source_digest),
    });
    let mut state = DaemonState {
        bootstrap_generation: 1,
        applied_resources: BTreeMap::from([(
            "config".to_string(),
            AppliedResourceState {
                sha256: target_digest.clone(),
                target: target.clone(),
                owner: None,
                group: None,
                mode: "0600".to_string(),
                mutable: true,
            },
        )]),
        ..DaemonState::default()
    };

    apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), "application-mutated");
    assert_eq!(state.bootstrap_generation, 2);
    assert_eq!(state.applied_resources["config"].sha256, target_digest);
}

#[test]
fn bootstrap_fixes_mutable_resource_metadata_without_overwriting_content() {
    let temp = tempfile::tempdir().unwrap();
    let _env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    write_test_file(&source, b"seed-config");
    write_test_file(&target, b"application-mutated");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    let source_digest = sha256_file(&source).unwrap();
    let target_digest = sha256_file(&target).unwrap();
    let mut bootstrap = test_bootstrap_config("hermes-agent");
    bootstrap.resources.push(GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: true,
        sha256: Some(source_digest),
    });
    let mut state = DaemonState {
        bootstrap_generation: 1,
        applied_resources: BTreeMap::from([(
            "config".to_string(),
            AppliedResourceState {
                sha256: target_digest.clone(),
                target: target.clone(),
                owner: None,
                group: None,
                mode: "0600".to_string(),
                mutable: true,
            },
        )]),
        ..DaemonState::default()
    };

    apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), "application-mutated");
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(state.applied_resources["config"].sha256, target_digest);
}

#[test]
fn bootstrap_fixes_mutable_metadata_without_stopping_app_service() {
    let env = EnvGuard::new(&["PATH", "CA_SKIP_SYSTEMCTL", "CA_DAEMON_STATUS_PATH"]);
    let temp = tempfile::tempdir().unwrap();
    let log_path = install_fake_systemctl(&env, &temp);

    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    write_test_file(&source, b"seed-config");
    write_test_file(&target, b"application-mutated");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    let source_digest = sha256_file(&source).unwrap();
    let mut bootstrap = test_bootstrap_config("hermes-agent");
    bootstrap.resources.push(GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: true,
        sha256: Some(source_digest),
    });
    bootstrap.app_service = Some("shelter-container-hermes-agent.service".to_string());
    let mut state = DaemonState {
        bootstrap_generation: 1,
        applied_resources: BTreeMap::from([(
            "config".to_string(),
            AppliedResourceState {
                sha256: "previous-audit-digest".to_string(),
                target: target.clone(),
                owner: None,
                group: None,
                mode: "0600".to_string(),
                mutable: true,
            },
        )]),
        ..DaemonState::default()
    };

    apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), "application-mutated");
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        state.applied_resources["config"].sha256,
        "previous-audit-digest"
    );
    let log = fs::read_to_string(log_path).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(
        commands,
        [
            "start --no-block shelter-container-hermes-agent.service",
            "is-active --quiet shelter-container-hermes-agent.service",
        ]
    );
}

#[test]
fn bootstrap_reseeds_mutable_resource_when_target_is_deleted() {
    let temp = tempfile::tempdir().unwrap();
    let _env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    write_test_file(&source, b"seed-config");
    let digest = sha256_file(&source).unwrap();
    let mut bootstrap = test_bootstrap_config("hermes-agent");
    bootstrap.generation = 2;
    bootstrap.resources.push(GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: true,
        sha256: Some(digest.clone()),
    });
    let mut state = DaemonState {
        bootstrap_generation: 1,
        applied_resources: BTreeMap::from([(
            "config".to_string(),
            AppliedResourceState {
                sha256: "previous-target-digest".to_string(),
                target: target.clone(),
                owner: None,
                group: None,
                mode: "0600".to_string(),
                mutable: true,
            },
        )]),
        ..DaemonState::default()
    };

    apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap();

    assert_eq!(fs::read_to_string(&target).unwrap(), "seed-config");
    assert_eq!(state.applied_resources["config"].sha256, digest);
}

#[test]
fn bootstrap_ready_accepts_mutable_resource_content_drift_after_apply() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("config.yaml");
    fs::write(&target, "application-mutated").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    let bootstrap = BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
        generation: 7,
        service_id: "hermes-agent".to_string(),
        mode: "challenge".to_string(),
        ports: Vec::new(),
        connect: Vec::new(),
        mcp_ports: Vec::new(),
        gateway_identity: None,
        resources: vec![GuestResource {
            id: "config".to_string(),
            resource_path: "default/local-resources/config".to_string(),
            target: target.clone(),
            owner: None,
            group: None,
            mode: "0600".to_string(),
            required: true,
            mutable: true,
            sha256: Some("seed-source-digest".to_string()),
        }],
        app_service: None,
        peers: Vec::new(),
        agent_card: None,
    };
    let state = DaemonState {
        bootstrap_generation: 7,
        applied_resources: BTreeMap::from([(
            "config".to_string(),
            AppliedResourceState {
                sha256: "old-target-digest".to_string(),
                target,
                owner: None,
                group: None,
                mode: "0600".to_string(),
                mutable: true,
            },
        )]),
        ..DaemonState::default()
    };

    assert!(bootstrap_resources_ready(&bootstrap, &state).unwrap());
}

#[test]
fn bootstrap_apply_stops_resource_backed_app_before_starting_it() {
    let env = EnvGuard::new(&["PATH", "CA_SKIP_SYSTEMCTL", "CA_DAEMON_STATUS_PATH"]);
    let temp = tempfile::tempdir().unwrap();
    let log_path = install_fake_systemctl(&env, &temp);

    let cdh_root = temp.path().join("cdh");
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    write_test_file(&source, b"managed");
    let digest = sha256_file(&source).unwrap();
    let mut bootstrap = test_bootstrap_config("hermes-agent");
    bootstrap.resources.push(GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: Some(digest),
    });
    bootstrap.app_service = Some("shelter-container-hermes-agent.service".to_string());
    let args = test_run_args(cdh_root);
    let mut state = DaemonState::default();

    apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap();

    assert_eq!(fs::read_to_string(target).unwrap(), "managed");
    let log = fs::read_to_string(log_path).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(
        commands[0],
        "is-active --quiet shelter-container-hermes-agent.service"
    );
    assert_eq!(commands[1], "stop shelter-container-hermes-agent.service");
    assert_eq!(
        commands[2],
        "reset-failed shelter-container-hermes-agent.service"
    );
    assert_eq!(
        commands[3],
        "start --no-block shelter-container-hermes-agent.service"
    );
    assert_eq!(
        commands[4],
        "is-active --quiet shelter-container-hermes-agent.service"
    );
}

#[test]
fn bootstrap_apply_writes_resource_when_app_service_unit_is_not_loaded() {
    let env = EnvGuard::new(&["PATH", "CA_SKIP_SYSTEMCTL", "CA_DAEMON_STATUS_PATH"]);
    let temp = tempfile::tempdir().unwrap();
    let fake_systemctl = temp.path().join("systemctl");
    let log_path = temp.path().join("systemctl.log");
    fs::write(
        &fake_systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = stop ]; then echo 'translated missing unit message' >&2; exit 5; fi\nexit 0\n",
            log_path.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    env.set(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    env.remove("CA_SKIP_SYSTEMCTL");
    env.set_path("CA_DAEMON_STATUS_PATH", &temp.path().join("status.json"));

    let cdh_root = temp.path().join("cdh");
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    write_test_file(&source, b"managed");
    let digest = sha256_file(&source).unwrap();
    let mut bootstrap = test_bootstrap_config("hermes-agent");
    bootstrap.resources.push(GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: Some(digest),
    });
    bootstrap.app_service = Some("shelter-container-hermes-agent.service".to_string());
    let args = test_run_args(cdh_root);
    let mut state = DaemonState::default();

    apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap();

    assert_eq!(fs::read_to_string(target).unwrap(), "managed");
    let log = fs::read_to_string(log_path).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(
        commands,
        [
            "is-active --quiet shelter-container-hermes-agent.service",
            "stop shelter-container-hermes-agent.service",
            "start --no-block shelter-container-hermes-agent.service",
            "is-active --quiet shelter-container-hermes-agent.service",
        ]
    );
}

#[test]
fn bootstrap_apply_missing_resource_does_not_stop_app_service() {
    let env = EnvGuard::new(&["PATH", "CA_SKIP_SYSTEMCTL", "CA_DAEMON_STATUS_PATH"]);
    let temp = tempfile::tempdir().unwrap();
    let log_path = install_fake_systemctl(&env, &temp);

    let cdh_root = temp.path().join("cdh");
    let target = temp.path().join("target");
    let mut bootstrap = test_bootstrap_config("hermes-agent");
    bootstrap.resources.push(GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target,
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: None,
    });
    bootstrap.app_service = Some("shelter-container-hermes-agent.service".to_string());
    let args = test_run_args(cdh_root);
    let mut state = DaemonState::default();

    assert!(!apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap());
    assert!(!log_path.exists());
}

#[test]
fn bootstrap_apply_digest_mismatch_does_not_stop_app_service() {
    let env = EnvGuard::new(&["PATH", "CA_SKIP_SYSTEMCTL", "CA_DAEMON_STATUS_PATH"]);
    let temp = tempfile::tempdir().unwrap();
    let log_path = install_fake_systemctl(&env, &temp);

    let cdh_root = temp.path().join("cdh");
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    write_test_file(&source, b"managed");
    let mut bootstrap = test_bootstrap_config("hermes-agent");
    bootstrap.resources.push(GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target,
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: Some("not-the-source-digest".to_string()),
    });
    bootstrap.app_service = Some("shelter-container-hermes-agent.service".to_string());
    let args = test_run_args(cdh_root);
    let mut state = DaemonState::default();

    let err = apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap_err();

    assert!(err.to_string().contains("digest mismatch"));
    assert!(!log_path.exists());
}

#[test]
fn bootstrap_apply_unchanged_resource_does_not_stop_app_service() {
    let env = EnvGuard::new(&["PATH", "CA_SKIP_SYSTEMCTL", "CA_DAEMON_STATUS_PATH"]);
    let temp = tempfile::tempdir().unwrap();
    let log_path = install_fake_systemctl(&env, &temp);

    let cdh_root = temp.path().join("cdh");
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    write_test_file(&source, b"managed");
    write_test_file(&target, b"managed");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    let digest = sha256_file(&source).unwrap();
    let mut bootstrap = test_bootstrap_config("hermes-agent");
    bootstrap.resources.push(GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target,
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: Some(digest),
    });
    bootstrap.app_service = Some("shelter-container-hermes-agent.service".to_string());
    let args = test_run_args(cdh_root);
    let mut state = DaemonState::default();

    assert!(apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap());

    let log = fs::read_to_string(log_path).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(
        commands,
        [
            "start --no-block shelter-container-hermes-agent.service",
            "is-active --quiet shelter-container-hermes-agent.service",
        ]
    );
}

#[test]
fn bootstrap_apply_restarts_app_service_when_resource_write_fails_after_stop() {
    let env = EnvGuard::new(&["PATH", "CA_SKIP_SYSTEMCTL", "CA_DAEMON_STATUS_PATH"]);
    let temp = tempfile::tempdir().unwrap();
    let log_path = install_fake_systemctl(&env, &temp);

    let cdh_root = temp.path().join("cdh");
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    let stale_tmp = target.with_extension("confidential-agent.tmp");
    write_test_file(&source, b"managed");
    fs::create_dir(&stale_tmp).unwrap();
    let digest = sha256_file(&source).unwrap();
    let mut bootstrap = test_bootstrap_config("hermes-agent");
    bootstrap.resources.push(GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target,
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: Some(digest),
    });
    bootstrap.app_service = Some("shelter-container-hermes-agent.service".to_string());
    let args = test_run_args(cdh_root);
    let mut state = DaemonState::default();

    let err = apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap_err();

    assert!(format!("{err:#}").contains("failed to remove stale"));
    assert_eq!(state.bootstrap_generation, 0);
    let log = fs::read_to_string(log_path).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(
        commands,
        [
            "is-active --quiet shelter-container-hermes-agent.service",
            "stop shelter-container-hermes-agent.service",
            "reset-failed shelter-container-hermes-agent.service",
            "start --no-block shelter-container-hermes-agent.service",
        ]
    );
}

#[test]
fn bootstrap_apply_does_not_start_inactive_app_when_resource_write_fails_after_stop() {
    let env = EnvGuard::new(&["PATH", "CA_SKIP_SYSTEMCTL", "CA_DAEMON_STATUS_PATH"]);
    let temp = tempfile::tempdir().unwrap();
    let fake_systemctl = temp.path().join("systemctl");
    let log_path = temp.path().join("systemctl.log");
    fs::write(
        &fake_systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  'is-active --quiet shelter-container-hermes-agent.service') exit 3 ;;\n  'start --no-block shelter-container-hermes-agent.service') exit 99 ;;\nesac\nexit 0\n",
            log_path.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    env.set(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    env.remove("CA_SKIP_SYSTEMCTL");
    env.set_path("CA_DAEMON_STATUS_PATH", &temp.path().join("status.json"));

    let cdh_root = temp.path().join("cdh");
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    let stale_tmp = target.with_extension("confidential-agent.tmp");
    write_test_file(&source, b"managed");
    fs::create_dir(&stale_tmp).unwrap();
    let digest = sha256_file(&source).unwrap();
    let mut bootstrap = test_bootstrap_config("hermes-agent");
    bootstrap.resources.push(GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target,
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: Some(digest),
    });
    bootstrap.app_service = Some("shelter-container-hermes-agent.service".to_string());
    let args = test_run_args(cdh_root);
    let mut state = DaemonState::default();

    let err = apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap_err();

    assert!(format!("{err:#}").contains("failed to remove stale"));
    let log = fs::read_to_string(log_path).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(
        commands,
        [
            "is-active --quiet shelter-container-hermes-agent.service",
            "stop shelter-container-hermes-agent.service",
            "reset-failed shelter-container-hermes-agent.service",
        ]
    );
}

#[test]
fn bootstrap_apply_does_not_write_resource_when_app_service_stop_fails() {
    let env = EnvGuard::new(&["PATH", "CA_SKIP_SYSTEMCTL", "CA_DAEMON_STATUS_PATH"]);
    let temp = tempfile::tempdir().unwrap();
    let fake_systemctl = temp.path().join("systemctl");
    let log_path = temp.path().join("systemctl.log");
    fs::write(
        &fake_systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = stop ]; then echo 'permission denied' >&2; exit 1; fi\nexit 0\n",
            log_path.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    env.set(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    env.remove("CA_SKIP_SYSTEMCTL");
    env.set_path("CA_DAEMON_STATUS_PATH", &temp.path().join("status.json"));

    let cdh_root = temp.path().join("cdh");
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    write_test_file(&source, b"managed");
    let digest = sha256_file(&source).unwrap();
    let mut bootstrap = test_bootstrap_config("hermes-agent");
    bootstrap.resources.push(GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: Some(digest),
    });
    bootstrap.app_service = Some("shelter-container-hermes-agent.service".to_string());
    let args = test_run_args(cdh_root);
    let mut state = DaemonState::default();

    let err = apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap_err();

    assert!(err.to_string().contains("systemctl stop"));
    assert!(!target.exists());
    let log = fs::read_to_string(log_path).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(
        commands,
        [
            "is-active --quiet shelter-container-hermes-agent.service",
            "stop shelter-container-hermes-agent.service",
        ]
    );
}

#[test]
fn bootstrap_rejects_oversized_resource() {
    let temp = tempfile::tempdir().unwrap();
    let cdh_root = temp.path().join("cdh");
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let file = fs::File::create(&source).unwrap();
    file.set_len(MAX_RESOURCE_BYTES + 1).unwrap();
    drop(file);
    let bootstrap = BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
        generation: 1,
        service_id: "openclaw".to_string(),
        mode: "challenge".to_string(),
        ports: Vec::new(),
        connect: Vec::new(),
        mcp_ports: Vec::new(),
        gateway_identity: None,
        resources: vec![GuestResource {
            id: "config".to_string(),
            resource_path: "default/local-resources/config".to_string(),
            target,
            owner: None,
            group: None,
            mode: "0600".to_string(),
            required: true,
            mutable: false,
            sha256: None,
        }],
        app_service: None,
        peers: Vec::new(),
        agent_card: None,
    };
    let args = RunArgs {
        cdh_root,
        bootstrap_resource: "default/local-resources/cagent_bootstrap_config".to_string(),
        mesh_resource: "default/local-resources/cagent_mesh_bundle".to_string(),
        a2a_bundle_resource: "default/local-resources/cagent_a2a_bundle".to_string(),
        poll_interval_sec: 5,
        status_listen: "127.0.0.1:0".to_string(),
        agent_card_listen: "127.0.0.1:0".to_string(),
    };
    let mut state = DaemonState::default();

    let err = apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap_err();

    assert!(err.to_string().contains("exceeding maximum"));
}

#[test]
fn bootstrap_rejects_non_regular_resource_even_when_empty() {
    let temp = tempfile::tempdir().unwrap();
    let cdh_root = temp.path().join("cdh");
    let source = cdh_root.join("default/local-resources/config");
    let target = temp.path().join("target");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&source).unwrap();
    let bootstrap = BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
        generation: 1,
        service_id: "openclaw".to_string(),
        mode: "challenge".to_string(),
        ports: Vec::new(),
        connect: Vec::new(),
        mcp_ports: Vec::new(),
        gateway_identity: None,
        resources: vec![GuestResource {
            id: "config".to_string(),
            resource_path: "default/local-resources/config".to_string(),
            target,
            owner: None,
            group: None,
            mode: "0600".to_string(),
            required: true,
            mutable: false,
            sha256: None,
        }],
        app_service: None,
        peers: Vec::new(),
        agent_card: None,
    };
    let args = RunArgs {
        cdh_root,
        bootstrap_resource: "default/local-resources/cagent_bootstrap_config".to_string(),
        mesh_resource: "default/local-resources/cagent_mesh_bundle".to_string(),
        a2a_bundle_resource: "default/local-resources/cagent_a2a_bundle".to_string(),
        poll_interval_sec: 5,
        status_listen: "127.0.0.1:0".to_string(),
        agent_card_listen: "127.0.0.1:0".to_string(),
    };
    let mut state = DaemonState::default();

    let err = apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap_err();

    drop(listener);
    assert!(err.to_string().contains("not a regular file"));
}

#[test]
fn json_atomic_write_replaces_content_without_tmp_leftover() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.json");
    write_json_atomic(&path, &json!({"generation": 1})).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    write_json_atomic(&path, &json!({"generation": 2})).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(parsed["generation"], 2);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert!(!path.with_extension("confidential-agent.tmp").exists());
}

#[test]
fn bootstrap_ready_requires_matching_generation_and_resource_digests() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("openclaw.json");
    fs::write(&target, "{}").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    let digest = sha256_file(&target).unwrap();
    let bootstrap = BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
        generation: 7,
        service_id: "openclaw".to_string(),
        mode: "challenge".to_string(),
        ports: Vec::new(),
        connect: Vec::new(),
        mcp_ports: Vec::new(),
        gateway_identity: None,
        resources: vec![GuestResource {
            id: "config".to_string(),
            resource_path: "default/local-resources/config".to_string(),
            target: target.clone(),
            owner: None,
            group: None,
            mode: "0600".to_string(),
            required: true,
            mutable: false,
            sha256: Some(digest.clone()),
        }],
        app_service: None,
        peers: Vec::new(),
        agent_card: None,
    };
    let mut state = DaemonState {
        bootstrap_generation: 7,
        applied_resources: BTreeMap::new(),
        mesh_fingerprint: None,
        ..DaemonState::default()
    };
    state.applied_resources.insert(
        "config".to_string(),
        AppliedResourceState {
            sha256: digest,
            target,
            owner: None,
            group: None,
            mode: "0600".to_string(),
            mutable: false,
        },
    );

    assert!(bootstrap_resources_ready(&bootstrap, &state).unwrap());
    state.bootstrap_generation = 6;
    assert!(!bootstrap_resources_ready(&bootstrap, &state).unwrap());
    state.bootstrap_generation = 7;
    state.applied_resources.get_mut("config").unwrap().sha256 = "def".to_string();
    assert!(!bootstrap_resources_ready(&bootstrap, &state).unwrap());
}

#[test]
fn bootstrap_ready_accepts_present_digestless_resources() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("openclaw.json");
    fs::write(&target, "{}").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    let digest = sha256_file(&target).unwrap();
    let bootstrap = BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
        generation: 7,
        service_id: "openclaw".to_string(),
        mode: "challenge".to_string(),
        ports: Vec::new(),
        connect: Vec::new(),
        mcp_ports: Vec::new(),
        gateway_identity: None,
        resources: vec![GuestResource {
            id: "config".to_string(),
            resource_path: "default/local-resources/config".to_string(),
            target: target.clone(),
            owner: None,
            group: None,
            mode: "0600".to_string(),
            required: true,
            mutable: false,
            sha256: None,
        }],
        app_service: None,
        peers: Vec::new(),
        agent_card: None,
    };
    let mut state = DaemonState {
        bootstrap_generation: 7,
        applied_resources: BTreeMap::new(),
        mesh_fingerprint: None,
        ..DaemonState::default()
    };
    state.applied_resources.insert(
        "config".to_string(),
        AppliedResourceState {
            sha256: digest,
            target,
            owner: None,
            group: None,
            mode: "0600".to_string(),
            mutable: false,
        },
    );

    assert!(bootstrap_resources_ready(&bootstrap, &state).unwrap());
    state.applied_resources.clear();
    assert!(!bootstrap_resources_ready(&bootstrap, &state).unwrap());
}

#[test]
fn no_tee_bootstrap_apply_writes_managed_resource_and_status() {
    let temp = tempfile::tempdir().unwrap();
    let env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let source = cdh_root.join("default/local-resources/openclaw.json");
    write_test_file(&source, b"{\"managed\":true}\n");
    let digest = sha256_file(&source).unwrap();
    let target = temp.path().join("managed/openclaw.json");
    let mut bootstrap = test_bootstrap_config("openclaw");
    bootstrap.resources.push(GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/openclaw.json".to_string(),
        target: target.clone(),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: Some(digest.clone()),
    });
    let mut state = DaemonState::default();

    assert!(apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap());

    assert_eq!(fs::read_to_string(&target).unwrap(), "{\"managed\":true}\n");
    assert_eq!(state.applied_resources["config"].sha256, digest);
    let status = read_status_file(&env);
    assert_eq!(status.phase, "resources-applied");
    assert_eq!(status.bootstrap_generation, 1);
    assert_eq!(status.applied_resources["config"].sha256, digest);
    assert_eq!(status.applied_resources["config"].target, target);
}

#[test]
fn no_tee_sync_mesh_after_bootstrap_writes_config_directory_and_status() {
    let temp = tempfile::tempdir().unwrap();
    let env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let mut bootstrap = test_bootstrap_config("openclaw");
    bootstrap.ports = vec![18789, 18800];
    bootstrap.connect = vec![confidential_agent_core::spec::ConnectPort::Plain(18789)];
    bootstrap.mcp_ports = vec![18800];
    let mut state = DaemonState::default();
    assert!(apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap());
    write_json_fixture(
        &cdh_root.join(TEST_MESH_RESOURCE),
        &json!({
            "schema": MESH_SCHEMA_VERSION,
            "generation": 42,
            "updated_at": 1,
            "services": {
                "openclaw": {
                    "phase": "active",
                    "private_ip": "10.0.0.10",
                    "public_ip": "127.0.0.1",
                    "ports": [18789, 18800],
                    "connect": [18789],
                    "mcp_ports": [18800],
                    "gateway_public_key": "openclaw-pub"
                },
                "mcp": {
                    "phase": "active",
                    "private_ip": "10.0.0.11",
                    "public_ip": "127.0.0.1",
                    "ports": [3001],
                    "connect": [],
                    "mcp_ports": [3001],
                    "gateway_public_key": "mcp-pub"
                }
            },
            "reference_values": {
                "mcp": {"measurement.uki.SHA-384": ["peer-rv"]}
            },
            "rekor_reference_values": {}
        }),
    );

    sync_mesh(&args, &bootstrap, &mut state).unwrap();

    let config: Value = read_json_file(&env.tng_config);
    assert_eq!(
        config["add_egress"][0]["netfilter"]["capture_dst"]["port"],
        18789
    );
    assert_eq!(config["add_egress"][0]["netfilter"]["listen_port"], 39000);
    assert_eq!(config["add_egress"][0]["netfilter"]["so_mark"], 565);
    assert!(config["add_egress"][0].get("verify").is_none());
    assert_eq!(
        config["add_egress"][1]["netfilter"]["capture_dst"]["port"],
        18800
    );
    assert_eq!(config["add_egress"][1]["verify"]["as_type"], "builtin");
    assert_eq!(config["add_ingress"][0]["mapping"]["in"]["port"], 39400);
    assert_eq!(
        config["add_ingress"][0]["mapping"]["out"]["host"],
        "127.0.0.1"
    );
    assert_eq!(config["add_ingress"][0]["mapping"]["out"]["port"], 3001);
    assert_eq!(
        config["add_ingress"][0]["verify"]["reference_values"][0]["type"],
        "sample"
    );
    let directory: ServiceDirectory = read_json_file(&env.service_directory);
    assert_eq!(directory.services["mcp"].ports[0].address, "127.0.0.1");
    assert_eq!(directory.services["mcp"].ports[0].port, 3001);
    assert_eq!(
        directory.services["mcp"].ports[0].mode.as_deref(),
        Some("mesh")
    );
    assert_eq!(
        directory.services["mcp"].ports[0].protocol.as_deref(),
        Some("mcp")
    );
    let gateway: Value = read_json_file(&env.gateway_config);
    assert_eq!(
        fs::metadata(&env.gateway_config)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(gateway["service_id"], "openclaw");
    assert_eq!(gateway["server_routes"].as_array().unwrap().len(), 1);
    assert_eq!(gateway["server_routes"][0]["listen_port"], 39201);
    assert_eq!(gateway["server_routes"][0]["upstream_port"], 18800);
    assert_eq!(gateway["server_routes"][0]["protocol"], "mcp");
    assert_eq!(gateway["client_routes"].as_array().unwrap().len(), 1);
    assert_eq!(gateway["client_routes"][0]["listen_port"], 3001);
    assert_eq!(gateway["client_routes"][0]["tng_port"], 39400);
    assert_eq!(gateway["trusted_services"]["mcp"]["public_key"], "mcp-pub");
    assert!(env.cache_dir.join("mesh-bundle.json").exists());
    let status = read_status_file(&env);
    assert!(matches!(status.phase.as_str(), "starting-mesh" | "running"));
    assert_eq!(status.mesh_generation, 42);
}

#[test]
fn sync_mesh_restarts_tng_when_config_unchanged_but_service_inactive() {
    let temp = tempfile::tempdir().unwrap();
    let env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let mut bootstrap = test_bootstrap_config("openclaw");
    bootstrap.ports = vec![18789];
    bootstrap.connect = Vec::new();
    let mut state = DaemonState::default();
    let _control = std::net::TcpListener::bind(("127.0.0.1", TNG_CONTROL_PORT)).unwrap();
    let _gateway = std::net::TcpListener::bind(("127.0.0.1", GATEWAY_SERVER_PORT_BASE)).unwrap();
    write_json_fixture(
        &cdh_root.join(TEST_MESH_RESOURCE),
        &json!({
            "schema": MESH_SCHEMA_VERSION,
            "generation": 42,
            "updated_at": 1,
            "services": {
                "openclaw": {
                    "phase": "active",
                    "private_ip": "10.0.0.10",
                    "public_ip": "127.0.0.1",
                    "ports": [18789],
                    "connect": [],
                    "mcp_ports": [],
                    "gateway_public_key": "openclaw-pub"
                }
            },
            "reference_values": {},
            "rekor_reference_values": {}
        }),
    );
    sync_mesh(&args, &bootstrap, &mut state).unwrap();
    let mesh_fingerprint = state.mesh_fingerprint.clone();
    let gateway_fingerprint = state.gateway_fingerprint.clone();
    state.mesh_last_restart_unix = None;

    let fake_systemctl = temp.path().join("systemctl");
    let systemctl_log = temp.path().join("systemctl.log");
    let active_marker = temp.path().join("tng.active");
    fs::write(
        &fake_systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  'is-active --quiet trusted-network-gateway.service') [ -f '{}' ] && exit 0 || exit 3 ;;\n  'restart trusted-network-gateway.service') touch '{}'; exit 0 ;;\n  'daemon-reload') exit 0 ;;\n  'reset-failed trusted-network-gateway.service') exit 0 ;;\nesac\nexit 0\n",
            systemctl_log.display(),
            active_marker.display(),
            active_marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    env._guard.set(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    env._guard.remove("CA_SKIP_SYSTEMCTL");

    sync_mesh(&args, &bootstrap, &mut state).unwrap();

    assert_eq!(state.mesh_fingerprint, mesh_fingerprint);
    assert_eq!(state.gateway_fingerprint, gateway_fingerprint);
    let log = fs::read_to_string(systemctl_log).unwrap();
    let commands: Vec<&str> = log.lines().collect();
    assert_eq!(
        commands,
        vec![
            "is-active --quiet cai-gateway.service",
            "is-active --quiet trusted-network-gateway.service",
            "daemon-reload",
            "reset-failed trusted-network-gateway.service",
            "restart trusted-network-gateway.service",
            "is-active --quiet trusted-network-gateway.service",
        ]
    );
}

#[test]
fn sync_mesh_throttles_tng_restart_when_service_stays_inactive() {
    let temp = tempfile::tempdir().unwrap();
    let env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let mut bootstrap = test_bootstrap_config("openclaw");
    bootstrap.ports = vec![18789];
    let mut state = DaemonState::default();
    write_json_fixture(
        &cdh_root.join(TEST_MESH_RESOURCE),
        &json!({
            "schema": MESH_SCHEMA_VERSION,
            "generation": 42,
            "updated_at": 1,
            "services": {
                "openclaw": {
                    "phase": "active",
                    "private_ip": "10.0.0.10",
                    "public_ip": "127.0.0.1",
                    "ports": [18789],
                    "connect": [],
                    "mcp_ports": [],
                    "gateway_public_key": "openclaw-pub"
                }
            },
            "reference_values": {},
            "rekor_reference_values": {}
        }),
    );
    let fake_systemctl = temp.path().join("systemctl");
    let systemctl_log = temp.path().join("systemctl.log");
    fs::write(
        &fake_systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  'is-active --quiet trusted-network-gateway.service') exit 3 ;;\n  'restart trusted-network-gateway.service') exit 0 ;;\n  'is-active --quiet cai-gateway.service') exit 0 ;;\n  'daemon-reload') exit 0 ;;\n  'reset-failed '*) exit 0 ;;\n  'restart '*) exit 0 ;;\nesac\nexit 0\n",
            systemctl_log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    env._guard.set(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    env._guard.remove("CA_SKIP_SYSTEMCTL");

    sync_mesh(&args, &bootstrap, &mut state).unwrap();
    sync_mesh(&args, &bootstrap, &mut state).unwrap();

    let log = fs::read_to_string(systemctl_log).unwrap();
    let tng_restarts = log
        .lines()
        .filter(|line| *line == "restart trusted-network-gateway.service")
        .count();
    assert_eq!(tng_restarts, 1);
    assert!(state.mesh_fingerprint.is_none());
}

#[test]
fn sync_mesh_clears_tng_config_when_mesh_and_a2a_removed() {
    let temp = tempfile::tempdir().unwrap();
    let env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let mut bootstrap = test_bootstrap_config("openclaw");
    bootstrap.ports = vec![18789];
    let mut state = DaemonState::default();
    let mesh_path = cdh_root.join(TEST_MESH_RESOURCE);
    write_json_fixture(
        &mesh_path,
        &json!({
            "schema": MESH_SCHEMA_VERSION,
            "generation": 42,
            "updated_at": 1,
            "services": {
                "openclaw": {
                    "phase": "active",
                    "private_ip": "10.0.0.10",
                    "public_ip": "127.0.0.1",
                    "ports": [18789],
                    "connect": [],
                    "mcp_ports": [],
                    "gateway_public_key": "openclaw-pub"
                },
                "remote": {
                    "phase": "active",
                    "private_ip": "10.0.0.20",
                    "public_ip": "127.0.0.1",
                    "ports": [3001],
                    "connect": [],
                    "mcp_ports": [],
                    "gateway_public_key": "remote-pub"
                }
            },
            "reference_values": {
                "remote": {"measurement.uki.SHA-384": ["remote-rv"]}
            },
            "rekor_reference_values": {}
        }),
    );

    sync_mesh(&args, &bootstrap, &mut state).unwrap();

    let mesh_config: Value = read_json_file(&env.tng_config);
    assert!(!mesh_config["add_ingress"].as_array().unwrap().is_empty());
    fs::remove_file(&mesh_path).unwrap();

    sync_mesh(&args, &bootstrap, &mut state).unwrap();

    let cleared_config: Value = read_json_file(&env.tng_config);
    assert!(cleared_config["add_ingress"].as_array().unwrap().is_empty());
    assert!(cleared_config["add_egress"].as_array().unwrap().is_empty());
    let directory: ServiceDirectory = read_json_file(&env.service_directory);
    assert!(directory.services.is_empty());
    let status = read_status_file(&env);
    assert_eq!(status.phase, "waiting-mesh");
    assert_eq!(status.mesh_generation, 0);
    assert_eq!(state.mesh_generation, 0);
}

#[test]
fn sync_mesh_waits_for_tng_ports_without_flapping_active_service() {
    let temp = tempfile::tempdir().unwrap();
    let env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let mut bootstrap = test_bootstrap_config("openclaw");
    bootstrap.ports = vec![18789];
    let mut state = DaemonState::default();
    write_json_fixture(
        &cdh_root.join(TEST_MESH_RESOURCE),
        &json!({
            "schema": MESH_SCHEMA_VERSION,
            "generation": 42,
            "updated_at": 1,
            "services": {
                "openclaw": {
                    "phase": "active",
                    "private_ip": "10.0.0.10",
                    "public_ip": "127.0.0.1",
                    "ports": [18789],
                    "connect": [],
                    "mcp_ports": [],
                    "gateway_public_key": "openclaw-pub"
                }
            },
            "reference_values": {},
            "rekor_reference_values": {}
        }),
    );
    let fake_systemctl = temp.path().join("systemctl");
    let systemctl_log = temp.path().join("systemctl.log");
    let active_marker = temp.path().join("tng.active");
    fs::write(
        &fake_systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  'is-active --quiet trusted-network-gateway.service') [ -f '{}' ] && exit 0 || exit 3 ;;\n  'restart trusted-network-gateway.service') touch '{}'; exit 0 ;;\n  'daemon-reload') exit 0 ;;\n  'reset-failed '*) exit 0 ;;\n  'restart '*) exit 0 ;;\nesac\nexit 0\n",
            systemctl_log.display(),
            active_marker.display(),
            active_marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    env._guard.set(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    env._guard.remove("CA_SKIP_SYSTEMCTL");

    sync_mesh(&args, &bootstrap, &mut state).unwrap();
    assert!(state.mesh_fingerprint.is_none());
    sync_mesh(&args, &bootstrap, &mut state).unwrap();
    assert!(state.mesh_fingerprint.is_none());

    let log = fs::read_to_string(systemctl_log).unwrap();
    let tng_restarts = log
        .lines()
        .filter(|line| *line == "restart trusted-network-gateway.service")
        .count();
    assert_eq!(tng_restarts, 1);
}

#[test]
fn sync_mesh_throttles_gateway_restart_when_service_stays_inactive() {
    let temp = tempfile::tempdir().unwrap();
    let env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let mut bootstrap = test_bootstrap_config("openclaw");
    bootstrap.ports = vec![18789];
    let mut state = DaemonState::default();
    let _control = std::net::TcpListener::bind(("127.0.0.1", TNG_CONTROL_PORT)).unwrap();
    write_json_fixture(
        &cdh_root.join(TEST_MESH_RESOURCE),
        &json!({
            "schema": MESH_SCHEMA_VERSION,
            "generation": 42,
            "updated_at": 1,
            "services": {
                "openclaw": {
                    "phase": "active",
                    "private_ip": "10.0.0.10",
                    "public_ip": "127.0.0.1",
                    "ports": [18789],
                    "connect": [],
                    "mcp_ports": [],
                    "gateway_public_key": "openclaw-pub"
                }
            },
            "reference_values": {},
            "rekor_reference_values": {}
        }),
    );
    let fake_systemctl = temp.path().join("systemctl");
    let systemctl_log = temp.path().join("systemctl.log");
    fs::write(
        &fake_systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  'is-active --quiet cai-gateway.service') exit 3 ;;\n  'restart cai-gateway.service') exit 0 ;;\n  'is-active --quiet trusted-network-gateway.service') exit 0 ;;\n  'daemon-reload') exit 0 ;;\n  'reset-failed '*) exit 0 ;;\n  'restart '*) exit 0 ;;\nesac\nexit 0\n",
            systemctl_log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    env._guard.set(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    env._guard.remove("CA_SKIP_SYSTEMCTL");

    sync_mesh(&args, &bootstrap, &mut state).unwrap();
    sync_mesh(&args, &bootstrap, &mut state).unwrap();

    let log = fs::read_to_string(systemctl_log).unwrap();
    let gateway_restarts = log
        .lines()
        .filter(|line| *line == "restart cai-gateway.service")
        .count();
    assert_eq!(gateway_restarts, 1);
    assert!(state.gateway_fingerprint.is_none());
}

#[test]
fn sync_mesh_waits_for_gateway_ports_without_flapping_active_service() {
    let temp = tempfile::tempdir().unwrap();
    let env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let mut bootstrap = test_bootstrap_config("openclaw");
    bootstrap.ports = vec![18789];
    let mut state = DaemonState::default();
    let _control = std::net::TcpListener::bind(("127.0.0.1", TNG_CONTROL_PORT)).unwrap();
    write_json_fixture(
        &cdh_root.join(TEST_MESH_RESOURCE),
        &json!({
            "schema": MESH_SCHEMA_VERSION,
            "generation": 42,
            "updated_at": 1,
            "services": {
                "openclaw": {
                    "phase": "active",
                    "private_ip": "10.0.0.10",
                    "public_ip": "127.0.0.1",
                    "ports": [18789],
                    "connect": [],
                    "mcp_ports": [],
                    "gateway_public_key": "openclaw-pub"
                }
            },
            "reference_values": {},
            "rekor_reference_values": {}
        }),
    );
    let fake_systemctl = temp.path().join("systemctl");
    let systemctl_log = temp.path().join("systemctl.log");
    let gateway_active_marker = temp.path().join("gateway.active");
    let tng_active_marker = temp.path().join("tng.active");
    fs::write(&tng_active_marker, "1").unwrap();
    fs::write(
        &fake_systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  'is-active --quiet cai-gateway.service') [ -f '{}' ] && exit 0 || exit 3 ;;\n  'restart cai-gateway.service') touch '{}'; exit 0 ;;\n  'is-active --quiet trusted-network-gateway.service') [ -f '{}' ] && exit 0 || exit 3 ;;\n  'restart trusted-network-gateway.service') touch '{}'; exit 0 ;;\n  'daemon-reload') exit 0 ;;\n  'reset-failed '*) exit 0 ;;\nesac\nexit 0\n",
            systemctl_log.display(),
            gateway_active_marker.display(),
            gateway_active_marker.display(),
            tng_active_marker.display(),
            tng_active_marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    env._guard.set(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    env._guard.remove("CA_SKIP_SYSTEMCTL");

    sync_mesh(&args, &bootstrap, &mut state).unwrap();
    assert!(state.gateway_fingerprint.is_none());
    sync_mesh(&args, &bootstrap, &mut state).unwrap();
    assert!(state.gateway_fingerprint.is_none());

    let log = fs::read_to_string(systemctl_log).unwrap();
    let gateway_restarts = log
        .lines()
        .filter(|line| *line == "restart cai-gateway.service")
        .count();
    assert_eq!(gateway_restarts, 1);
}

#[test]
fn sync_mesh_skips_tng_restart_when_config_unchanged_and_service_active() {
    let temp = tempfile::tempdir().unwrap();
    let env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let mut bootstrap = test_bootstrap_config("openclaw");
    bootstrap.ports = vec![18789];
    bootstrap.connect = Vec::new();
    let mut state = DaemonState::default();
    let _control = std::net::TcpListener::bind(("127.0.0.1", TNG_CONTROL_PORT)).unwrap();
    let _gateway = std::net::TcpListener::bind(("127.0.0.1", GATEWAY_SERVER_PORT_BASE)).unwrap();
    write_json_fixture(
        &cdh_root.join(TEST_MESH_RESOURCE),
        &json!({
            "schema": MESH_SCHEMA_VERSION,
            "generation": 42,
            "updated_at": 1,
            "services": {
                "openclaw": {
                    "phase": "active",
                    "private_ip": "10.0.0.10",
                    "public_ip": "127.0.0.1",
                    "ports": [18789],
                    "connect": [],
                    "mcp_ports": [],
                    "gateway_public_key": "openclaw-pub"
                }
            },
            "reference_values": {},
            "rekor_reference_values": {}
        }),
    );
    sync_mesh(&args, &bootstrap, &mut state).unwrap();

    let fake_systemctl = temp.path().join("systemctl");
    let systemctl_log = temp.path().join("systemctl.log");
    let active_marker = temp.path().join("tng.active");
    fs::write(&active_marker, "1").unwrap();
    fs::write(
        &fake_systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n  'is-active --quiet trusted-network-gateway.service') [ -f '{}' ] && exit 0 || exit 3 ;;\n  'restart trusted-network-gateway.service') exit 99 ;;\nesac\nexit 0\n",
            systemctl_log.display(),
            active_marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    env._guard.set(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    env._guard.remove("CA_SKIP_SYSTEMCTL");

    sync_mesh(&args, &bootstrap, &mut state).unwrap();

    let log = fs::read_to_string(systemctl_log).unwrap();
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        vec![
            "is-active --quiet cai-gateway.service",
            "is-active --quiet trusted-network-gateway.service",
        ]
    );
}

#[test]
fn http_status_serves_status_file_not_health_response() {
    let temp = tempfile::tempdir().unwrap();
    let env = no_tee_test_env(&temp);
    let body = r#"{"sentinel":"from-status-file"}"#;
    write_test_file(&env.status, body.as_bytes());

    let status_response = http_roundtrip(
        HttpServerKind::Status,
        b"GET /status HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );
    let health_response = http_roundtrip(
        HttpServerKind::Status,
        b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );

    assert!(status_response.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(http_response_body(&status_response), body);
    assert!(health_response.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(http_response_body(&health_response), r#"{"status":"ok"}"#);
}

#[test]
fn no_tee_bootstrap_agent_card_writes_file_and_http_serves_it() {
    let temp = tempfile::tempdir().unwrap();
    let env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root);
    let mut bootstrap = test_bootstrap_config("openclaw");
    bootstrap.agent_card = Some(test_agent_card("openclaw-card", &[18789]));
    let mut state = DaemonState::default();

    assert!(apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap());

    let written: confidential_agent_core::schema::AgentCard = read_json_file(&env.agent_card);
    assert_eq!(written.name, "openclaw-card");
    let response = http_roundtrip(
        HttpServerKind::AgentCard,
        b"GET /.well-known/agent-card.json HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    let served: confidential_agent_core::schema::AgentCard =
        serde_json::from_str(http_response_body(&response)).unwrap();
    assert_eq!(served.name, "openclaw-card");
    assert_eq!(
        served.supported_interfaces[0].url,
        "http://127.0.0.1:18789/a2a"
    );
}

#[test]
fn no_tee_sync_mesh_a2a_peer_stale_fallback_persists_state_and_status() {
    let temp = tempfile::tempdir().unwrap();
    let env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let mut bootstrap = test_bootstrap_config("openclaw");
    bootstrap.ports = vec![18789];
    bootstrap.connect = vec![confidential_agent_core::spec::ConnectPort::Plain(18789)];
    let mut state = DaemonState::default();
    assert!(apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap());
    let card = test_agent_card("remote-agent", &[3001]);
    let url = serve_agent_card_then_status(card, 503, "unavailable");
    let bundle = test_a2a_bundle(Some("remote"), &url, &[]);
    write_json_fixture(&cdh_root.join(TEST_A2A_BUNDLE_RESOURCE), &bundle);

    sync_mesh(&args, &bootstrap, &mut state).unwrap();
    write_daemon_state(&state).unwrap();

    let config: Value = read_json_file(&env.tng_config);
    assert_eq!(config["add_ingress"][0]["mapping"]["in"]["port"], 3001);
    assert_eq!(config["add_ingress"][0]["mapping"]["out"]["port"], 3001);
    let directory: ServiceDirectory = read_json_file(&env.service_directory);
    assert_eq!(directory.services["remote"].ports[0].port, 3001);
    let status = read_status_file(&env);
    assert_eq!(status.a2a_peers["remote"].state, "ok");
    assert_eq!(status.a2a_peers["remote"].ports, vec![3001]);
    let persisted: DaemonState = read_json_file(&env.state);
    assert!(persisted.a2a_cache["remote"].error.is_none());

    state.a2a_cache.get_mut("remote").unwrap().next_refresh_unix = 0;
    sync_mesh(&args, &bootstrap, &mut state).unwrap();
    write_daemon_state(&state).unwrap();

    let stale_config: Value = read_json_file(&env.tng_config);
    assert_eq!(
        stale_config["add_ingress"][0]["mapping"]["in"]["port"],
        3001
    );
    let stale_directory: ServiceDirectory = read_json_file(&env.service_directory);
    assert_eq!(stale_directory.services["remote"].ports[0].port, 3001);
    let stale_status = read_status_file(&env);
    assert_eq!(stale_status.a2a_peers["remote"].state, "stale");
    assert!(stale_status.a2a_peers["remote"]
        .error
        .as_deref()
        .unwrap()
        .contains("HTTP status 503"));
    let stale_state: DaemonState = read_json_file(&env.state);
    assert!(stale_state.a2a_cache["remote"]
        .error
        .as_deref()
        .unwrap()
        .contains("HTTP status 503"));
    assert_eq!(stale_state.a2a_cache["remote"].ports[0].local, 3001);
}

#[test]
fn resource_watcher_add_resource_paths_detects_new_dynamic_resource_file() {
    let temp = tempfile::tempdir().unwrap();
    let cdh_root = temp.path().join("cdh");
    fs::create_dir_all(cdh_root.join("default/local-resources")).unwrap();
    fs::create_dir_all(cdh_root.join("default/dynamic-resources")).unwrap();
    let dynamic = Path::new("default/dynamic-resources/runtime.json");
    let mut watcher =
        ResourceWatcher::new(&cdh_root, [Path::new(TEST_BOOTSTRAP_RESOURCE)]).unwrap();

    watcher.add_resource_paths([dynamic]).unwrap();
    assert_eq!(
        watcher.wait(Some(Duration::from_secs(1))).unwrap(),
        ResourceWatcherWait::Event
    );

    write_test_file(&cdh_root.join(dynamic), b"{\"runtime\":true}\n");
    assert_eq!(
        watcher.wait(Some(Duration::from_secs(1))).unwrap(),
        ResourceWatcherWait::Event
    );
}

#[test]
fn write_json_if_changed_skips_identical_rewrite() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.json");
    let compact = r#"{"alpha":1,"nested":[true]}"#;
    fs::write(&path, compact).unwrap();

    let changed = write_json_if_changed(&path, &json!({"alpha": 1, "nested": [true]})).unwrap();

    assert!(!changed);
    assert_eq!(fs::read_to_string(&path).unwrap(), compact);
    assert!(!path.with_extension("confidential-agent.tmp").exists());
}

#[test]
fn a2a_tng_ingress_fetches_agent_card_and_generates_config() {
    let card = test_agent_card("remote-agent", &[3001, 3002]);
    let url = serve_agent_card_once(card);
    let bundle = test_a2a_bundle(None, &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 2);
    assert_eq!(ingress[0]["mapping"]["out"]["host"], "127.0.0.1");
    assert_eq!(ingress[0]["mapping"]["out"]["port"], 3001);
    assert_eq!(ingress[0]["mapping"]["in"]["host"], "127.0.0.1");
    assert_eq!(ingress[0]["mapping"]["in"]["port"], 3001);
    assert_eq!(ingress[1]["mapping"]["out"]["port"], 3002);
    assert_eq!(ingress[0]["rats_tls"]["multiplex"], true);
    assert_eq!(
        ingress[0]["verify"]["attestation_policy"]["path"],
        DEFAULT_POLICY_PATH
    );
    assert!(ingress[0]["verify"].get("policy").is_none());
    assert!(ingress[0]["verify"].get("policy_ids").is_none());

    let rv = &ingress[0]["verify"]["reference_values"][0];
    assert_eq!(rv["type"], "slsa");
    assert!(ingress[0].get("attest").is_none());
    assert_eq!(
        rv["payload"]["content"]["rv_list"][0]["id"],
        "remote-agent-release"
    );
    assert_eq!(
        rv["payload"]["content"]["rv_list"][0]["rv_name"],
        "measurement.uki.SHA-384"
    );

    assert!(directory.contains_key("remote-agent"));
    assert_eq!(directory["remote-agent"].ports.len(), 2);
    assert_eq!(directory["remote-agent"].ports[0].port, 3001);
    assert_eq!(state.a2a_status[&url].state, "ok");
}

#[test]
fn a2a_tng_ingress_prefers_agent_card_sample_reference_values() {
    let mut card = test_agent_card("remote-agent", &[3001]);
    mutate_confidential(&mut card, |confidential| {
        confidential.reference_values = Some(json!({"measurement.uki.SHA-384": ["sample-rv"]}));
    });
    let url = serve_agent_card_once(card);
    let bundle = test_a2a_bundle(None, &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    let rv = &ingress[0]["verify"]["reference_values"][0];
    assert_eq!(rv["type"], "sample");
    assert_eq!(
        rv["payload"]["content"]["measurement.uki.SHA-384"][0],
        "sample-rv"
    );
}

#[test]
fn a2a_tng_ingress_uses_alias_and_allocates_non_conflicting_local_port() {
    let card = test_agent_card("remote-openclaw", &[18789]);
    let url = serve_agent_card_once(card);
    let bundle = test_a2a_bundle(Some("beta"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[18789], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    assert_eq!(ingress[0]["mapping"]["out"]["port"], 18789);
    assert_ne!(ingress[0]["mapping"]["in"]["port"], 18789);
    assert_eq!(
        directory["beta"].ports[0].port,
        ingress[0]["mapping"]["in"]["port"]
    );
    assert_eq!(state.a2a_status["beta"].state, "ok");
}

#[test]
fn a2a_tng_ingress_records_unreachable_peer_without_panicking() {
    let bundle = test_a2a_bundle(
        Some("unreachable"),
        "http://127.0.0.1:1/.well-known/agent-card.json",
        &[],
    );
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert!(directory.is_empty());
    assert_eq!(state.a2a_status["unreachable"].state, "error");
    assert!(state.last_error.is_some());
}

#[test]
fn a2a_tng_ingress_uses_negative_cache_after_initial_fetch_failure() {
    let (url, hits) = serve_agent_card_error_counter();
    let bundle = test_a2a_bundle(Some("unreachable"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let cached = state.a2a_cache.get("unreachable").unwrap();
    assert!(cached.id.is_none());
    assert_eq!(
        cached.next_refresh_unix - cached.fetched_at_unix,
        A2A_FETCH_FAILURE_BACKOFF_SEC
    );

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(state.a2a_status["unreachable"].state, "error");
}

#[test]
fn a2a_tng_ingress_uses_stale_cache_for_transient_fetch_failure() {
    let card = test_agent_card("remote-agent", &[3001]);
    let url = serve_agent_card_once(card);
    let bundle = test_a2a_bundle(Some("remote"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    state.a2a_cache.get_mut("remote").unwrap().next_refresh_unix = 0;

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    assert!(directory.contains_key("remote"));
    assert_eq!(state.a2a_status["remote"].state, "stale");
    assert!(state.a2a_status["remote"]
        .error
        .as_deref()
        .unwrap()
        .contains("agent card transport error"));

    let directory = empty_service_directory();
    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    assert_eq!(state.a2a_status["remote"].state, "stale");
}

#[test]
fn a2a_tng_ingress_uses_stale_cache_for_http_5xx() {
    let valid = test_agent_card("remote-agent", &[3001]);
    let url = serve_agent_card_then_status(valid, 503, "unavailable");
    let bundle = test_a2a_bundle(Some("remote"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    state.a2a_cache.get_mut("remote").unwrap().next_refresh_unix = 0;

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    assert!(directory.contains_key("remote"));
    assert_eq!(state.a2a_status["remote"].state, "stale");
    assert!(state.a2a_status["remote"]
        .error
        .as_deref()
        .unwrap()
        .contains("HTTP status 503"));
}

#[test]
fn a2a_fetch_error_stale_policy_keeps_trust_failures_strict() {
    assert!(a2a_fetch_error_allows_stale(
        &AgentCardFetchError::Transport("connection reset".to_string())
    ));
    assert!(a2a_fetch_error_allows_stale(
        &AgentCardFetchError::HttpStatus {
            status: 503,
            body_preview: "unavailable".to_string(),
        }
    ));
    assert!(!a2a_fetch_error_allows_stale(
        &AgentCardFetchError::HttpStatus {
            status: 499,
            body_preview: "client closed".to_string(),
        }
    ));
    assert!(!a2a_fetch_error_allows_stale(
        &AgentCardFetchError::HostResolution {
            host: "peer.example".to_string(),
            message: "dns failed".to_string(),
        }
    ));
    assert!(!a2a_fetch_error_allows_stale(
        &AgentCardFetchError::PublicIpHostMismatch {
            declared: "203.0.113.10".parse().unwrap(),
            resolved: vec!["203.0.113.11".parse().unwrap()],
        }
    ));
    assert!(!a2a_fetch_error_allows_stale(
        &AgentCardFetchError::RekorUrlNotTrusted {
            url: "https://rekor.example".to_string(),
            allowed: vec!["https://rekor.sigstore.dev".to_string()],
        }
    ));
}

#[test]
fn a2a_tng_ingress_rejects_agent_card_public_ip_mismatch() {
    let mut card = test_agent_card("remote-agent", &[3001]);
    mutate_confidential(&mut card, |confidential| {
        confidential.public_ip = "198.51.100.10".to_string();
    });
    let url = serve_agent_card_once(card);
    let bundle = test_a2a_bundle(Some("remote"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert!(directory.is_empty());
    assert_eq!(state.a2a_status["remote"].state, "error");
    assert!(state.a2a_status["remote"]
        .error
        .as_deref()
        .unwrap()
        .contains("publicIp"));
}

#[test]
fn a2a_tng_ingress_rejects_public_ip_update_without_stale_fallback() {
    let valid = test_agent_card("remote-agent", &[3001]);
    let mut mismatch = test_agent_card("remote-agent", &[3001]);
    mutate_confidential(&mut mismatch, |confidential| {
        confidential.public_ip = "198.51.100.10".to_string();
    });
    let url = serve_agent_cards(vec![valid, mismatch]);
    let bundle = test_a2a_bundle(Some("remote"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    state.a2a_cache.get_mut("remote").unwrap().next_refresh_unix = 0;

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert!(directory.is_empty());
    assert_eq!(state.a2a_status["remote"].state, "error");
    assert!(state.a2a_status["remote"].last_success_unix.is_some());
    assert!(state.a2a_status["remote"]
        .error
        .as_deref()
        .unwrap()
        .contains("publicIp"));
    assert!(!cached_peer_is_resolvable(
        state.a2a_cache.get("remote").unwrap()
    ));
}

#[test]
fn a2a_tng_ingress_rejects_untrusted_rekor_update_without_stale_fallback() {
    let valid = test_agent_card("remote-agent", &[3001]);
    let mut untrusted = test_agent_card("remote-agent", &[3001]);
    mutate_confidential(&mut untrusted, |confidential| {
        confidential.rekor.rekor_url = "https://attacker.example/rekor".to_string();
    });
    let url = serve_agent_cards(vec![valid, untrusted]);
    let bundle = test_a2a_bundle(Some("remote"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    state.a2a_cache.get_mut("remote").unwrap().next_refresh_unix = 0;

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert!(directory.is_empty());
    assert_eq!(state.a2a_status["remote"].state, "error");
    assert!(state.a2a_status["remote"].last_success_unix.is_some());
    assert!(state.a2a_status["remote"]
        .error
        .as_deref()
        .unwrap()
        .contains("rekorUrl"));
    assert!(!cached_peer_is_resolvable(
        state.a2a_cache.get("remote").unwrap()
    ));
}

#[test]
fn a2a_tng_ingress_rejects_http_404_without_stale_fallback() {
    let valid = test_agent_card("remote-agent", &[3001]);
    let url = serve_agent_card_then_status(valid, 404, "removed");
    let bundle = test_a2a_bundle(Some("remote"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    state.a2a_cache.get_mut("remote").unwrap().next_refresh_unix = 0;

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert!(directory.is_empty());
    assert_eq!(state.a2a_status["remote"].state, "error");
    assert!(state.a2a_status["remote"].last_success_unix.is_some());
    assert!(state.a2a_status["remote"]
        .error
        .as_deref()
        .unwrap()
        .contains("HTTP status 404"));
    assert!(!cached_peer_is_resolvable(
        state.a2a_cache.get("remote").unwrap()
    ));
}

#[test]
fn a2a_tng_ingress_rejects_untrusted_rekor_url() {
    let mut card = test_agent_card("remote-agent", &[3001]);
    mutate_confidential(&mut card, |confidential| {
        confidential.rekor.rekor_url = "https://attacker.example/rekor".to_string();
    });
    let url = serve_agent_card_once(card);
    let bundle = test_a2a_bundle(Some("remote"), &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert!(directory.is_empty());
    assert_eq!(state.a2a_status["remote"].state, "error");
    assert!(state.a2a_status["remote"]
        .error
        .as_deref()
        .unwrap()
        .contains("rekorUrl"));
}

#[test]
fn a2a_tng_ingress_rejects_peer_id_collision_with_service_directory() {
    let card = test_agent_card("remote-agent", &[3001]);
    let url = serve_agent_card_once(card);
    let bundle = test_a2a_bundle(None, &url, &[]);
    let mut directory = empty_service_directory();
    directory.services.insert(
        "remote-agent".to_string(),
        ServiceDirectoryService { ports: Vec::new() },
    );
    let mut state = DaemonState::default();

    let (ingress, peer_directory) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert!(ingress.is_empty());
    assert!(peer_directory.is_empty());
    assert_eq!(state.a2a_status[&url].state, "error");
    assert!(state.a2a_status[&url]
        .error
        .as_deref()
        .unwrap()
        .contains("conflicts with an existing service"));
}

#[test]
fn a2a_tng_ingress_clamps_agent_card_cache_ttl() {
    let mut card = test_agent_card("remote-agent", &[3001]);
    mutate_confidential(&mut card, |confidential| {
        confidential.cache_ttl_sec = 1;
    });
    let url = serve_agent_card_once(card);
    let bundle = test_a2a_bundle(None, &url, &[]);
    let directory = empty_service_directory();
    let mut state = DaemonState::default();

    let (ingress, _) = a2a_tng_ingress(&bundle, "self", &[], &directory, &mut state);

    assert_eq!(ingress.len(), 1);
    let cached = state.a2a_cache.get(&url).unwrap();
    assert_eq!(
        cached.next_refresh_unix - cached.fetched_at_unix,
        A2A_CACHE_TTL_MIN_SEC
    );
}

#[test]
fn initrd_fetch_stages_disk_key_with_private_mode() {
    let temp = tempfile::tempdir().unwrap();
    let cdh_root = temp.path().join("cdh");
    let bootstrap_path = cdh_root.join(TEST_BOOTSTRAP_RESOURCE);
    let disk_key_resource = "default/local-resources/disk_passphrase";
    let disk_key_path = cdh_root.join(disk_key_resource);
    write_json_fixture(&bootstrap_path, &test_bootstrap_config("openclaw"));
    write_test_file(&disk_key_path, b"secret-passphrase\n");
    let stage_dir = temp.path().join("stage");

    initrd_fetch(InitrdFetchArgs {
        cdh_root,
        bootstrap_resource: TEST_BOOTSTRAP_RESOURCE.to_string(),
        disk_key_resource: disk_key_resource.to_string(),
        stage_dir: stage_dir.clone(),
        wait_timeout_sec: 1,
        retry_interval_sec: 1,
    })
    .unwrap();

    let staged_key = stage_dir.join("disk_key");
    assert_eq!(fs::read(&staged_key).unwrap(), b"secret-passphrase\n");
    assert_eq!(
        fs::metadata(staged_key).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn initrd_retry_recovers_from_transient_trustee_failure() {
    let mut attempts = 0u64;
    let deadline = InitrdDeadline::new(1).unwrap();

    let value = retry_initrd_operation("test Trustee resource", &deadline, 0, |_| {
        attempts += 1;
        if attempts < 3 {
            bail!("transient KBS failure {attempts}");
        }
        Ok("resource")
    })
    .unwrap();

    assert_eq!(value, "resource");
    assert_eq!(attempts, 3);
}

#[test]
fn initrd_retry_uses_one_global_deadline_across_operations() {
    let started = Instant::now();
    let deadline = InitrdDeadline {
        started,
        expires_at: Some(started + Duration::from_millis(300)),
    };
    let first_remaining = deadline.remaining("first resource").unwrap().unwrap();
    thread::sleep(Duration::from_millis(120));
    let mut second_remaining = None;

    let error =
        retry_initrd_operation("second resource", &deadline, 1, |remaining| -> Result<()> {
            second_remaining = remaining;
            bail!("resource is still unavailable")
        })
        .unwrap_err();

    assert!(second_remaining.unwrap() < first_remaining);
    assert!(format!("{error:#}").contains("timed out waiting for second resource"));
    assert!(started.elapsed() < Duration::from_millis(700));
}

#[test]
fn initrd_fail_closed_honors_poweroff_skip() {
    let env = EnvGuard::new(&["CA_SKIP_INITRD_POWEROFF"]);
    env.set("CA_SKIP_INITRD_POWEROFF", "1");

    let err = initrd_fail_closed("missing initrd secret".to_string()).unwrap_err();

    assert!(err.to_string().contains("missing initrd secret"));
}

#[test]
fn initrd_fail_closed_uses_injected_systemctl_only_when_forced() {
    let env = EnvGuard::new(&[
        "CA_SKIP_INITRD_POWEROFF",
        "CA_FORCE_INITRD_POWEROFF",
        "CA_SYSTEMCTL_BIN",
    ]);
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("systemctl.args");
    let fake_systemctl = temp.path().join("systemctl-test-double");
    fs::write(
        &fake_systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o700)).unwrap();
    env.remove("CA_SKIP_INITRD_POWEROFF");
    env.set("CA_FORCE_INITRD_POWEROFF", "1");
    env.set_path("CA_SYSTEMCTL_BIN", &fake_systemctl);

    let err = initrd_fail_closed("forced test failure".to_string()).unwrap_err();

    assert!(err.to_string().contains("forced test failure"));
    assert_eq!(
        fs::read_to_string(marker).unwrap().trim(),
        "--no-block poweroff"
    );
}

#[test]
fn read_bootstrap_handles_absent_empty_and_invalid_resources() {
    let temp = tempfile::tempdir().unwrap();
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());

    assert!(read_bootstrap(&args, RuntimeMode::Challenge)
        .unwrap()
        .is_none());
    write_test_file(&cdh_root.join(TEST_BOOTSTRAP_RESOURCE), b"");
    assert!(read_bootstrap(&args, RuntimeMode::Challenge)
        .unwrap()
        .is_none());

    let mut unsupported = test_bootstrap_config("openclaw");
    unsupported.mode = "trustee".to_string();
    write_json_fixture(&cdh_root.join(TEST_BOOTSTRAP_RESOURCE), &unsupported);

    let err = read_bootstrap(&args, RuntimeMode::Challenge).unwrap_err();
    assert!(err.to_string().contains("does not match runtime mode"));
    assert!(read_bootstrap(&args, RuntimeMode::Trustee)
        .unwrap()
        .is_some());
}

#[test]
fn bootstrap_apply_reports_waiting_for_missing_required_resource() {
    let temp = tempfile::tempdir().unwrap();
    let env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root);
    let target = temp.path().join("managed/config.json");
    let mut bootstrap = test_bootstrap_config("openclaw");
    bootstrap.resources.push(GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config.json".to_string(),
        target,
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: None,
    });
    let mut state = DaemonState::default();

    let ready = apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap();

    assert!(!ready);
    assert_eq!(state.bootstrap_generation, 0);
    let status = read_status_file(&env);
    assert_eq!(status.bootstrap_generation, 1);
    assert_eq!(status.phase, "waiting-resources");
    assert!(status.applied_resources.is_empty());
}

#[test]
fn bootstrap_apply_rejects_digest_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let source = cdh_root.join("default/local-resources/config.json");
    write_test_file(&source, b"{\"version\":1}\n");
    let mut bootstrap = test_bootstrap_config("openclaw");
    bootstrap.resources.push(GuestResource {
        id: "config".to_string(),
        resource_path: "default/local-resources/config.json".to_string(),
        target: temp.path().join("managed/config.json"),
        owner: None,
        group: None,
        mode: "0600".to_string(),
        required: true,
        mutable: false,
        sha256: Some("0".repeat(64)),
    });
    let mut state = DaemonState::default();

    let err = apply_bootstrap(&args, &bootstrap, &mut state, false).unwrap_err();

    assert!(err.to_string().contains("digest mismatch"));
}

#[test]
fn read_a2a_bundle_none_empty_then_caches_valid_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let env = no_tee_test_env(&temp);
    let cdh_root = temp.path().join("cdh");
    let args = test_run_args(cdh_root.clone());
    let bundle_path = cdh_root.join(TEST_A2A_BUNDLE_RESOURCE);

    assert!(read_a2a_bundle(&args).unwrap().is_none());
    write_test_file(&bundle_path, b"");
    assert!(read_a2a_bundle(&args).unwrap().is_none());

    let bundle = test_a2a_bundle(
        Some("remote"),
        "http://127.0.0.1:65535/.well-known/agent-card.json",
        &["openclaw"],
    );
    write_json_fixture(&bundle_path, &bundle);

    let read = read_a2a_bundle(&args).unwrap().unwrap();

    assert_eq!(read.peers[0].alias.as_deref(), Some("remote"));
    assert!(env.cache_dir.join("a2a-bundle.json").exists());
}

#[test]
fn runtime_port_plan_rejects_invalid_mesh_shapes() {
    let missing_self: MeshBundle = serde_json::from_value(json!({
        "schema": MESH_SCHEMA_VERSION,
        "generation": 1,
        "updated_at": 0,
        "services": {},
        "reference_values": {},
        "rekor_reference_values": {}
    }))
    .unwrap();
    assert!(runtime_port_plan(&missing_self, "self")
        .unwrap_err()
        .to_string()
        .contains("not present"));

    let inactive_self: MeshBundle = serde_json::from_value(json!({
        "schema": MESH_SCHEMA_VERSION,
        "generation": 1,
        "updated_at": 0,
        "services": {
            "self": {"phase": "deleted", "ports": [18789], "connect": []}
        },
        "reference_values": {},
        "rekor_reference_values": {}
    }))
    .unwrap();
    assert!(runtime_port_plan(&inactive_self, "self")
        .unwrap_err()
        .to_string()
        .contains("not active"));

    let peer_without_ip: MeshBundle = serde_json::from_value(json!({
        "schema": MESH_SCHEMA_VERSION,
        "generation": 1,
        "updated_at": 0,
        "services": {
            "self": {"phase": "active", "ports": [18789], "connect": []},
            "peer": {"phase": "active", "ports": [3001], "connect": []}
        },
        "reference_values": {"peer": {"measurement.uki.SHA-384": ["rv"]}},
        "rekor_reference_values": {}
    }))
    .unwrap();
    assert!(runtime_port_plan(&peer_without_ip, "self")
        .unwrap_err()
        .to_string()
        .contains("no reachable IP"));
}

#[test]
fn allocate_internal_port_reports_exhaustion() {
    let mut used = (TNG_INGRESS_PORT_BASE..=60999).collect::<BTreeSet<_>>();

    let err = allocate_internal_port(TNG_INGRESS_PORT_BASE, &mut used).unwrap_err();

    assert!(err.to_string().contains("no available internal"));
}

#[test]
fn tng_reference_values_accepts_rekor_and_rejects_missing_peer_values() {
    let bundle: MeshBundle = serde_json::from_value(json!({
        "schema": MESH_SCHEMA_VERSION,
        "generation": 1,
        "updated_at": 0,
        "services": {
            "peer": {
                "phase": "active",
                "public_ip": "127.0.0.1",
                "ports": [3001],
                "connect": []
            }
        },
        "reference_values": {},
        "rekor_reference_values": {
            "peer": {
                "artifact_id": "peer-disk",
                "artifact_version": "20260601000000",
                "artifact_type": "uki",
                "rekor_url": "https://rekor.sigstore.dev",
                "rv_name": "measurement.uki.SHA-384"
            }
        }
    }))
    .unwrap();

    let values = tng_reference_values(&bundle, "peer").unwrap();
    assert_eq!(values[0]["type"], "slsa");
    assert_eq!(
        values[0]["payload"]["content"]["rv_list"][0]["id"],
        "peer-disk"
    );

    let err = tng_reference_values(&bundle, "missing").unwrap_err();
    assert!(err.to_string().contains("missing reference values"));
}

#[test]
fn gateway_config_rejects_missing_identity_and_peer_gateway_key() {
    let bundle: MeshBundle = serde_json::from_value(json!({
        "schema": MESH_SCHEMA_VERSION,
        "generation": 1,
        "updated_at": 0,
        "services": {
            "self": {
                "phase": "active",
                "public_ip": "127.0.0.1",
                "ports": [18789, 18800],
                "connect": [18789],
                "gateway_public_key": "self-pub"
            },
            "peer": {
                "phase": "active",
                "public_ip": "127.0.0.1",
                "ports": [3001],
                "connect": []
            }
        },
        "reference_values": {"peer": {"measurement.uki.SHA-384": ["rv"]}},
        "rekor_reference_values": {}
    }))
    .unwrap();
    let plan = runtime_port_plan(&bundle, "self").unwrap();
    let mut bootstrap = test_bootstrap_config("self");
    bootstrap.gateway_identity = None;

    let err = gateway_config(&bundle, "self", &bootstrap, &plan).unwrap_err();
    assert!(err.to_string().contains("missing gateway_identity"));

    bootstrap.gateway_identity = Some(GatewayIdentity {
        public_key: "pub".to_string(),
        private_key: "priv".to_string(),
    });
    let err = gateway_config(&bundle, "self", &bootstrap, &plan).unwrap_err();
    assert!(err.to_string().contains("missing gateway_public_key"));
}

#[test]
fn gateway_config_listen_ports_deduplicates_and_ignores_invalid_values() {
    let config = json!({
        "client_routes": [
            {"listen_port": 3001},
            {"listen_port": 3001},
            {"listen_port": 70000},
            {"listen_port": "3002"}
        ],
        "server_routes": [
            {"listen_port": 39200},
            {"missing": true}
        ]
    });

    assert_eq!(gateway_config_listen_ports(&config), vec![3001, 39200]);
}

#[test]
fn daemon_state_round_trips_through_configured_path() {
    let temp = tempfile::tempdir().unwrap();
    let env = no_tee_test_env(&temp);
    let mut state = read_daemon_state().unwrap();
    assert_eq!(state.bootstrap_generation, 0);
    state.bootstrap_generation = 9;
    state.mesh_generation = 42;
    state.last_error = Some("last failure".to_string());

    write_daemon_state(&state).unwrap();
    let read = read_daemon_state().unwrap();

    assert_eq!(read.bootstrap_generation, 9);
    assert_eq!(read.mesh_generation, 42);
    assert_eq!(read.last_error.as_deref(), Some("last failure"));
    assert!(env.state.exists());
}

#[test]
fn a2a_cache_helpers_clamp_reassign_and_validate_cached_peers() {
    assert_eq!(a2a_cache_ttl_sec(1), A2A_CACHE_TTL_MIN_SEC);
    assert_eq!(a2a_cache_ttl_sec(u64::MAX), A2A_CACHE_TTL_MAX_SEC);

    let mut cached = A2aCachedPeer {
        url: "http://127.0.0.1/card.json".to_string(),
        alias: Some("alias".to_string()),
        id: Some("remote".to_string()),
        ports: vec![A2aCachedPort {
            remote: 3001,
            local: 3001,
        }],
        public_ip: Some("127.0.0.1".to_string()),
        reference_values: json!([{"type": "sample"}]),
        fetched_at_unix: 10,
        next_refresh_unix: 20,
        fingerprint: "fingerprint".to_string(),
        error: None,
    };
    let mut used = BTreeSet::from([3001]);
    ensure_cached_ports_available(&mut cached, &mut used).unwrap();

    assert_ne!(cached.ports[0].local, 3001);
    assert!(used.contains(&cached.ports[0].local));
    let resolved = resolved_from_cache(&cached).unwrap();
    assert_eq!(resolved.id, "alias");

    cached.id = None;
    cached.alias = None;
    assert!(resolved_from_cache(&cached)
        .unwrap_err()
        .to_string()
        .contains("no id"));
}

#[test]
fn a2a_peer_scope_key_and_negative_cache_are_stable() {
    let peer = A2aBundlePeer {
        alias: Some("beta".to_string()),
        url: "http://127.0.0.1/card.json".to_string(),
        scoped_services: vec!["openclaw".to_string()],
        signer: None,
        fingerprint: "fingerprint".to_string(),
    };

    assert!(a2a_peer_scoped_to_service(&peer, "openclaw"));
    assert!(!a2a_peer_scoped_to_service(&peer, "other"));
    assert_eq!(a2a_peer_key(&peer), "beta");

    let cached = negative_a2a_cache(&peer, 100, "fetch failed".to_string());
    assert_eq!(
        cached.next_refresh_unix,
        100 + A2A_FETCH_FAILURE_BACKOFF_SEC
    );
    assert!(!cached_peer_is_resolvable(&cached));
    assert_eq!(cached.error.as_deref(), Some("fetch failed"));
}

#[test]
fn agent_card_http_returns_404_when_card_file_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let _env = no_tee_test_env(&temp);

    let response = http_roundtrip(
        HttpServerKind::AgentCard,
        b"GET /.well-known/agent-card.json HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );

    assert!(response.starts_with("HTTP/1.1 404 Not Found"));
}

fn no_tee_test_env(temp: &tempfile::TempDir) -> NoTeeTestEnv {
    let guard = EnvGuard::new(DAEMON_ENV_VARS);
    let root = temp.path().join("daemon-env");
    let tng_config = root.join("etc/tng/config.json");
    let gateway_config = root.join("etc/cai/gateway.json");
    let service_directory = root.join("etc/cai/service-directory.json");
    let status = root.join("run/confidential-agent/status.json");
    let agent_card = root.join("opt/confidential-agent/agent-card.json");
    let cache_dir = root.join("var/cache/confidential-agent");
    let state = root.join("var/lib/confidential-agent/state.json");
    guard.set_path("CA_TNG_CONFIG_PATH", &tng_config);
    guard.set_path("CA_GATEWAY_CONFIG_PATH", &gateway_config);
    guard.set_path("CA_SERVICE_DIRECTORY_PATH", &service_directory);
    guard.set_path("CA_DAEMON_STATUS_PATH", &status);
    guard.set_path("CA_AGENT_CARD_PATH", &agent_card);
    guard.set_path("CA_DAEMON_CACHE_DIR", &cache_dir);
    guard.set_path("CA_DAEMON_STATE_PATH", &state);
    guard.set("CA_SKIP_SYSTEMCTL", "1");
    NoTeeTestEnv {
        _guard: guard,
        tng_config,
        gateway_config,
        service_directory,
        status,
        agent_card,
        cache_dir,
        state,
    }
}

fn test_run_args(cdh_root: PathBuf) -> RunArgs {
    RunArgs {
        cdh_root,
        bootstrap_resource: TEST_BOOTSTRAP_RESOURCE.to_string(),
        mesh_resource: TEST_MESH_RESOURCE.to_string(),
        a2a_bundle_resource: TEST_A2A_BUNDLE_RESOURCE.to_string(),
        poll_interval_sec: 5,
        status_listen: "127.0.0.1:0".to_string(),
        agent_card_listen: "127.0.0.1:0".to_string(),
    }
}

fn test_bootstrap_config(service_id: &str) -> BootstrapConfig {
    BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
        generation: 1,
        service_id: service_id.to_string(),
        mode: "challenge".to_string(),
        ports: Vec::new(),
        connect: Vec::new(),
        mcp_ports: Vec::new(),
        gateway_identity: Some(GatewayIdentity {
            public_key: "pub".to_string(),
            private_key: "priv".to_string(),
        }),
        resources: Vec::new(),
        app_service: None,
        peers: Vec::new(),
        agent_card: None,
    }
}

fn write_test_file(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn install_fake_systemctl(env: &EnvGuard, temp: &tempfile::TempDir) -> PathBuf {
    let fake_systemctl = temp.path().join("systemctl");
    let log_path = temp.path().join("systemctl.log");
    fs::write(
        &fake_systemctl,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 0\n",
            log_path.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    env.set(
        "PATH",
        format!("{}:{}", temp.path().display(), old_path.to_string_lossy()),
    );
    env.remove("CA_SKIP_SYSTEMCTL");
    env.set_path("CA_DAEMON_STATUS_PATH", &temp.path().join("status.json"));
    log_path
}

fn write_json_fixture(path: &Path, value: &impl serde::Serialize) {
    let content = serde_json::to_vec_pretty(value).unwrap();
    write_test_file(path, &content);
}

fn read_json_file<T: serde::de::DeserializeOwned>(path: &Path) -> T {
    let content = fs::read_to_string(path).unwrap();
    serde_json::from_str(&content).unwrap()
}

fn read_status_file(env: &NoTeeTestEnv) -> DaemonStatus {
    read_json_file(&env.status)
}

fn http_response_body(response: &str) -> &str {
    response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap()
}

fn test_agent_card(id: &str, ports: &[u16]) -> confidential_agent_core::schema::AgentCard {
    use confidential_agent_core::agent_card::CONFIDENTIAL_AGENT_EXTENSION;
    use confidential_agent_core::schema::{
        AgentCard, AgentCardCapabilities, AgentCardConfidential, AgentCardPort, AgentCardRekor,
        AgentExtension, AgentInterface,
    };

    AgentCard {
        protocol_version: "1.0".to_string(),
        name: id.to_string(),
        description: format!("{id} test card"),
        version: Some("1.0.0".to_string()),
        supported_interfaces: ports
            .iter()
            .map(|port| AgentInterface {
                url: format!("http://127.0.0.1:{port}/a2a"),
                protocol_binding: "JSONRPC".to_string(),
                protocol_version: "1.0".to_string(),
                tenant: None,
            })
            .collect(),
        preferred_transport: Some("JSONRPC".to_string()),
        skills: Vec::new(),
        default_input_modes: vec!["text".to_string()],
        default_output_modes: vec!["text".to_string()],
        capabilities: AgentCardCapabilities {
            extensions: vec![AgentExtension {
                uri: CONFIDENTIAL_AGENT_EXTENSION.to_string(),
                description: None,
                required: true,
                params: serde_json::to_value(AgentCardConfidential {
                    id: id.to_string(),
                    cache_ttl_sec: 300,
                    public_ip: "127.0.0.1".to_string(),
                    ports: ports
                        .iter()
                        .map(|port| AgentCardPort {
                            name: format!("port-{port}"),
                            port: *port,
                        })
                        .collect(),
                    reference_values: None,
                    rekor: AgentCardRekor {
                        rekor_url: "https://rekor.sigstore.dev".to_string(),
                        artifact_id: format!("{id}-release"),
                        artifact_type: "uki".to_string(),
                        artifact_version: "20260512".to_string(),
                        rv_name: "measurement.uki.SHA-384".to_string(),
                    },
                    tee: "tdx".to_string(),
                })
                .unwrap(),
            }],
            ..Default::default()
        },
        provider: None,
        security_schemes: None,
        security: Vec::new(),
        supports_authenticated_extended_card: Some(false),
        signatures: Vec::new(),
    }
}

fn mutate_confidential(
    card: &mut confidential_agent_core::schema::AgentCard,
    f: impl FnOnce(&mut confidential_agent_core::schema::AgentCardConfidential),
) {
    let extension = card.capabilities.extensions.first_mut().unwrap();
    let mut confidential: confidential_agent_core::schema::AgentCardConfidential =
        serde_json::from_value(extension.params.clone()).unwrap();
    f(&mut confidential);
    extension.params = serde_json::to_value(confidential).unwrap();
}

fn serve_agent_card_once(card: confidential_agent_core::schema::AgentCard) -> String {
    serve_agent_cards(vec![card])
}

fn serve_agent_cards(cards: Vec<confidential_agent_core::schema::AgentCard>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for card in cards {
            let card_json = serde_json::to_string(&card).unwrap();
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                card_json.len(),
                card_json
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}/.well-known/agent-card.json")
}

fn serve_agent_card_then_status(
    card: confidential_agent_core::schema::AgentCard,
    status: u16,
    body: &str,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let card_json = serde_json::to_string(&card).unwrap();
    let body = body.to_string();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                card_json.len(),
                card_json
            );
            let _ = stream.write_all(response.as_bytes());
        }
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 {status} Error\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}/.well-known/agent-card.json")
}

fn serve_agent_card_error_counter() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let thread_hits = hits.clone();
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    thread_hits.fetch_add(1, Ordering::SeqCst);
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf);
                    let body = "temporary failure";
                    let response = format!(
                        "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes());
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });
    (
        format!("http://127.0.0.1:{port}/.well-known/agent-card.json"),
        hits,
    )
}

fn http_roundtrip(kind: HttpServerKind, request: &[u8]) -> String {
    http_roundtrip_with_limits(kind, Arc::new(HttpServerLimits::default()), request)
}

fn http_roundtrip_with_limits(
    kind: HttpServerKind,
    limits: Arc<HttpServerLimits>,
    request: &[u8],
) -> String {
    http_roundtrip_with_accept_outcome(kind, limits, request).0
}

fn http_roundtrip_with_accept_outcome(
    kind: HttpServerKind,
    limits: Arc<HttpServerLimits>,
    request: &[u8],
) -> (String, bool) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let request = request.to_vec();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        match handle_accepted_http_stream(stream, kind, limits).unwrap() {
            HttpAcceptOutcome::Spawned(handle) => {
                handle.join().unwrap();
                true
            }
            HttpAcceptOutcome::Rejected => false,
        }
    });

    let mut client = TcpStream::connect(addr).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    match client.write_all(&request) {
        Ok(()) => {}
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
            ) => {}
        Err(err) => panic!("failed to write HTTP request: {err}"),
    }
    let _ = client.shutdown(std::net::Shutdown::Write);

    let mut response = String::new();
    let mut buffer = [0u8; 1024];
    loop {
        match client.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => response.push_str(&String::from_utf8_lossy(&buffer[..read])),
            Err(err) if err.kind() == std::io::ErrorKind::ConnectionReset => break,
            Err(err) => panic!("failed to read HTTP response: {err}"),
        }
    }
    let spawned_handler = server.join().unwrap();
    (response, spawned_handler)
}

fn test_a2a_bundle(alias: Option<&str>, url: &str, scoped_services: &[&str]) -> A2aBundle {
    A2aBundle {
        version: 2,
        peers: vec![A2aBundlePeer {
            alias: alias.map(str::to_string),
            url: url.to_string(),
            scoped_services: scoped_services
                .iter()
                .map(|value| value.to_string())
                .collect(),
            signer: None,
            fingerprint: sha256_bytes(url.as_bytes()),
        }],
    }
}

fn empty_service_directory() -> ServiceDirectory {
    ServiceDirectory {
        schema: SERVICE_DIRECTORY_SCHEMA_VERSION.to_string(),
        services: BTreeMap::new(),
    }
}
