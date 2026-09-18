use super::commands::{
    a2a_cli_preview_error_kind, build_debug_ssh_command, cmd_a2a, cmd_build, cmd_deploy,
    cmd_destroy, cmd_image, cmd_inject, collect_image_entries, collect_live_status, debug_ssh_hint,
    deploy_shelter_args, fetch_daemon_status_from, live_status_table_columns,
    resolve_debug_ssh_command, status_table_columns, status_views, validate_build_start,
    validate_deploy_start, wait_for_daemon_status_from,
};
use super::*;
use crate::cli::{ConnectCommands, ImageArgs, ImageCommands, InitTarget, StatusArgs};
use clap::Parser;
use confidential_agent_core::agent_card_fetch::AgentCardFetchError;
use confidential_agent_core::schema::DAEMON_STATUS_SCHEMA_VERSION;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn test_cli() -> Cli {
    Cli {
        command: Commands::Status(StatusArgs {
            service: None,
            json: false,
            live: false,
        }),
        shelter_bin: PathBuf::from("shelter"),
        state_dir: PathBuf::from("/work/.confidential-agent"),
        tools_image: "confidential-agent-tools:test".to_string(),
    }
}

#[test]
fn common_commands_default_to_confidential_agent_yaml() {
    let build = Cli::parse_from(["confidential-agent", "build"]);
    match build.command {
        Commands::Build(args) => assert_eq!(args.spec, PathBuf::from("confidential-agent.yaml")),
        other => panic!("expected build command, got {other:?}"),
    }

    let deploy = Cli::parse_from(["confidential-agent", "deploy"]);
    match deploy.command {
        Commands::Deploy(args) => assert_eq!(args.spec, PathBuf::from("confidential-agent.yaml")),
        other => panic!("expected deploy command, got {other:?}"),
    }

    let validate = Cli::parse_from(["confidential-agent", "spec", "validate"]);
    match validate.command {
        Commands::Spec(SpecArgs {
            command: SpecCommands::Validate { spec, .. },
        }) => assert_eq!(spec, PathBuf::from("confidential-agent.yaml")),
        other => panic!("expected spec validate command, got {other:?}"),
    }

    let key = Cli::parse_from([
        "confidential-agent",
        "key",
        "generate-cosign",
        "--output-key-prefix",
        "./secrets/cosign",
    ]);
    match key.command {
        Commands::Key(KeyArgs {
            command:
                KeyCommands::GenerateCosign {
                    output_key_prefix,
                    force,
                },
        }) => {
            assert_eq!(output_key_prefix, PathBuf::from("./secrets/cosign"));
            assert!(!force);
        }
        other => panic!("expected key generate-cosign command, got {other:?}"),
    }
}

#[test]
fn init_cli_accepts_targets_and_aliases() {
    let openclaw = Cli::parse_from([
        "confidential-agent",
        "init",
        "openclaw",
        "--output-dir",
        "/tmp/init",
        "--force",
        "--non-interactive",
    ]);
    match openclaw.command {
        Commands::Init(args) => {
            assert_eq!(args.target, Some(InitTarget::Openclaw));
            assert_eq!(args.output_dir, PathBuf::from("/tmp/init"));
            assert!(args.force);
            assert!(args.non_interactive);
        }
        other => panic!("expected init command, got {other:?}"),
    }

    let claude = Cli::parse_from(["confidential-agent", "init", "claude-code"]);
    match claude.command {
        Commands::Init(args) => assert_eq!(args.target, Some(InitTarget::Claudecode)),
        other => panic!("expected init command, got {other:?}"),
    }
}

#[test]
fn connect_cli_keeps_bare_mode_and_adds_start_stop() {
    let bare = Cli::parse_from(["confidential-agent", "connect", "--service", "openclaw"]);
    match bare.command {
        Commands::Connect(args) => {
            assert_eq!(args.service.as_deref(), Some("openclaw"));
            assert!(args.command.is_none());
        }
        other => panic!("expected connect command, got {other:?}"),
    }

    let start = Cli::parse_from([
        "confidential-agent",
        "connect",
        "start",
        "--service",
        "openclaw",
        "--ready-json",
        "ready.json",
    ]);
    match start.command {
        Commands::Connect(args) => match args.command {
            Some(ConnectCommands::Start(start)) => {
                assert_eq!(start.service.as_deref(), Some("openclaw"));
                assert_eq!(start.ready_json, PathBuf::from("ready.json"));
            }
            other => panic!("expected connect start, got {other:?}"),
        },
        other => panic!("expected connect command, got {other:?}"),
    }

    let stop = Cli::parse_from([
        "confidential-agent",
        "connect",
        "stop",
        "--ready-json",
        "ready.json",
    ]);
    match stop.command {
        Commands::Connect(args) => match args.command {
            Some(ConnectCommands::Stop(stop)) => {
                assert_eq!(stop.ready_json, PathBuf::from("ready.json"))
            }
            other => panic!("expected connect stop, got {other:?}"),
        },
        other => panic!("expected connect command, got {other:?}"),
    }
}

#[test]
fn ssh_cli_accepts_service_and_trailing_args() {
    let cli = Cli::parse_from([
        "confidential-agent",
        "ssh",
        "openclaw",
        "--",
        "-vv",
        "-L",
        "127.0.0.1:8080:127.0.0.1:8080",
    ]);

    match cli.command {
        Commands::Ssh(args) => {
            assert_eq!(args.service, "openclaw");
            assert_eq!(
                args.ssh_args,
                vec![
                    OsString::from("-vv"),
                    OsString::from("-L"),
                    OsString::from("127.0.0.1:8080:127.0.0.1:8080"),
                ]
            );
        }
        other => panic!("expected ssh command, got {other:?}"),
    }
}

#[test]
fn human_status_tables_hide_internal_generations() {
    let status_columns = status_table_columns();
    assert!(!status_columns.contains(&"MESH"));
    assert!(!status_columns.contains(&"MESH_GEN"));

    let live_columns = live_status_table_columns();
    assert!(!live_columns.contains(&"BOOTSTRAP"));
    assert!(!live_columns.contains(&"MESH_ID"));
}

#[test]
fn timestamped_build_id_adds_run_id_without_changing_service_identity() {
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  image_name: openclaw-agent
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
"#,
        Path::new("/project"),
    )
    .unwrap();

    assert_eq!(
        timestamped_shelter_build_id(&spec, "20260508123045123"),
        "openclaw-agent-release-20260508123045123"
    );
    assert_eq!(shelter_build_id(&spec), "openclaw-agent-release");
}

#[test]
fn build_start_rejects_active_and_deployed_services() {
    let active = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    let mut deployed = active.clone();
    deployed.phase = "deployed".to_string();
    let mut deleted = active.clone();
    deleted.phase = "deleted".to_string();

    assert!(validate_build_start(Some(&active)).is_err());
    assert!(validate_build_start(Some(&deployed)).is_err());
    validate_build_start(Some(&deleted)).unwrap();
    validate_build_start(None).unwrap();
}

#[test]
fn deploy_start_only_accepts_built_or_deleted_current_builds() {
    let mut built = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    built.phase = "built".to_string();
    let mut deleted = built.clone();
    deleted.phase = "deleted".to_string();
    let mut active = built.clone();
    active.phase = "active".to_string();

    validate_deploy_start(Some(&built)).unwrap();
    validate_deploy_start(Some(&deleted)).unwrap();
    assert!(validate_deploy_start(Some(&active)).is_err());
    assert!(validate_deploy_start(None).is_err());
}

#[test]
fn image_rm_removes_local_service_state_for_deleted_service() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    service.phase = "deleted".to_string();
    write_state(temp.path(), &service);
    let service_dir = temp.path().join("services/openclaw");
    fs::write(
        service_dir.join("shelter.yaml"),
        "deploy:\n  name: openclaw\n",
    )
    .unwrap();
    let mut cli = test_cli();
    cli.state_dir = temp.path().to_path_buf();

    cmd_image(
        &cli,
        &ImageArgs {
            command: ImageCommands::Rm {
                service: "openclaw".to_string(),
                force: false,
            },
        },
    )
    .unwrap();

    assert!(!service_dir.exists());
    assert!(read_service_states(temp.path()).unwrap().is_empty());
}

#[test]
fn image_rm_rejects_active_service_even_with_force() {
    let temp = tempfile::tempdir().unwrap();
    let service = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    write_state(temp.path(), &service);
    let mut cli = test_cli();
    cli.state_dir = temp.path().to_path_buf();

    let err = cmd_image(
        &cli,
        &ImageArgs {
            command: ImageCommands::Rm {
                service: "openclaw".to_string(),
                force: true,
            },
        },
    )
    .unwrap_err();

    assert!(err.to_string().contains("destroy it before removing"));
    assert!(temp.path().join("services/openclaw/state.json").exists());
}

#[test]
fn image_rm_rejects_unknown_service_phase() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    service.phase = "failed".to_string();
    write_state(temp.path(), &service);
    let mut cli = test_cli();
    cli.state_dir = temp.path().to_path_buf();

    let err = cmd_image(
        &cli,
        &ImageArgs {
            command: ImageCommands::Rm {
                service: "openclaw".to_string(),
                force: true,
            },
        },
    )
    .unwrap_err();

    assert!(err.to_string().contains("unsupported phase"));
    assert!(temp.path().join("services/openclaw/state.json").exists());
}

#[test]
fn image_list_marks_current_build_and_local_image_presence() {
    let temp = tempfile::tempdir().unwrap();
    let mut state = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    state.phase = "built".to_string();
    state.build.build_id = "openclaw-agent-release-20260508123045123".to_string();
    let paths = context_paths(temp.path(), "openclaw");
    state.build.image_path = paths
        .shelter_work_dir
        .join("images")
        .join(&state.build.build_id)
        .join("image.qcow2");
    write_state(temp.path(), &state);
    fs::create_dir_all(state.build.image_path.parent().unwrap()).unwrap();
    fs::write(&state.build.image_path, "image").unwrap();
    let result_path = shelter_build_result_path(&paths.shelter_work_dir, &state.build.build_id);
    fs::write(
        result_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "id": state.build.build_id,
            "image_path": state.build.image_path,
            "reference_value": null,
            "rekor_value": null
        }))
        .unwrap(),
    )
    .unwrap();

    let entries = collect_image_entries(temp.path()).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].service_id, "openclaw");
    assert_eq!(entries[0].phase.as_deref(), Some("built"));
    assert!(entries[0].current);
    assert!(entries[0].image_present);
    assert_eq!(entries[0].image_size, Some(5));
}

#[test]
fn image_list_includes_published_image_status() {
    let temp = tempfile::tempdir().unwrap();
    let mut state = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    state.phase = "built".to_string();
    state.build.build_id = "openclaw-agent-release-20260602112233".to_string();
    let paths = context_paths(temp.path(), "openclaw");
    state.build.image_path = paths
        .shelter_work_dir
        .join("images")
        .join(&state.build.build_id)
        .join("image.qcow2");
    fs::create_dir_all(state.build.image_path.parent().unwrap()).unwrap();
    fs::write(&state.build.image_path, "image").unwrap();
    let source_sha256 = sha256_file(&state.build.image_path).unwrap();
    state.build.published.insert(
        "aliyun/cn-beijing/release/build/sha".to_string(),
        test_published_image(&state.build.build_id, &source_sha256),
    );
    write_state(temp.path(), &state);
    let result_path = shelter_build_result_path(&paths.shelter_work_dir, &state.build.build_id);
    write_build_result(&result_path, &state.build.build_id, &state.build.image_path);

    let entries = collect_image_entries(temp.path()).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].published.as_deref(),
        Some("cn-beijing:m-2ze123abc(avl)")
    );
}

#[test]
fn image_publish_unpublish_and_prune_args_parse() {
    let publish = Cli::parse_from([
        "confidential-agent",
        "image",
        "publish",
        "openclaw",
        "--region",
        "cn-beijing",
        "--variant",
        "release",
        "--no-wait",
    ]);
    match publish.command {
        Commands::Image(ImageArgs {
            command: ImageCommands::Publish(args),
        }) => {
            assert_eq!(args.service, "openclaw");
            assert_eq!(args.region.as_deref(), Some("cn-beijing"));
            assert_eq!(args.variant.as_deref(), Some("release"));
            assert!(args.no_wait);
        }
        other => panic!("expected image publish, got {other:?}"),
    }

    let unpublish = Cli::parse_from([
        "confidential-agent",
        "image",
        "unpublish",
        "openclaw",
        "--image-id",
        "m-2ze123abc",
        "--force",
    ]);
    match unpublish.command {
        Commands::Image(ImageArgs {
            command: ImageCommands::Unpublish(args),
        }) => {
            assert_eq!(args.service, "openclaw");
            assert_eq!(args.image_id.as_deref(), Some("m-2ze123abc"));
            assert!(args.force);
        }
        other => panic!("expected image unpublish, got {other:?}"),
    }

    let prune = Cli::parse_from(["confidential-agent", "image", "prune", "--dry-run"]);
    match prune.command {
        Commands::Image(ImageArgs {
            command: ImageCommands::Prune(args),
        }) => {
            assert!(args.dry_run);
            assert!(!args.all);
        }
        other => panic!("expected image prune, got {other:?}"),
    }
}

#[test]
fn local_state_without_published_fields_deserializes_with_defaults() {
    let json = r#"{
      "schema": "confidential-agent/service-state/v1",
      "service_id": "openclaw",
      "generation": 1,
      "phase": "built",
      "spec": {"path": "/spec.yaml", "sha256": "abc"},
      "build": {
        "build_id": "openclaw-agent-release",
        "image_name": "openclaw-agent",
        "variant": "release",
        "image_path": "/state/image.qcow2",
        "images_dir": "/state/images",
        "cache_dir": "/state/cache"
      },
      "deploy": {
        "provider": "aliyun",
        "run_id": "",
        "resource_name": "",
        "tee": "tdx"
      },
      "service": {"ports": [18789], "connect": [18789]},
      "resources": {},
      "mesh_generation": 0,
      "reference_values": "sample"
    }"#;

    let state: LocalServiceState = serde_json::from_str(json).unwrap();

    assert!(!state.build.remote);
    assert!(state.build.published.is_empty());
    assert!(state.deploy.published_image_id.is_none());
}

#[test]
fn status_view_separates_phase_from_local_image_presence() {
    let temp = tempfile::tempdir().unwrap();
    let mut state = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    state.phase = "deleted".to_string();
    state.deploy.public_ip = None;
    state.deploy.private_ip = None;
    write_state(temp.path(), &state);

    let views = status_views(temp.path(), &[state]);

    assert_eq!(views[0].phase, "deleted");
    assert!(!views[0].local_image.present);
    assert!(!views[0].cloud.present);
    assert_eq!(views[0].cloud.run_id, None);
    assert_eq!(views[0].cloud.resource_name, None);
    assert_eq!(views[0].cloud.public_ip, None);
    assert_eq!(views[0].cloud.tee, None);
}

fn local_state(service_id: &str, ports: Vec<u16>, connect: Vec<ConnectPort>) -> LocalServiceState {
    LocalServiceState {
            schema: LOCAL_SERVICE_STATE_SCHEMA_VERSION.to_string(),
            service_id: service_id.to_string(),
            generation: 1,
            phase: "active".to_string(),
            attestation_mode: "challenge".to_string(),
            spec: LocalSpecState {
                path: PathBuf::from("/spec.yaml"),
                sha256: "spec".to_string(),
            },
            build: LocalBuildState {
                build_id: format!("{service_id}-agent-release"),
                image_name: format!("{service_id}-agent"),
                variant: "release".to_string(),
                image_path: PathBuf::from(format!(
                    "/work/.confidential-agent/services/{service_id}/shelter/images/{service_id}-agent-release/image-{service_id}-agent-release.qcow2"
                )),
                images_dir: PathBuf::from("/work/.confidential-agent/services/test/artifacts"),
                cache_dir: PathBuf::from("/work/.confidential-agent/services/test/cache"),
                debug_ssh: None,
                sample_rv: None,
                rekor_meta: None,
                remote: false,
                published: BTreeMap::new(),
            },
            deploy: LocalDeployState {
                provider: "aliyun".to_string(),
                run_id: "20260429201011".to_string(),
                resource_name: format!("{service_id}-20260429201011"),
                terraform_dir: None,
                image_source: None,
                image_import_name: None,
                bucket: None,
                instance_id: None,
                security_group_id: None,
                private_ip: Some("10.0.1.20".to_string()),
                public_ip: Some("39.0.0.1".to_string()),
                tee: "tdx".to_string(),
                published_image_id: None,
            },
            service: LocalServiceNetwork {
                ports,
                connect,
                mcp_ports: Vec::new(),
            },
            gateway_identity: Some(LocalGatewayIdentity {
                public_key: "pub".to_string(),
                private_key_path: PathBuf::from(format!(
                    "/work/.confidential-agent/services/{service_id}/secrets/gateway_identity.seed"
                )),
            }),
            resources: BTreeMap::new(),
            mesh_generation: 0,
            reference_values: "sample".to_string(),
        }
}

fn write_state(state_dir: &Path, state: &LocalServiceState) {
    let service_dir = state_dir.join("services").join(&state.service_id);
    fs::create_dir_all(&service_dir).unwrap();
    fs::write(
        service_dir.join("state.json"),
        serde_json::to_string_pretty(state).unwrap(),
    )
    .unwrap();
}

fn write_manifest(state_dir: &Path, service_id: &str, build_id: &str) {
    let service_dir = state_dir.join("services").join(service_id);
    fs::create_dir_all(&service_dir).unwrap();
    let shelter_work_dir = service_dir.join("shelter");
    let manifest = BuildManifest {
        service_id: service_id.to_string(),
        shelter_build_id: build_id.to_string(),
        shelter_work_dir: shelter_work_dir.clone(),
        build_result: shelter_build_result_path(&shelter_work_dir, build_id),
        deploy_result: shelter_deploy_result_path(&service_dir.join("terraform").join(build_id)),
        shelter_config: service_dir.join("shelter.yaml"),
        agentd_bin: PathBuf::from("/bin/confidential-agentd"),
        agentd_service: PathBuf::from("/etc/systemd/system/confidential-agentd.service"),
        gateway_bin: PathBuf::from("/bin/cai-gateway"),
        gateway_service: PathBuf::from("/etc/systemd/system/cai-gateway.service"),
        tng_service: PathBuf::from("/etc/systemd/system/trusted-network-gateway.service"),
        initrd_secret_fetch_module: PathBuf::from("/build/99confidential-agent-secret-fetch"),
        fde_config_file: PathBuf::from("/build/fde.toml"),
        policy_default: PathBuf::from("/build/default.rego"),
        policy_local_dev: PathBuf::from("/build/local-dev.rego"),
        images_dir: service_dir.join("artifacts"),
        cache_dir: service_dir.join("cache"),
        guest_setup_script: None,
        extra_files: Vec::new(),
        debug_ssh: None,
        variants: BTreeMap::new(),
    };
    fs::write(
        service_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn test_published_image(build_id: &str, source_sha256: &str) -> PublishedImage {
    PublishedImage {
        provider: "aliyun".to_string(),
        region: "cn-beijing".to_string(),
        variant: "release".to_string(),
        build_id: build_id.to_string(),
        source_sha256: source_sha256.to_string(),
        source_size: 5,
        status: "available".to_string(),
        image_name: format!("ca-pub-openclaw-release-{build_id}"),
        image_id: Some("m-2ze123abc".to_string()),
        import_task_id: Some("t-import".to_string()),
        bucket: Some("ca-images-cn-beijing-test".to_string()),
        object_key: Some("confidential-agent/images/openclaw/release/image.qcow2".to_string()),
        created_at: "2026-06-02T00:00:00Z".to_string(),
        updated_at: "2026-06-02T00:00:00Z".to_string(),
        oss_cleaned: true,
        error: None,
    }
}

fn write_build_result(build_result: &Path, build_id: &str, image_path: &Path) {
    fs::create_dir_all(build_result.parent().unwrap()).unwrap();
    fs::write(
        build_result,
        serde_json::to_string_pretty(&serde_json::json!({
            "id": build_id,
            "image_path": image_path,
            "reference_value": null,
            "rekor_value": null
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn state_dir_lock_serializes_concurrent_writers() {
    let temp = tempfile::tempdir().unwrap();
    let first = lock_state_dir(temp.path()).unwrap();
    let state_dir = temp.path().to_path_buf();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();

    let waiter = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        let _second = lock_state_dir(&state_dir).unwrap();
        done_tx.send(()).unwrap();
    });

    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
    drop(first);
    done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    waiter.join().unwrap();
}

#[test]
fn json_atomic_write_replaces_local_state_content() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("services/openclaw/state.json");

    write_json_atomic(&path, &serde_json::json!({"generation": 1})).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    write_json_atomic(&path, &serde_json::json!({"generation": 2})).unwrap();

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
fn local_state_reader_rejects_schema_before_full_parse() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.json");
    fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "schema": "confidential-agent/service-state/v0"
        }))
        .unwrap(),
    )
    .unwrap();

    let err = read_service_state_file(&path).unwrap_err();

    assert!(err
        .to_string()
        .contains("unsupported local service state schema"));
}

#[test]
fn fetch_daemon_status_reads_readonly_status_endpoint() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        loop {
            let mut chunk = [0u8; 1024];
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let request = std::str::from_utf8(&request).unwrap();
        assert!(request.starts_with("GET /status HTTP/1.1"));
        let body = serde_json::to_string(&DaemonStatus {
            schema: DAEMON_STATUS_SCHEMA_VERSION.to_string(),
            service_id: "openclaw".to_string(),
            phase: "running".to_string(),
            bootstrap_generation: 3,
            mesh_generation: 2,
            applied_resources: BTreeMap::new(),
            mesh_fingerprint: Some("abc123".to_string()),
            app_ready: true,
            mesh_ready: true,
            debug_ssh_ready: true,
            a2a_peers: BTreeMap::new(),
            last_error: None,
        })
        .unwrap();
        write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        stream.flush().unwrap();
    });

    let status = fetch_daemon_status_from("127.0.0.1", port, Duration::from_secs(1)).unwrap();

    assert_eq!(status.service_id, "openclaw");
    assert_eq!(status.phase, "running");
    assert!(status.app_ready);
    assert!(status.mesh_ready);
    server.join().unwrap();
}

#[test]
fn wait_for_daemon_status_retries_until_status_is_ready() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        for attempt in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0u8; 1024];
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            if attempt == 0 {
                write!(
                    stream,
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 22\r\nConnection: close\r\n\r\n{{\"error\":\"not ready\"}}"
                )
                .unwrap();
            } else {
                let body = serde_json::to_string(&DaemonStatus {
                    schema: DAEMON_STATUS_SCHEMA_VERSION.to_string(),
                    service_id: "openclaw".to_string(),
                    phase: "resources-applied".to_string(),
                    bootstrap_generation: 1,
                    mesh_generation: 0,
                    applied_resources: BTreeMap::new(),
                    mesh_fingerprint: None,
                    app_ready: false,
                    mesh_ready: false,
                    debug_ssh_ready: false,
                    a2a_peers: BTreeMap::new(),
                    last_error: None,
                })
                .unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
            stream.flush().unwrap();
        }
    });

    let status = wait_for_daemon_status_from(
        "127.0.0.1",
        port,
        Duration::from_secs(1),
        Duration::from_millis(10),
    )
    .unwrap();

    assert_eq!(status.service_id, "openclaw");
    assert_eq!(status.phase, "resources-applied");
    server.join().unwrap();
}

fn write_script(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    set_mode(path, 0o755).unwrap();
}

fn write_fake_shelter(path: &Path, log: &Path) {
    write_script(
        path,
        &format!(
            r#"#!/usr/bin/env python3.11
import json
import os
import sys

args = sys.argv[1:]
with open("{log}", "a", encoding="utf-8") as fh:
    fh.write(json.dumps(args) + "\n")

def arg_after(flag):
    try:
        return args[args.index(flag) + 1]
    except (ValueError, IndexError):
        return ""

work_dir = arg_after("--work-dir")
if "build" in args:
    image_id = arg_after("--image-id")
    image_dir = os.path.join(work_dir, "images", image_id)
    os.makedirs(image_dir, exist_ok=True)
    image_path = os.path.join(image_dir, f"image-{{image_id}}.qcow2")
    with open(image_path, "w", encoding="utf-8") as fh:
        fh.write("image")
    with open(os.path.join(image_dir, "build-result.json"), "w", encoding="utf-8") as fh:
        json.dump({{
            "id": image_id,
            "image_path": image_path,
            "reference_value": {{"measurement.uki.SHA-384": [image_id]}},
            "rekor_value": None,
        }}, fh)
    sys.exit(0)

if "deploy" in args:
    image_id = args[args.index("deploy") + 1]
    terraform_dir = arg_after("--terraform-dir")
    os.makedirs(terraform_dir, exist_ok=True)
    with open(os.path.join(terraform_dir, "deploy-result.json"), "w", encoding="utf-8") as fh:
        json.dump({{
            "id": image_id,
            "deploy": {{
                "instance_id": "i-test",
                "public_ip": "203.0.113.42",
                "private_ip": "10.0.1.20",
                "outputs": {{"security_group_id": "sg-test"}},
            }},
        }}, fh)
    sys.exit(0)

sys.exit(0)
"#,
            log = log.display()
        ),
    );
}

#[test]
fn effective_shelter_bin_prefers_packaged_binary_when_default_is_used() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::remove_var("CA_SHELTER_BIN");
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("usr-bin-shelter");
    let second = temp.path().join("usr-local-bin-shelter");
    write_script(&first, "#!/bin/sh\nexit 0\n");
    write_script(&second, "#!/bin/sh\nexit 0\n");

    let cli = test_cli();
    assert_eq!(
        effective_shelter_bin_from_candidates(&cli, &[first.clone(), second]),
        first
    );
}

#[test]
fn effective_shelter_bin_respects_explicit_env_override() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("CA_SHELTER_BIN", "shelter");
    let temp = tempfile::tempdir().unwrap();
    let preferred = temp.path().join("preferred-shelter");
    write_script(&preferred, "#!/bin/sh\nexit 0\n");

    let cli = test_cli();
    assert_eq!(
        effective_shelter_bin_from_candidates(&cli, &[preferred]),
        PathBuf::from("shelter")
    );
    std::env::remove_var("CA_SHELTER_BIN");
}

fn write_fake_prepare_tools(temp: &Path) -> PathBuf {
    let current = std::env::current_exe().unwrap();
    let agentd = current.parent().unwrap().join("confidential-agentd");
    if !agentd.exists() {
        fs::write(&agentd, "#!/bin/sh\nexit 0\n").unwrap();
        set_mode(&agentd, 0o755).unwrap();
    }
    let bin_dir = temp.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let docker = bin_dir.join("docker");
    write_script(
        &docker,
        r#"#!/bin/sh
set -eu
case "${1:-}" in
  --version)
    echo "Docker version test"
    exit 0
    ;;
  create)
    echo "container-test"
    exit 0
    ;;
  cp)
    dest="$3"
    printf 'asset\n' > "$dest"
    exit 0
    ;;
  rm)
    exit 0
    ;;
esac
exit 1
"#,
    );
    bin_dir
}

fn write_mesh_bundle(
    state_dir: &Path,
    services: Vec<LocalServiceState>,
    sample: BTreeMap<String, serde_json::Value>,
) {
    let bundle = render_mesh_bundle(
        &services,
        &ReferenceValueArtifacts {
            sample,
            rekor: BTreeMap::new(),
        },
        1,
    );
    fs::write(
        state_dir.join("mesh-bundle.json"),
        serde_json::to_string_pretty(&bundle).unwrap(),
    )
    .unwrap();
}

#[test]
fn tools_container_wraps_challenge_client() {
    let cli = test_cli();
    let args = tools_container_args(
        &cli.tools_image,
        ToolContainerSpec {
            tool: "attestation-challenge-client",
            tool_args: vec![OsString::from("inject-resource")],
            mounts: vec![PathBuf::from("/tmp/resources")],
            envs: vec![("NO_PROXY".to_string(), "39.105.79.128".to_string())],
            workdir: Some(PathBuf::from("/work")),
            container_name: None,
        },
    );

    let args = args
        .iter()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect::<Vec<_>>();

    assert_eq!(args[0], "run");
    assert!(args.windows(2).any(|pair| pair == ["--network", "host"]));
    assert!(args.contains(&"/tmp/resources:/tmp/resources".to_string()));
    assert!(args.windows(2).any(|pair| pair == ["--env", "NO_PROXY"]));
    assert!(!args
        .iter()
        .any(|arg| arg.contains("NO_PROXY=39.105.79.128")));
    assert_eq!(
        args.iter()
            .position(|arg| arg == "confidential-agent-tools:test")
            .map(|idx| &args[idx + 1]),
        Some(&"attestation-challenge-client".to_string())
    );
}

#[test]
fn tools_container_wraps_tng_connect() {
    let cli = test_cli();
    let args = tools_container_args(
        &cli.tools_image,
        ToolContainerSpec {
            tool: "tng",
            tool_args: vec![
                OsString::from("launch"),
                OsString::from("--config-content={}"),
            ],
            mounts: Vec::new(),
            envs: Vec::new(),
            workdir: None,
            container_name: Some("ca-connect-test".to_string()),
        },
    );

    let args = args
        .iter()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    let image_idx = args
        .iter()
        .position(|arg| arg == "confidential-agent-tools:test")
        .unwrap();

    assert!(args
        .windows(2)
        .any(|pair| pair == ["--name", "ca-connect-test"]));
    assert_eq!(
        &args[image_idx + 1..],
        ["tng", "launch", "--config-content={}"]
    );
}

#[test]
fn cryptpilot_fde_config_matches_current_schema() {
    let config = cryptpilot_fde_config();

    assert!(config.contains("[rootfs]"));
    assert!(config.contains("delta_location = \"disk-persist\""));
    assert!(config.contains("delta_backend = \"dm-snapshot\""));
    assert!(config.contains("[delta]"));
    assert!(config.contains("integrity = false"));
    assert!(config.contains("[delta.encrypt.exec]"));
    assert!(config.contains("command = \"/bin/bash\""));
    assert!(config.contains(
        "args = [\"-c\", \"CA_SECRET_WAIT_TIMEOUT_SEC=210 /usr/bin/confidential-agentd initrd-fetch >/dev/console 2>&1 && /usr/bin/cat /run/cai/secrets/disk_key\"]"
    ));
    assert!(!config.contains("initrd-fetch 1>&2"));
    assert!(!config.contains("rw_overlay"));
    assert!(!config.contains("[data]"));
}

#[test]
fn deploy_args_use_positional_build_id_without_cloud_image_override() {
    let prepared = PreparedConfig {
        rendered_config: PathBuf::from("/state/shelter.yaml"),
        shelter_build_id: "cai-e2e-agent-release".to_string(),
        shelter_work_dir: PathBuf::from("/state/shelter-work"),
        build_result: PathBuf::from(
            "/state/shelter-work/images/cai-e2e-agent-release/build-result.json",
        ),
        deploy_result: PathBuf::from(
            "/state/shelter-work/deploy/cai-e2e-agent-release/deploy-result.json",
        ),
        deploy_names: None,
        terraform_dir: None,
        debug_ssh: None,
        cloud_image_id: None,
    };

    let args = deploy_shelter_args(&prepared);

    assert_eq!(args[0], OsStr::new("--work-dir"));
    assert_eq!(args[1], OsStr::new("/state/shelter-work"));
    assert_eq!(args[2], OsStr::new("deploy"));
    assert_eq!(args[3], OsStr::new("cai-e2e-agent-release"));
    assert!(!args.iter().any(|arg| arg == OsStr::new("--image-id")));
    assert!(!args.iter().any(|arg| arg == OsStr::new("--cloud-image-id")));
    assert!(args.iter().any(|arg| arg == OsStr::new("--auto-approve")));
}

#[test]
fn deploy_args_pass_confidential_agent_terraform_dir_to_shelter() {
    let prepared = PreparedConfig {
        rendered_config: PathBuf::from("/state/shelter.yaml"),
        shelter_build_id: "cai-e2e-agent-debug".to_string(),
        shelter_work_dir: PathBuf::from("/state/shelter-work"),
        build_result: PathBuf::from(
            "/state/shelter-work/images/cai-e2e-agent-debug/build-result.json",
        ),
        deploy_result: PathBuf::from(
            "/state/services/openclaw/terraform/20260506130000/deploy-result.json",
        ),
        deploy_names: None,
        terraform_dir: Some(PathBuf::from(
            "/state/services/openclaw/terraform/20260506130000",
        )),
        debug_ssh: None,
        cloud_image_id: None,
    };

    let args = deploy_shelter_args(&prepared);

    let terraform_dir = args
        .windows(2)
        .find(|window| window[0] == OsStr::new("--terraform-dir"))
        .map(|window| window[1].as_os_str());
    assert_eq!(
        terraform_dir,
        Some(OsStr::new(
            "/state/services/openclaw/terraform/20260506130000"
        ))
    );
}

#[test]
fn build_runs_all_enabled_variants_and_records_manifest_entries() {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = write_fake_prepare_tools(temp.path());
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{}", fake_bin.display(), old_path.to_string_lossy()),
    );
    let spec = temp.path().join("openclaw.yaml");
    fs::write(
        &spec,
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
  variants:
    release:
      enabled: true
    debug:
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
"#,
    )
    .unwrap();
    let shelter = temp.path().join("fake-shelter");
    let log = temp.path().join("shelter.log");
    write_fake_shelter(&shelter, &log);
    let mut cli = test_cli();
    cli.state_dir = temp.path().join("state");
    cli.shelter_bin = shelter;

    cmd_build(
        &cli,
        &BuildArgs {
            spec: spec.clone(),
            render_only: false,
        },
    )
    .unwrap();
    std::env::set_var("PATH", old_path);

    let invocations = fs::read_to_string(&log).unwrap();
    assert!(invocations.contains("openclaw-agent-release-"));
    assert!(invocations.contains("openclaw-agent-debug-"));
    let manifest_path = cli.state_dir.join("services/openclaw/manifest.json");
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(manifest_path).unwrap()).unwrap();
    assert!(manifest["variants"]["release"]["shelter_build_id"]
        .as_str()
        .unwrap()
        .starts_with("openclaw-agent-release-"));
    assert!(manifest["variants"]["debug"]["shelter_build_id"]
        .as_str()
        .unwrap()
        .starts_with("openclaw-agent-debug-"));
}

#[test]
fn build_render_only_ignores_peerings_and_keeps_selected_variant_rendered_config() {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = write_fake_prepare_tools(temp.path());
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{}", fake_bin.display(), old_path.to_string_lossy()),
    );
    let spec = temp.path().join("openclaw.yaml");
    fs::write(
        &spec,
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
  variants:
    release:
      enabled: true
    debug:
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
"#,
    )
    .unwrap();
    let mut cli = test_cli();
    cli.state_dir = temp.path().join("state");
    PeeringsFile {
        version: 1,
        peerings: vec![PeeringEntry {
            label: "ops".to_string(),
            role: PeeringRole::Operator,
            cidr: "203.0.113.10/32".to_string(),
            scope: Vec::new(),
            note: None,
            added_at: None,
            added_by: None,
        }],
    }
    .write_to_path(&cli.state_dir.join("peerings.yaml"))
    .unwrap();

    cmd_build(
        &cli,
        &BuildArgs {
            spec,
            render_only: true,
        },
    )
    .unwrap();
    std::env::set_var("PATH", old_path);

    let rendered =
        fs::read_to_string(cli.state_dir.join("services/openclaw/shelter.yaml")).unwrap();
    assert!(!rendered.contains("variants:"));
    assert!(!rendered.contains("resize:"));
    assert!(!rendered.contains("deploy:"));
    assert!(!rendered.contains("security_group_ports:"));
    assert!(!rendered.contains("control_8006_peer_203_0_113_10_32"));
    assert!(!rendered.contains("name: debug"));
}

#[test]
fn deploy_render_only_includes_operator_peering_security_group_rules() {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = write_fake_prepare_tools(temp.path());
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{}", fake_bin.display(), old_path.to_string_lossy()),
    );
    let spec = temp.path().join("openclaw.yaml");
    fs::write(
        &spec,
        r#"
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
  reference_values: sample
resources: {}
"#,
    )
    .unwrap();
    let shelter = temp.path().join("fake-shelter");
    let log = temp.path().join("shelter.log");
    write_fake_shelter(&shelter, &log);
    let mut cli = test_cli();
    cli.state_dir = temp.path().join("state");
    cli.shelter_bin = shelter;

    cmd_build(
        &cli,
        &BuildArgs {
            spec: spec.clone(),
            render_only: false,
        },
    )
    .unwrap();
    let err = cmd_deploy(
        &cli,
        &DeployArgs {
            spec: spec.clone(),
            skip_inject: false,
            render_only: false,
            skip_peering_check: false,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("no operator peering"));

    PeeringsFile {
        version: 1,
        peerings: vec![PeeringEntry {
            label: "ops".to_string(),
            role: PeeringRole::Operator,
            cidr: "203.0.113.10/32".to_string(),
            scope: Vec::new(),
            note: None,
            added_at: None,
            added_by: None,
        }],
    }
    .write_to_path(&cli.state_dir.join("peerings.yaml"))
    .unwrap();

    cmd_deploy(
        &cli,
        &DeployArgs {
            spec,
            skip_inject: true,
            render_only: true,
            skip_peering_check: false,
        },
    )
    .unwrap();
    std::env::set_var("PATH", old_path);

    let rendered =
        fs::read_to_string(cli.state_dir.join("services/openclaw/shelter.yaml")).unwrap();
    assert!(rendered.contains("deploy:"));
    assert!(rendered.contains("backend: terraform"));
    assert!(rendered.contains("cc_mode: tdx"));
    assert!(!rendered.contains("security_group_ports:"));
    assert!(!rendered.contains("security_group_allowed_cidr:"));
    assert!(!rendered.contains("tdx: true"));
    assert!(rendered.contains("control_8006_peer_203_0_113_10_32"));
    assert!(rendered.contains("status_8088_peer_203_0_113_10_32"));
    assert!(rendered.contains("connect_18789_peer_203_0_113_10_32"));
}

#[test]
fn render_service_config_from_state_refreshes_peering_rules_without_rebuild() {
    let temp = tempfile::tempdir().unwrap();
    let state_dir = temp.path().join("state");
    let spec_path = temp.path().join("openclaw.yaml");
    fs::write(
        &spec_path,
        r#"
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
  reference_values: sample
resources: {}
"#,
    )
    .unwrap();
    let mut state = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    state.spec.path = spec_path;
    state.deploy.terraform_dir = Some(
        state_dir
            .join("services")
            .join("openclaw")
            .join("terraform")
            .join("active"),
    );
    state.deploy.image_import_name = Some("openclaw-agent-release-20260429201011".to_string());
    write_state(&state_dir, &state);
    write_manifest(&state_dir, "openclaw", &state.build.build_id);

    PeeringsFile {
        version: 1,
        peerings: vec![PeeringEntry {
            label: "ops".to_string(),
            role: PeeringRole::Operator,
            cidr: "203.0.113.10/32".to_string(),
            scope: Vec::new(),
            note: None,
            added_at: None,
            added_by: None,
        }],
    }
    .write_to_path(&state_dir.join("peerings.yaml"))
    .unwrap();
    render_service_config_from_state(&state_dir, &state, Vec::new()).unwrap();
    let rendered = fs::read_to_string(state_dir.join("services/openclaw/shelter.yaml")).unwrap();
    assert!(rendered.contains("deploy:"));
    assert!(rendered.contains("control_8006_peer_203_0_113_10_32"));
    assert!(rendered.contains("connect_18789_peer_203_0_113_10_32"));

    PeeringsFile::empty()
        .write_to_path(&state_dir.join("peerings.yaml"))
        .unwrap();
    render_service_config_from_state(&state_dir, &state, Vec::new()).unwrap();
    let rendered = fs::read_to_string(state_dir.join("services/openclaw/shelter.yaml")).unwrap();
    assert!(rendered.contains("deploy:"));
    assert!(!rendered.contains("control_8006_peer_203_0_113_10_32"));
    assert!(!rendered.contains("connect_18789_peer_203_0_113_10_32"));
}

#[test]
fn guest_setup_installs_staged_attestation_challenge_client() {
    let script = guest_setup_script();
    assert!(script.contains("attestation-challenge-client"));
    assert!(script.contains("/opt/confidential-agent/hack/attestation-challenge-client"));
    assert!(!script.contains("attestation-challenge-client package requires yum or dnf"));
    assert!(script.contains("/usr/bin/attestation-challenge-client"));
}

#[test]
fn deploy_uses_requested_variant_from_multi_variant_manifest() {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let fake_bin = write_fake_prepare_tools(temp.path());
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{}", fake_bin.display(), old_path.to_string_lossy()),
    );
    let spec = temp.path().join("openclaw.yaml");
    fs::write(
        &spec,
        r#"
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
    debug:
      enabled: true
deploy:
  provider: aliyun
  image_variant: debug
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#,
    )
    .unwrap();
    let service_dir = temp.path().join("state/services/openclaw");
    let shelter_dir = service_dir.join("shelter");
    let release_id = "openclaw-agent-release-20260511130000";
    let debug_id = "openclaw-agent-debug-20260511130000";
    let release_image = shelter_dir
        .join("images")
        .join(release_id)
        .join("image.qcow2");
    let debug_image = shelter_dir
        .join("images")
        .join(debug_id)
        .join("image.qcow2");
    fs::create_dir_all(release_image.parent().unwrap()).unwrap();
    fs::create_dir_all(debug_image.parent().unwrap()).unwrap();
    fs::write(&release_image, "release").unwrap();
    fs::write(&debug_image, "debug").unwrap();
    for (id, image) in [(release_id, &release_image), (debug_id, &debug_image)] {
        let result = shelter_build_result_path(&shelter_dir, id);
        fs::create_dir_all(result.parent().unwrap()).unwrap();
        fs::write(
            result,
            serde_json::to_string_pretty(&serde_json::json!({
                "id": id,
                "image_path": image,
                "reference_value": {"measurement.uki.SHA-384": [id]},
                "rekor_value": null
            }))
            .unwrap(),
        )
        .unwrap();
    }
    let mut state = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    state.phase = "built".to_string();
    state.build.build_id = release_id.to_string();
    state.build.variant = "release".to_string();
    state.build.image_path = release_image.clone();
    write_state(&temp.path().join("state"), &state);
    fs::create_dir_all(&service_dir).unwrap();
    fs::write(
        service_dir.join("manifest.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "service_id": "openclaw",
            "shelter_build_id": release_id,
            "shelter_work_dir": shelter_dir,
            "build_result": shelter_build_result_path(&shelter_dir, release_id),
            "deploy_result": shelter_deploy_result_path(&service_dir.join("terraform/unused")),
            "shelter_config": service_dir.join("shelter.yaml"),
            "agentd_bin": "/bin/confidential-agentd",
            "agentd_service": "/etc/systemd/system/confidential-agentd.service",
            "gateway_bin": "/bin/cai-gateway",
            "gateway_service": "/etc/systemd/system/cai-gateway.service",
            "tng_service": "/etc/systemd/system/trusted-network-gateway.service",
            "initrd_secret_fetch_module": "/build/99confidential-agent-secret-fetch",
            "fde_config_file": "/build/fde.toml",
            "policy_default": "/build/default.rego",
            "policy_local_dev": "/build/local-dev.rego",
            "images_dir": service_dir.join("artifacts"),
            "cache_dir": service_dir.join("cache"),
            "variants": {
                "release": {
                    "shelter_build_id": release_id,
                    "build_result": shelter_build_result_path(&shelter_dir, release_id),
                    "extra_files": []
                },
                "debug": {
                    "shelter_build_id": debug_id,
                    "build_result": shelter_build_result_path(&shelter_dir, debug_id),
                    "extra_files": []
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let shelter = temp.path().join("fake-shelter");
    let log = temp.path().join("shelter.log");
    write_fake_shelter(&shelter, &log);
    let mut cli = test_cli();
    cli.state_dir = temp.path().join("state");
    cli.shelter_bin = shelter;

    cmd_deploy(
        &cli,
        &DeployArgs {
            spec,
            skip_inject: true,
            render_only: false,
            skip_peering_check: true,
        },
    )
    .unwrap();
    std::env::set_var("PATH", old_path);

    let invocations = fs::read_to_string(log).unwrap();
    assert!(invocations.contains(&format!("\"deploy\", \"{debug_id}\"")));
    assert!(!invocations.contains(&format!("\"deploy\", \"{release_id}\"")));
    let deployed = read_service_state_file(&service_dir.join("state.json"))
        .unwrap()
        .unwrap();
    assert_eq!(deployed.build.build_id, debug_id);
    assert_eq!(deployed.build.variant, "debug");
    assert_eq!(deployed.build.image_path, debug_image);
}

#[test]
fn latest_built_image_reads_shelter_build_result_json() {
    let temp = tempfile::tempdir().unwrap();
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
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
"#,
        Path::new("/project"),
    )
    .unwrap();
    let paths = context_paths(temp.path(), "openclaw");
    let build_id = shelter_build_id(&spec);
    let result_path = shelter_build_result_path(&paths.shelter_work_dir, &build_id);
    fs::create_dir_all(result_path.parent().unwrap()).unwrap();
    fs::write(
            &result_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "id": build_id,
                "image_path": "/state/shelter/images/openclaw-agent-release/image-openclaw-agent-release.qcow2",
                "reference_value": {"measurement.uki.SHA-384": ["abc"]},
                "rekor_value": null
            }))
            .unwrap(),
        )
        .unwrap();

    let image = latest_built_image(temp.path(), &spec).unwrap();

    assert_eq!(
        image,
        PathBuf::from(
            "/state/shelter/images/openclaw-agent-release/image-openclaw-agent-release.qcow2"
        )
    );
}

#[test]
fn render_bootstrap_carries_app_service_for_daemon_readiness() {
    let temp = tempfile::tempdir().unwrap();
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
  app_service: cai-openclaw-gateway.service
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
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
"#,
        Path::new("/project"),
    )
    .unwrap();
    let paths = context_paths(temp.path(), "openclaw");

    let bootstrap = render_bootstrap(&paths, &spec).unwrap();

    assert_eq!(
        bootstrap.app_service.as_deref(),
        Some("cai-openclaw-gateway.service")
    );
}

#[test]
fn write_bootstrap_file_protects_embedded_gateway_private_key() {
    let temp = tempfile::tempdir().unwrap();
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
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
"#,
        Path::new("/project"),
    )
    .unwrap();
    let paths = context_paths(temp.path(), "openclaw");
    let bootstrap = render_bootstrap(&paths, &spec).unwrap();
    fs::write(&paths.bootstrap_file, "{}\n").unwrap();
    fs::set_permissions(&paths.bootstrap_file, fs::Permissions::from_mode(0o644)).unwrap();

    write_bootstrap_file(&paths.bootstrap_file, &bootstrap).unwrap();

    let persisted: BootstrapConfig =
        serde_json::from_str(&fs::read_to_string(&paths.bootstrap_file).unwrap()).unwrap();
    let identity = bootstrap
        .gateway_identity
        .as_ref()
        .expect("bootstrap should embed gateway identity");
    assert!(!identity.private_key.is_empty());
    assert_eq!(
        persisted.gateway_identity.as_ref().unwrap().private_key,
        identity.private_key
    );
    assert_eq!(
        fs::metadata(&paths.bootstrap_file)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(paths.service_dir.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&paths.service_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&paths.secrets_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn write_bootstrap_file_rejects_symlink_target() {
    let temp = tempfile::tempdir().unwrap();
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
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
"#,
        Path::new("/project"),
    )
    .unwrap();
    let paths = context_paths(temp.path(), "openclaw");
    let bootstrap = render_bootstrap(&paths, &spec).unwrap();
    let target = temp.path().join("target.json");
    fs::write(&target, "sentinel\n").unwrap();
    std::os::unix::fs::symlink(&target, &paths.bootstrap_file).unwrap();

    let err = write_bootstrap_file(&paths.bootstrap_file, &bootstrap).unwrap_err();

    assert!(format!("{err:#}").contains("symlink"));
    assert_eq!(fs::read_to_string(&target).unwrap(), "sentinel\n");
}

#[test]
fn render_agent_card_uses_rekor_metadata_and_rejects_disabled_a2a() {
    let yaml = r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789, 18800]
  connect: [18789]
build:
  image_name: openclaw-agent
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
    cosign_key: ./cosign.key
resources: {}
a2a:
  id: openclaw-agent
  name: OpenClaw
  cacheTtlSec: 120
  skills:
    - id: chat
      name: Chat
"#;
    let spec = AgentSpec::from_yaml(yaml, Path::new("/project")).unwrap();
    let meta = serde_json::json!({
        "rekor_url": "https://rekor.sigstore.dev",
        "artifact_id": "openclaw-release",
        "artifact_type": "uki",
        "artifact_version": "20260514",
        "rv_name": "measurement.uki.SHA-384"
    });

    let sample = serde_json::json!({"measurement.uki.SHA-384": ["abc123"]});
    let card = render_agent_card(&spec, "198.51.100.20", &meta, Some(&sample)).unwrap();

    assert_eq!(card.name, "OpenClaw");
    assert_eq!(card.protocol_version, "1.0");
    assert_eq!(
        card.supported_interfaces[0].url,
        "http://198.51.100.20:18789/a2a"
    );
    let ext = confidential_extension(&card).unwrap();
    assert_eq!(ext.id, "openclaw-agent");
    assert_eq!(ext.cache_ttl_sec, 120);
    assert_eq!(
        ext.ports.iter().map(|port| port.port).collect::<Vec<_>>(),
        vec![18789]
    );
    assert_eq!(ext.rekor.artifact_id, "openclaw-release");
    assert_eq!(ext.reference_values.as_ref(), Some(&sample));

    let disabled = AgentSpec::from_yaml(
        &yaml.replace("a2a:\n", "a2a:\n  enabled: false\n"),
        Path::new("/project"),
    )
    .unwrap();
    let err = render_agent_card(&disabled, "198.51.100.20", &meta, Some(&sample)).unwrap_err();
    assert!(err.to_string().contains("a2a is disabled"));

    let no_connect = AgentSpec::from_yaml(
        &yaml.replace("  connect: [18789]\n", "  connect: []\n"),
        Path::new("/project"),
    )
    .unwrap();
    let err = render_agent_card(&no_connect, "198.51.100.20", &meta, Some(&sample)).unwrap_err();
    assert!(err.to_string().contains("a2a requires service.connect"));
}

#[test]
fn latest_built_image_uses_current_state_build_id() {
    let temp = tempfile::tempdir().unwrap();
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
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
"#,
        Path::new("/project"),
    )
    .unwrap();
    let build_id = "openclaw-agent-release-20260508123045123";
    let paths = context_paths(temp.path(), "openclaw");
    let mut state = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    state.phase = "built".to_string();
    state.build.build_id = build_id.to_string();
    state.build.image_path =
        PathBuf::from("/state/shelter/images/openclaw-agent-release-20260508123045123/image.qcow2");
    write_state(temp.path(), &state);
    let result_path = shelter_build_result_path(&paths.shelter_work_dir, build_id);
    fs::create_dir_all(result_path.parent().unwrap()).unwrap();
    fs::write(
        &result_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "id": build_id,
            "image_path": state.build.image_path,
            "reference_value": {"measurement.uki.SHA-384": ["abc"]},
            "rekor_value": null
        }))
        .unwrap(),
    )
    .unwrap();

    let image = latest_built_image(temp.path(), &spec).unwrap();

    assert_eq!(
        image,
        PathBuf::from("/state/shelter/images/openclaw-agent-release-20260508123045123/image.qcow2")
    );
}

#[test]
fn materialize_build_artifacts_reads_manifest_result_path() {
    let temp = tempfile::tempdir().unwrap();
    let paths = context_paths(temp.path(), "openclaw");
    let result_path = temp.path().join("custom-build-result.json");
    fs::write(
        &result_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "openclaw-agent-release",
            "image_path": "/shelter/images/openclaw-agent-release/image.qcow2",
            "reference_value": {"measurement.uki.SHA-384": ["abc"]},
            "rekor_value": null
        }))
        .unwrap(),
    )
    .unwrap();

    let artifacts =
        materialize_shelter_build_artifacts(&paths, &result_path, "openclaw-agent-release")
            .unwrap();

    assert_eq!(
        artifacts.image_path,
        PathBuf::from("/shelter/images/openclaw-agent-release/image.qcow2")
    );
    assert_eq!(
        artifacts.sample_rv.unwrap(),
        paths.service_dir.join("shelter-reference-values.json")
    );
}

#[test]
fn resolve_deploy_observation_reads_shelter_deploy_result_json() {
    let temp = tempfile::tempdir().unwrap();
    let deploy_result = temp.path().join("deploy-result.json");
    fs::write(
        &deploy_result,
        serde_json::to_string_pretty(&serde_json::json!({
            "id": "openclaw-agent-release",
            "image_path": "/image.qcow2",
            "reference_value": null,
            "rekor_value": null,
            "deploy": {
                "name": "openclaw-20260429103011",
                "instance_id": {"value": "i-test"},
                "public_ip": {"value": "39.0.0.1"},
                "private_ip": {"value": "10.0.0.8"},
                "components": null,
                "outputs": {
                    "security_group_id": {"value": "sg-test"}
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let prepared = PreparedConfig {
        rendered_config: PathBuf::from("/state/shelter.yaml"),
        shelter_build_id: "openclaw-agent-release".to_string(),
        shelter_work_dir: temp.path().join("shelter"),
        build_result: temp
            .path()
            .join("shelter/images/openclaw-agent-release/build-result.json"),
        deploy_result,
        deploy_names: None,
        terraform_dir: None,
        debug_ssh: None,
        cloud_image_id: None,
    };
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
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
"#,
        Path::new("/project"),
    )
    .unwrap();

    let observation = resolve_deploy_observation(&prepared, &spec).unwrap();

    assert_eq!(observation.instance_id.as_deref(), Some("i-test"));
    assert_eq!(observation.security_group_id.as_deref(), Some("sg-test"));
    assert_eq!(observation.public_ip.as_deref(), Some("39.0.0.1"));
    assert_eq!(observation.private_ip.as_deref(), Some("10.0.0.8"));
}

#[test]
fn service_state_preserves_generated_image_import_name_without_explicit_image_source() {
    let temp = tempfile::tempdir().unwrap();
    let spec_path = temp.path().join("openclaw.yaml");
    fs::write(
        &spec_path,
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  image_name: openclaw-agent
  variants:
    debug:
      enabled: true
deploy:
  provider: aliyun
  image_variant: debug
  instance_type: ecs.gn8v-tee.4xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#,
    )
    .unwrap();
    let spec = AgentSpec::from_path(&spec_path).unwrap();
    let names = DeployNames::from_run_id(&spec, "20260506130056");
    let paths = context_paths(temp.path(), "openclaw");
    let build_id = shelter_build_id(&spec);
    let build_result = shelter_build_result_path(&paths.shelter_work_dir, &build_id);
    fs::create_dir_all(build_result.parent().unwrap()).unwrap();
    fs::write(
            &build_result,
            serde_json::to_string_pretty(&serde_json::json!({
                "id": build_id,
                "image_path": "/state/shelter/images/openclaw-agent-debug/image-openclaw-agent-debug.qcow2",
                "reference_value": null,
                "rekor_value": null
            }))
            .unwrap(),
        )
        .unwrap();
    let prepared = PreparedConfig {
        rendered_config: paths.rendered_config.clone(),
        shelter_build_id: build_id,
        shelter_work_dir: paths.shelter_work_dir.clone(),
        build_result,
        deploy_result: shelter_deploy_result_path(&paths.service_dir.join("terraform/active")),
        deploy_names: Some(names.clone()),
        terraform_dir: Some(paths.service_dir.join("terraform/active")),
        debug_ssh: None,
        cloud_image_id: None,
    };

    let state = build_service_state(
        temp.path(),
        &spec_path,
        &spec,
        &DeployObservation::default(),
        &prepared,
        "deployed",
    )
    .unwrap();

    assert_eq!(
        state.deploy.image_import_name.as_deref(),
        Some(names.image_import_name.as_str())
    );
}

#[test]
fn service_state_records_published_image_id_from_prepared_config() {
    let temp = tempfile::tempdir().unwrap();
    let spec_path = temp.path().join("openclaw.yaml");
    fs::write(
        &spec_path,
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  image_name: openclaw-agent
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
"#,
    )
    .unwrap();
    let spec = AgentSpec::from_path(&spec_path).unwrap();
    let paths = context_paths(temp.path(), "openclaw");
    let build_id = shelter_build_id(&spec);
    let build_result = shelter_build_result_path(&paths.shelter_work_dir, &build_id);
    let image_path = temp.path().join("image.qcow2");
    write_build_result(&build_result, &build_id, &image_path);
    fs::write(&image_path, "image").unwrap();
    let prepared = PreparedConfig {
        rendered_config: paths.rendered_config.clone(),
        shelter_build_id: build_id,
        shelter_work_dir: paths.shelter_work_dir.clone(),
        build_result,
        deploy_result: shelter_deploy_result_path(&paths.service_dir.join("terraform/active")),
        deploy_names: Some(DeployNames::new(&spec)),
        terraform_dir: Some(paths.service_dir.join("terraform/active")),
        debug_ssh: None,
        cloud_image_id: Some("m-2ze123abc".to_string()),
    };

    let state = build_service_state(
        temp.path(),
        &spec_path,
        &spec,
        &DeployObservation::default(),
        &prepared,
        "deployed",
    )
    .unwrap();

    assert_eq!(
        state.deploy.published_image_id.as_deref(),
        Some("m-2ze123abc")
    );
}

#[test]
fn render_service_config_from_state_preserves_published_image_id() {
    let temp = tempfile::tempdir().unwrap();
    let spec_path = temp.path().join("openclaw.yaml");
    fs::write(
        &spec_path,
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
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
"#,
    )
    .unwrap();
    let mut state = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    state.phase = "active".to_string();
    state.spec.path = spec_path;
    state.deploy.published_image_id = Some("m-2ze123abc".to_string());
    write_state(temp.path(), &state);
    write_manifest(temp.path(), "openclaw", &state.build.build_id);

    render_service_config_from_state(temp.path(), &state, Vec::new()).unwrap();

    let rendered =
        fs::read_to_string(context_paths(temp.path(), "openclaw").rendered_config).unwrap();
    let deploy_section = rendered.split("deploy:").nth(1).unwrap();
    assert!(deploy_section.contains("image_id: m-2ze123abc"));
    assert!(!deploy_section.contains("image:\n"));
}

#[test]
fn deploy_terraform_dir_is_scoped_by_resource_name() {
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
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
"#,
        Path::new("/project"),
    )
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let paths = context_paths(temp.path(), "openclaw");
    let names = DeployNames::from_run_id(&spec, "20260429103011");

    assert_eq!(
        deploy_terraform_dir(&paths, Some(&PathBuf::from("/terraform")), Some(&names)).unwrap(),
        PathBuf::from("/terraform/openclaw-20260429103011")
    );
    assert_eq!(
        deploy_terraform_dir(&paths, None, Some(&names)).unwrap(),
        paths.service_dir.join("terraform/20260429103011")
    );
    assert_eq!(
        deploy_terraform_dir(&paths, Some(&PathBuf::from("/terraform")), None).unwrap(),
        PathBuf::from("/terraform")
    );
}

#[test]
fn debug_deploy_without_configured_key_generates_stable_key_paths() {
    let temp = tempfile::tempdir().unwrap();
    let spec_yaml = r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
  variants:
    release:
      enabled: true
    debug:
      enabled: true
deploy:
  provider: aliyun
  image_variant: debug
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#;
    let mut spec = AgentSpec::from_yaml(spec_yaml, temp.path()).unwrap();
    let paths = context_paths(temp.path(), "openclaw");

    let generated = ensure_debug_ssh_key_with_generator(&paths, &mut spec, |private, public| {
        fs::write(private, "PRIVATE").unwrap();
        fs::write(public, "PUBLIC").unwrap();
        Ok(())
    })
    .unwrap()
    .unwrap();

    assert_eq!(generated.private_key, paths.secrets_dir.join("debug_ssh"));
    assert_eq!(
        generated.public_key,
        paths.secrets_dir.join("debug_ssh.pub")
    );
    assert_eq!(
        spec.build
            .variants
            .debug
            .as_ref()
            .and_then(|debug| debug.ssh_public_key.as_ref()),
        Some(&generated.public_key)
    );

    let mut fresh_spec = AgentSpec::from_yaml(spec_yaml, temp.path()).unwrap();
    let reused = ensure_debug_ssh_key_with_generator(&paths, &mut fresh_spec, |_, _| {
        panic!("existing generated debug SSH key should be reused")
    })
    .unwrap();
    assert_eq!(reused, Some(generated.clone()));

    let reused = ensure_debug_ssh_key_with_generator(&paths, &mut spec, |_, _| {
        panic!("configured debug SSH key should not be replaced")
    })
    .unwrap();
    assert_eq!(reused, None);
}

#[test]
fn mkosi_debug_deploy_stages_authorized_keys() {
    let temp = tempfile::tempdir().unwrap();
    let paths = context_paths(temp.path(), "openclaw");
    fs::create_dir_all(&paths.secrets_dir).unwrap();
    fs::create_dir_all(&paths.guest_staging_dir).unwrap();
    let public_key = paths.secrets_dir.join("debug_ssh.pub");
    fs::write(&public_key, "ssh-ed25519 AAAA test\n").unwrap();
    let debug_ssh = LocalDebugSshKey {
        private_key: paths.secrets_dir.join("debug_ssh"),
        public_key,
    };
    let mut assets = GuestAssets {
        agentd_bin: paths.guest_staging_dir.join("confidential-agentd"),
        agentd_service: paths.guest_staging_dir.join("confidential-agentd.service"),
        gateway_bin: paths.guest_staging_dir.join("cai-gateway"),
        gateway_service: paths.guest_staging_dir.join("cai-gateway.service"),
        tng_service: paths
            .guest_staging_dir
            .join("trusted-network-gateway.service"),
        initrd_secret_fetch_module: paths
            .guest_staging_dir
            .join("99confidential-agent-secret-fetch"),
        fde_config_file: paths.guest_staging_dir.join("fde.toml"),
        policy_default: paths.guest_staging_dir.join("default.rego"),
        policy_local_dev: paths.guest_staging_dir.join("local-dev.rego"),
        guest_setup_script: None,
        extra_files: Vec::new(),
    };

    stage_mkosi_debug_ssh_authorized_keys(&mut assets, &paths, Some(&debug_ssh.public_key))
        .unwrap();

    assert_eq!(assets.extra_files.len(), 2);
    assert_eq!(
        assets.extra_files[0].destination,
        "/root/.ssh/authorized_keys"
    );
    assert_eq!(
        assets.extra_files[1].destination,
        "/etc/confidential-agent/debug-ssh-enabled"
    );
    assert_eq!(
        fs::read_to_string(&assets.extra_files[0].source).unwrap(),
        "ssh-ed25519 AAAA test\n"
    );
    assert_eq!(
        fs::read_to_string(&assets.extra_files[1].source).unwrap(),
        "1\n"
    );
}

#[test]
fn mkosi_debug_deploy_stages_configured_authorized_keys() {
    let temp = tempfile::tempdir().unwrap();
    let paths = context_paths(temp.path(), "openclaw");
    fs::create_dir_all(&paths.guest_staging_dir).unwrap();
    let public_key = temp.path().join("configured_debug_ssh.pub");
    fs::write(&public_key, "ssh-ed25519 BBBB configured\n").unwrap();
    let mut assets = GuestAssets {
        agentd_bin: paths.guest_staging_dir.join("confidential-agentd"),
        agentd_service: paths.guest_staging_dir.join("confidential-agentd.service"),
        gateway_bin: paths.guest_staging_dir.join("cai-gateway"),
        gateway_service: paths.guest_staging_dir.join("cai-gateway.service"),
        tng_service: paths
            .guest_staging_dir
            .join("trusted-network-gateway.service"),
        initrd_secret_fetch_module: paths
            .guest_staging_dir
            .join("99confidential-agent-secret-fetch"),
        fde_config_file: paths.guest_staging_dir.join("fde.toml"),
        policy_default: paths.guest_staging_dir.join("default.rego"),
        policy_local_dev: paths.guest_staging_dir.join("local-dev.rego"),
        guest_setup_script: None,
        extra_files: Vec::new(),
    };

    stage_mkosi_debug_ssh_authorized_keys(&mut assets, &paths, Some(&public_key)).unwrap();

    assert_eq!(assets.extra_files.len(), 2);
    assert_eq!(
        fs::read_to_string(&assets.extra_files[0].source).unwrap(),
        "ssh-ed25519 BBBB configured\n"
    );
    assert_eq!(
        assets.extra_files[1].destination,
        "/etc/confidential-agent/debug-ssh-enabled"
    );
}

#[test]
fn debug_ssh_key_generator_writes_openssh_ed25519_pair() {
    let temp = tempfile::tempdir().unwrap();
    let private_key = temp.path().join("debug_ssh");
    let public_key = temp.path().join("debug_ssh.pub");

    generate_debug_ssh_key("openclaw", &private_key, &public_key).unwrap();

    let private = fs::read_to_string(private_key).unwrap();
    let public = fs::read_to_string(public_key).unwrap();
    assert!(private.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----\n"));
    assert!(private.ends_with("-----END OPENSSH PRIVATE KEY-----\n"));
    let parts = public.split_whitespace().collect::<Vec<_>>();
    assert_eq!(parts[0], "ssh-ed25519");
    assert_eq!(parts[2], "confidential-agent:openclaw:debug");
    let public_blob = BASE64_STANDARD.decode(parts[1]).unwrap();
    assert_eq!(&public_blob[0..4], &11u32.to_be_bytes());
    assert_eq!(&public_blob[4..15], b"ssh-ed25519");
    assert_eq!(&public_blob[15..19], &32u32.to_be_bytes());
    assert_eq!(public_blob.len(), 51);
}

#[test]
fn debug_ssh_key_generator_output_is_accepted_by_ssh_keygen_when_available() {
    if Command::new("ssh-keygen").arg("-?").output().is_err() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let private_key = temp.path().join("debug_ssh");
    let public_key = temp.path().join("debug_ssh.pub");
    generate_debug_ssh_key("openclaw", &private_key, &public_key).unwrap();
    set_mode(&private_key, 0o600).unwrap();

    let output = Command::new("ssh-keygen")
        .arg("-y")
        .arg("-f")
        .arg(&private_key)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let derived = String::from_utf8(output.stdout).unwrap();
    let expected = fs::read_to_string(public_key).unwrap();
    let derived_parts = derived.split_whitespace().take(2).collect::<Vec<_>>();
    let expected_parts = expected.split_whitespace().take(2).collect::<Vec<_>>();
    assert_eq!(derived_parts, expected_parts);
}

#[test]
fn debug_ssh_hint_uses_generated_private_key_and_public_ip() {
    let mut state = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    state.build.variant = "debug".to_string();
    state.build.debug_ssh = Some(confidential_agent_core::schema::LocalDebugSshKey {
        private_key: PathBuf::from("/state/services/openclaw/secrets/debug_ssh"),
        public_key: PathBuf::from("/state/services/openclaw/secrets/debug_ssh.pub"),
    });

    let hint = debug_ssh_hint(&state).unwrap();

    assert_eq!(
        hint,
        "openclaw: ssh -i /state/services/openclaw/secrets/debug_ssh root@39.0.0.1"
    );
}

#[test]
fn debug_ssh_command_errors_when_state_missing() {
    let temp = tempfile::tempdir().unwrap();
    let err = resolve_debug_ssh_command(temp.path(), "openclaw", &[]).unwrap_err();

    assert!(err
        .to_string()
        .contains("service 'openclaw' has no local state"));
}

#[test]
fn debug_ssh_command_errors_without_debug_key() {
    let temp = tempfile::tempdir().unwrap();
    let state = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    write_state(temp.path(), &state);

    let err = resolve_debug_ssh_command(temp.path(), "openclaw", &[]).unwrap_err();

    assert!(err.to_string().contains("has no debug SSH key"));
}

#[test]
fn debug_ssh_command_errors_without_ip() {
    let mut state = debug_ssh_state("openclaw");
    state.deploy.public_ip = None;
    state.deploy.private_ip = None;

    let err = build_debug_ssh_command(&state, &[]).unwrap_err();

    assert!(err.to_string().contains("has no public_ip or private_ip"));
}

#[test]
fn debug_ssh_command_appends_extra_args() {
    let state = debug_ssh_state("openclaw");
    let extras = vec![
        OsString::from("-vv"),
        OsString::from("-L"),
        OsString::from("127.0.0.1:8080:127.0.0.1:8080"),
    ];

    let command = build_debug_ssh_command(&state, &extras).unwrap();

    assert_eq!(
        command.argv,
        vec![
            OsString::from("ssh"),
            OsString::from("-i"),
            OsString::from("/state/services/openclaw/secrets/debug_ssh"),
            OsString::from("root@39.0.0.1"),
            OsString::from("-vv"),
            OsString::from("-L"),
            OsString::from("127.0.0.1:8080:127.0.0.1:8080"),
        ]
    );
}

#[test]
fn debug_ssh_command_prefers_public_ip() {
    let mut state = debug_ssh_state("openclaw");
    state.deploy.public_ip = Some("39.0.0.1".to_string());
    state.deploy.private_ip = Some("10.0.1.20".to_string());

    let command = build_debug_ssh_command(&state, &[]).unwrap();

    assert_eq!(command.target, "39.0.0.1");
    assert_eq!(
        command.argv,
        vec![
            OsString::from("ssh"),
            OsString::from("-i"),
            OsString::from("/state/services/openclaw/secrets/debug_ssh"),
            OsString::from("root@39.0.0.1"),
        ]
    );
}

#[test]
fn debug_ssh_command_falls_back_to_private_ip() {
    let mut state = debug_ssh_state("openclaw");
    state.deploy.public_ip = None;
    state.deploy.private_ip = Some("10.0.1.20".to_string());

    let command = build_debug_ssh_command(&state, &[]).unwrap();

    assert_eq!(command.target, "10.0.1.20");
    assert_eq!(
        command.argv,
        vec![
            OsString::from("ssh"),
            OsString::from("-i"),
            OsString::from("/state/services/openclaw/secrets/debug_ssh"),
            OsString::from("root@10.0.1.20"),
        ]
    );
}

#[test]
fn debug_ssh_command_treats_empty_public_ip_as_absent() {
    let mut state = debug_ssh_state("openclaw");
    state.deploy.public_ip = Some("   ".to_string());
    state.deploy.private_ip = Some("10.0.1.20".to_string());

    let command = build_debug_ssh_command(&state, &[]).unwrap();

    assert_eq!(command.target, "10.0.1.20");
}

fn debug_ssh_state(service_id: &str) -> LocalServiceState {
    let mut state = local_state(service_id, vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    state.build.variant = "debug".to_string();
    state.build.debug_ssh = Some(LocalDebugSshKey {
        private_key: PathBuf::from(format!("/state/services/{service_id}/secrets/debug_ssh")),
        public_key: PathBuf::from(format!(
            "/state/services/{service_id}/secrets/debug_ssh.pub"
        )),
    });
    state
}

#[test]
fn deploy_resource_name_keeps_shelter_default_bucket_valid() {
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: this-service-name-is-intentionally-very-long-to-exercise-oss-bucket-limits
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
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
"#,
        Path::new("/project"),
    )
    .unwrap();

    let names = DeployNames::from_run_id(&spec, "20260429103011");
    let bucket = format!("{}{}", names.resource_name, SHELTER_IMAGE_BUCKET_SUFFIX);

    assert!(bucket.len() <= MAX_SHELTER_IMAGE_BUCKET_LEN);
    assert!(bucket.ends_with("-images"));
    assert!(names.resource_name.ends_with("-20260429103011"));
    assert!(!names.resource_name.ends_with('-'));
}

#[test]
fn destroy_leaves_local_state_active_when_shelter_destroy_fails() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    service.deploy.terraform_dir = Some(temp.path().join("services/openclaw/terraform/active"));
    write_state(temp.path(), &service);
    write_manifest(temp.path(), "openclaw", &service.build.build_id);
    let service_dir = temp.path().join("services/openclaw");
    fs::write(
        service_dir.join("shelter.yaml"),
        "deploy:\n  name: openclaw\n",
    )
    .unwrap();
    let shelter = temp.path().join("shelter-fail");
    let argv = temp.path().join("shelter-argv");
    write_script(
        &shelter,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nexit 42\n",
            argv.display()
        ),
    );
    let mut cli = test_cli();
    cli.state_dir = temp.path().to_path_buf();
    cli.shelter_bin = shelter;

    let err = cmd_destroy(
        &cli,
        &DestroyArgs {
            service: "openclaw".to_string(),
        },
    )
    .unwrap_err();

    assert!(err.to_string().contains("shelter exited with status"));
    let recorded = fs::read_to_string(argv).unwrap();
    assert!(recorded.contains("--work-dir\n"));
    assert!(recorded.contains("destroy\nopenclaw-agent-release\n"));
    assert!(recorded.contains("--terraform-dir\n"));
    assert!(recorded.contains("services/openclaw/terraform/active\n"));
    let state = read_service_state_file(&service_dir.join("state.json"))
        .unwrap()
        .unwrap();
    assert_eq!(state.phase, "active");
}

#[test]
fn destroy_keeps_local_build_artifacts_after_shelter_destroy_succeeds() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    let service_dir = temp.path().join("services/openclaw");
    service.deploy.terraform_dir = Some(service_dir.join("terraform/active"));
    service.deploy.instance_id = Some("i-test".to_string());
    service.deploy.security_group_id = Some("sg-test".to_string());
    service.build.image_path =
        service_dir.join("shelter/images/openclaw-agent-release/image.qcow2");
    service.build.sample_rv = Some(service_dir.join("shelter-reference-values.json"));
    service.build.rekor_meta = Some(service_dir.join("shelter-rekor-meta.json"));
    service.build.debug_ssh = Some(LocalDebugSshKey {
        private_key: service_dir.join("debug/id_ed25519"),
        public_key: service_dir.join("debug/id_ed25519.pub"),
    });
    write_state(temp.path(), &service);
    write_manifest(temp.path(), "openclaw", &service.build.build_id);
    fs::write(
        service_dir.join("shelter.yaml"),
        "deploy:\n  name: openclaw\n",
    )
    .unwrap();
    fs::create_dir_all(service.build.image_path.parent().unwrap()).unwrap();
    fs::write(&service.build.image_path, "image").unwrap();
    let build_result =
        shelter_build_result_path(&service_dir.join("shelter"), &service.build.build_id);
    fs::create_dir_all(build_result.parent().unwrap()).unwrap();
    fs::write(
        &build_result,
        serde_json::to_string_pretty(&serde_json::json!({
            "id": service.build.build_id,
            "image_path": service.build.image_path,
            "reference_value": {"sample": true},
            "rekor_value": {"rekor": true}
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(service.build.sample_rv.as_ref().unwrap(), "{}").unwrap();
    fs::write(service.build.rekor_meta.as_ref().unwrap(), "{}").unwrap();
    fs::create_dir_all(
        service
            .build
            .debug_ssh
            .as_ref()
            .unwrap()
            .private_key
            .parent()
            .unwrap(),
    )
    .unwrap();
    fs::write(
        &service.build.debug_ssh.as_ref().unwrap().private_key,
        "private",
    )
    .unwrap();
    fs::write(
        &service.build.debug_ssh.as_ref().unwrap().public_key,
        "public",
    )
    .unwrap();
    let shelter = temp.path().join("shelter-ok");
    let argv = temp.path().join("shelter-argv");
    write_script(
        &shelter,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nexit 0\n",
            argv.display()
        ),
    );
    let mut cli = test_cli();
    cli.state_dir = temp.path().to_path_buf();
    cli.shelter_bin = shelter;

    cmd_destroy(
        &cli,
        &DestroyArgs {
            service: "openclaw".to_string(),
        },
    )
    .unwrap();

    assert!(service_dir.exists());
    assert!(service.build.image_path.exists());
    assert!(build_result.exists());
    assert!(service.build.sample_rv.as_ref().unwrap().exists());
    assert!(service.build.rekor_meta.as_ref().unwrap().exists());
    assert!(service
        .build
        .debug_ssh
        .as_ref()
        .unwrap()
        .private_key
        .exists());
    let state = read_service_state_file(&service_dir.join("state.json"))
        .unwrap()
        .unwrap();
    assert_eq!(state.phase, "deleted");
    assert_eq!(state.generation, 2);
    assert_eq!(state.deploy.public_ip, None);
    assert_eq!(state.deploy.private_ip, None);
    assert_eq!(state.deploy.instance_id, None);
    assert_eq!(state.deploy.security_group_id, None);
    assert_eq!(state.deploy.terraform_dir, None);
    assert_eq!(state.deploy.run_id, "");
    assert_eq!(state.deploy.resource_name, "");
    assert_eq!(state.deploy.image_source, None);
    assert_eq!(state.deploy.image_import_name, None);
    assert_eq!(state.deploy.bucket, None);
    assert_eq!(state.deploy.tee, "");
}

#[test]
fn destroy_is_idempotent_for_deleted_service_without_cloud_state() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    service.phase = "deleted".to_string();
    service.deploy.run_id.clear();
    service.deploy.resource_name.clear();
    service.deploy.terraform_dir = None;
    service.deploy.image_source = None;
    service.deploy.image_import_name = None;
    service.deploy.bucket = None;
    service.deploy.instance_id = None;
    service.deploy.security_group_id = None;
    service.deploy.private_ip = None;
    service.deploy.public_ip = None;
    service.deploy.tee.clear();
    write_state(temp.path(), &service);
    let shelter = temp.path().join("shelter-should-not-run");
    write_script(&shelter, "#!/bin/sh\nexit 42\n");
    let mut cli = test_cli();
    cli.state_dir = temp.path().to_path_buf();
    cli.shelter_bin = shelter;

    cmd_destroy(
        &cli,
        &DestroyArgs {
            service: "openclaw".to_string(),
        },
    )
    .unwrap();
}

#[test]
fn live_status_skips_deleted_services() {
    let mut service = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    service.phase = "deleted".to_string();
    service.deploy.public_ip = Some("127.0.0.1".to_string());

    let temp = tempfile::tempdir().unwrap();
    let live = collect_live_status(temp.path(), &[service]);

    assert!(live[0].daemon.is_none());
    assert_eq!(
        live[0].live_error.as_deref(),
        Some("service is not active or deployed")
    );
}

#[test]
fn guest_reference_value_source_resolves_mode_specific_files() {
    let temp = tempfile::tempdir().unwrap();
    let paths = context_paths(temp.path(), "svc");
    fs::create_dir_all(&paths.service_dir).unwrap();

    assert!(guest_reference_value_source(&paths, None, "rekor").is_none());
    let rekor = paths.service_dir.join("rekor-rv-list.json");
    fs::write(&rekor, r#"{"rv_list":[]}"#).unwrap();
    assert_eq!(
        guest_reference_value_source(&paths, None, "rekor"),
        Some(rekor)
    );

    let sample = paths.service_dir.join("shelter-reference-values.json");
    assert!(guest_reference_value_source(&paths, Some(&sample), "sample").is_none());
    fs::write(&sample, r#"{"measurement.uki.SHA-384":["abc"]}"#).unwrap();
    assert_eq!(
        guest_reference_value_source(&paths, Some(&sample), "sample"),
        Some(sample)
    );
}

#[test]
fn inject_requires_existing_managed_state() {
    let temp = tempfile::tempdir().unwrap();
    let spec = temp.path().join("confidential-agent.yaml");
    fs::write(
        &spec,
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
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
"#,
    )
    .unwrap();
    let mut cli = test_cli();
    cli.state_dir = temp.path().join("state");

    let err = cmd_inject(
        &cli,
        &InjectArgs {
            spec,
            target_ip: "127.0.0.1".to_string(),
            skip_peering_check: true,
        },
    )
    .unwrap_err();

    assert!(err.to_string().contains("run deploy first"));
}

#[test]
fn deploy_requires_existing_local_build_before_prepare() {
    let temp = tempfile::tempdir().unwrap();
    let spec = temp.path().join("openclaw.yaml");
    fs::write(
        &spec,
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
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
"#,
    )
    .unwrap();
    let mut cli = test_cli();
    cli.state_dir = temp.path().join("state");

    let err = cmd_deploy(
        &cli,
        &DeployArgs {
            spec,
            skip_inject: false,
            render_only: false,
            skip_peering_check: true,
        },
    )
    .unwrap_err();

    assert!(err.to_string().contains("run build first"));
}

#[test]
fn deploy_rejects_missing_current_local_image_before_shelter() {
    let temp = tempfile::tempdir().unwrap();
    let spec = temp.path().join("openclaw.yaml");
    fs::write(
        &spec,
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
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
"#,
    )
    .unwrap();
    let mut state = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    state.phase = "built".to_string();
    state.build.image_path = temp.path().join("missing.qcow2");
    write_state(temp.path(), &state);
    write_manifest(temp.path(), "openclaw", &state.build.build_id);
    let build_result = shelter_build_result_path(
        &temp.path().join("services/openclaw/shelter"),
        &state.build.build_id,
    );
    fs::create_dir_all(build_result.parent().unwrap()).unwrap();
    fs::write(
        &build_result,
        serde_json::to_string_pretty(&serde_json::json!({
            "id": state.build.build_id,
            "image_path": state.build.image_path,
            "reference_value": null,
            "rekor_value": null
        }))
        .unwrap(),
    )
    .unwrap();
    let mut cli = test_cli();
    cli.state_dir = temp.path().to_path_buf();

    let err = cmd_deploy(
        &cli,
        &DeployArgs {
            spec,
            skip_inject: false,
            render_only: false,
            skip_peering_check: true,
        },
    )
    .unwrap_err();

    assert!(err.to_string().contains("local image"));
    assert!(err.to_string().contains("run build first"));
}

#[test]
fn deploy_render_only_uses_published_image_without_local_qcow2() {
    let temp = tempfile::tempdir().unwrap();
    let spec = temp.path().join("openclaw.yaml");
    fs::write(
        &spec,
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
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
"#,
    )
    .unwrap();
    let mut state = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    state.phase = "built".to_string();
    state.spec.path = spec.clone();
    state.build.image_path = temp.path().join("missing.qcow2");
    let source_sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    state.build.published.insert(
        "aliyun/cn-beijing/release/build/sha".to_string(),
        test_published_image(&state.build.build_id, source_sha256),
    );
    write_state(temp.path(), &state);
    write_manifest(temp.path(), "openclaw", &state.build.build_id);
    let build_result = shelter_build_result_path(
        &temp.path().join("services/openclaw/shelter"),
        &state.build.build_id,
    );
    write_build_result(
        &build_result,
        &state.build.build_id,
        &state.build.image_path,
    );
    let mut cli = test_cli();
    cli.state_dir = temp.path().to_path_buf();

    cmd_deploy(
        &cli,
        &DeployArgs {
            spec,
            skip_inject: false,
            render_only: true,
            skip_peering_check: true,
        },
    )
    .unwrap();

    let rendered =
        fs::read_to_string(context_paths(temp.path(), "openclaw").rendered_config).unwrap();
    let deploy_section = rendered.split("deploy:").nth(1).unwrap();
    assert!(deploy_section.contains("image_id: m-2ze123abc"));
    assert!(!deploy_section.contains("image:\n"));
    assert!(!deploy_section.contains("nvme_support:"));
}

#[test]
fn published_image_requires_build_result_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let mut state = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    state.phase = "built".to_string();
    state.build.image_path = temp.path().join("missing.qcow2");
    state.build.published.insert(
        "aliyun/cn-beijing/release/build/sha".to_string(),
        test_published_image(
            &state.build.build_id,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ),
    );
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  image_name: openclaw-agent
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
"#,
        Path::new("/project"),
    )
    .unwrap();
    let variant = BuildManifestVariant {
        shelter_build_id: state.build.build_id.clone(),
        build_result: temp.path().join("missing-build-result.json"),
        extra_files: Vec::new(),
        debug_ssh: None,
    };

    let image_id = publish::published_image_for_deploy(&state, &spec, &variant);

    assert!(image_id.is_none());
}

#[test]
fn published_image_for_deploy_skips_stale_entry_and_uses_valid_one() {
    let temp = tempfile::tempdir().unwrap();
    let mut state = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    state.phase = "built".to_string();
    state.build.image_path = temp.path().join("image.qcow2");
    fs::write(&state.build.image_path, "image").unwrap();
    let source_sha256 = sha256_file(&state.build.image_path).unwrap();
    let build_result = temp.path().join("build-result.json");
    write_build_result(
        &build_result,
        &state.build.build_id,
        &state.build.image_path,
    );
    state.build.published.insert(
        "aliyun/cn-beijing/release/build/000-stale".to_string(),
        test_published_image(
            &state.build.build_id,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ),
    );
    let mut valid = test_published_image(&state.build.build_id, &source_sha256);
    valid.image_id = Some("m-valid".to_string());
    state.build.published.insert(
        "aliyun/cn-beijing/release/build/999-valid".to_string(),
        valid,
    );
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  image_name: openclaw-agent
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
"#,
        Path::new("/project"),
    )
    .unwrap();
    let variant = BuildManifestVariant {
        shelter_build_id: state.build.build_id.clone(),
        build_result,
        extra_files: Vec::new(),
        debug_ssh: None,
    };

    let image_id = publish::published_image_for_deploy(&state, &spec, &variant);

    assert_eq!(image_id.as_deref(), Some("m-valid"));
}

#[test]
fn activate_existing_service_state_accepts_deployed_phase() {
    let temp = tempfile::tempdir().unwrap();
    let spec_path = temp.path().join("confidential-agent.yaml");
    fs::write(
        &spec_path,
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789, 18800]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
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
"#,
    )
    .unwrap();
    let spec = AgentSpec::from_path(&spec_path).unwrap();
    let mut state = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    state.phase = "deployed".to_string();
    state.deploy.run_id = "20260429110011".to_string();
    state.deploy.resource_name = "openclaw-20260429110011".to_string();

    let activated = activate_existing_service_state(temp.path(), &spec_path, &spec, state).unwrap();

    assert_eq!(activated.phase, "active");
    assert_eq!(activated.generation, 2);
    assert_eq!(activated.deploy.run_id, "20260429110011");
    assert_eq!(activated.deploy.resource_name, "openclaw-20260429110011");
    assert_eq!(activated.deploy.public_ip.as_deref(), Some("39.0.0.1"));
    assert_eq!(activated.service.ports, vec![18789, 18800]);
    assert!(activated.gateway_identity.is_some());
}

#[test]
fn activate_existing_service_state_generates_missing_gateway_identity() {
    let temp = tempfile::tempdir().unwrap();
    let spec_path = temp.path().join("confidential-agent.yaml");
    fs::write(
        &spec_path,
        r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
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
"#,
    )
    .unwrap();
    let spec = AgentSpec::from_path(&spec_path).unwrap();
    let mut state = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    state.gateway_identity = None;

    let activated = activate_existing_service_state(temp.path(), &spec_path, &spec, state).unwrap();

    let identity = activated.gateway_identity.expect("gateway identity");
    assert!(!identity.public_key.is_empty());
    assert!(identity.private_key_path.exists());
}

#[test]
fn ensure_gateway_identity_rejects_mismatched_public_key() {
    let temp = tempfile::tempdir().unwrap();
    let paths = context_paths(temp.path(), "openclaw");
    fs::create_dir_all(&paths.secrets_dir).unwrap();
    let private_key = URL_SAFE_NO_PAD.encode([7u8; 32]);
    let wrong_public_key = URL_SAFE_NO_PAD.encode([9u8; 32]);
    fs::write(paths.secrets_dir.join("gateway_identity.seed"), private_key).unwrap();
    fs::write(
        paths.secrets_dir.join("gateway_identity.pub"),
        wrong_public_key,
    )
    .unwrap();

    let err = ensure_gateway_identity(&paths).unwrap_err();

    assert!(err
        .to_string()
        .contains("gateway public key does not match private key seed"));
}

#[test]
fn mesh_port_conflict_is_rejected() {
    let states = vec![local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))])];
    let err = validate_mesh_port_conflicts(&states, "mcp", &[18789]).unwrap_err();
    assert!(err
        .to_string()
        .contains("port 18789 is already used by service openclaw"));
}

#[test]
fn built_services_do_not_reserve_mesh_ports() {
    let mut built = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    built.phase = "built".to_string();

    validate_mesh_port_conflicts(&[built], "mcp", &[18789]).unwrap();
}

#[test]
fn filtered_mesh_sync_requires_active_local_state() {
    let cli = test_cli();
    let temp = tempfile::tempdir().unwrap();
    let mut service = local_state("mcp", vec![3001], Vec::new());
    service.phase = "deployed".to_string();

    let err = sync_mesh_for_services(&cli, temp.path(), vec![service], Some("mcp")).unwrap_err();

    assert!(err
        .to_string()
        .contains("service 'mcp' is not active in local state"));
}

#[test]
fn render_mesh_bundle_includes_service_ports_and_sample_reference_values() {
    let services = vec![local_state("cai-e2e", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))])];
    let mut sample = BTreeMap::new();
    sample.insert(
        "cai-e2e".to_string(),
        serde_json::json!({
            "measurement.uki.SHA-384": ["abc123"]
        }),
    );
    let reference_values = ReferenceValueArtifacts {
        sample,
        rekor: BTreeMap::new(),
    };

    let bundle = render_mesh_bundle(&services, &reference_values, 7);

    assert_eq!(bundle.generation, 7);
    assert_eq!(bundle.services["cai-e2e"].ports, vec![18789]);
    assert_eq!(bundle.services["cai-e2e"].connect, vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    assert_eq!(
        bundle.reference_values["cai-e2e"]["measurement.uki.SHA-384"][0],
        "abc123"
    );
}

#[test]
fn render_mesh_bundle_omits_deleted_services() {
    let mut openclaw = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    let mut mcp = local_state("mcp", vec![3001], Vec::new());
    mcp.phase = "deleted".to_string();
    let mut sample = BTreeMap::new();
    sample.insert(
        "openclaw".to_string(),
        serde_json::json!({"measurement.uki.SHA-384": ["abc123"]}),
    );
    let reference_values = ReferenceValueArtifacts {
        sample,
        rekor: BTreeMap::new(),
    };

    let bundle = render_mesh_bundle(&[openclaw.clone(), mcp], &reference_values, 7);

    assert!(bundle.services.contains_key("openclaw"));
    assert!(!bundle.services.contains_key("mcp"));
    openclaw.phase = "deleted".to_string();
    let bundle = render_mesh_bundle(&[openclaw], &reference_values, 8);
    assert!(bundle.services.is_empty());
    assert!(bundle.reference_values.is_empty());
}

#[test]
fn connect_requires_public_ip() {
    let temp = tempfile::tempdir().unwrap();
    let mut service = local_state("openclaw", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    service.deploy.public_ip = None;
    write_state(temp.path(), &service);

    let mut sample = BTreeMap::new();
    sample.insert(
        "openclaw".to_string(),
        serde_json::json!({"measurement.uki.SHA-384": ["abc123"]}),
    );
    write_mesh_bundle(temp.path(), vec![service], sample);

    let err = render_connect_config(temp.path(), None).unwrap_err();

    assert!(err
        .to_string()
        .contains("service 'openclaw' has no public_ip"));
}

#[test]
fn connect_ignores_services_without_connect_ports() {
    let temp = tempfile::tempdir().unwrap();
    let service = local_state("mcp", vec![3001], Vec::new());
    write_state(temp.path(), &service);

    let err = render_connect_config(temp.path(), None).unwrap_err();

    assert!(err
        .to_string()
        .contains("no active services expose host connect ports"));
}

#[test]
fn connect_renders_all_connect_services() {
    let temp = tempfile::tempdir().unwrap();
    let openclaw = local_state("openclaw", vec![49152], vec![(confidential_agent_core::spec::ConnectPort::Plain(49152))]);
    let worker = local_state("worker", vec![49153], vec![(confidential_agent_core::spec::ConnectPort::Plain(49153))]);
    let mcp = local_state("mcp", vec![3001], Vec::new());
    write_state(temp.path(), &openclaw);
    write_state(temp.path(), &worker);
    write_state(temp.path(), &mcp);

    let mut sample = BTreeMap::new();
    sample.insert(
        "openclaw".to_string(),
        serde_json::json!({"measurement.uki.SHA-384": ["openclaw-rv"]}),
    );
    sample.insert(
        "worker".to_string(),
        serde_json::json!({"measurement.uki.SHA-384": ["worker-rv"]}),
    );
    write_mesh_bundle(temp.path(), vec![openclaw, worker, mcp], sample);

    let config = render_connect_config(temp.path(), None).unwrap();

    assert_eq!(config["add_ingress"].as_array().unwrap().len(), 2);
    assert_eq!(config["add_ingress"][0]["mapping"]["out"]["port"], 49152);
    assert_eq!(config["add_ingress"][1]["mapping"]["out"]["port"], 49153);
    assert_eq!(config["add_ingress"][0]["rats_tls"]["multiplex"], false);
    assert_eq!(
        config["add_ingress"][0]["verify"]["attestation_policy"]["type"],
        "path"
    );
    assert!(config["add_ingress"][0]["verify"].get("policy").is_none());
    assert!(config["add_ingress"][0]["verify"]
        .get("policy_ids")
        .is_none());
    assert_eq!(config["client_endpoints"].as_array().unwrap().len(), 2);
    assert_eq!(config["client_endpoints"][0]["service"], "openclaw");
    assert_eq!(config["client_endpoints"][0]["guest_port"], 49152);
    assert_eq!(config["client_endpoints"][0]["local_host"], "127.0.0.1");
    let local_port = config["client_endpoints"][0]["local_port"]
        .as_u64()
        .unwrap();
    assert!(local_port >= 49152);
    assert_eq!(
        config["client_endpoints"][0]["http_base_url"],
        format!("http://127.0.0.1:{local_port}")
    );
}

#[test]
fn connect_service_filter_renders_only_requested_service() {
    let temp = tempfile::tempdir().unwrap();
    let openclaw = local_state("openclaw", vec![49152], vec![(confidential_agent_core::spec::ConnectPort::Plain(49152))]);
    let worker = local_state("worker", vec![49153], vec![(confidential_agent_core::spec::ConnectPort::Plain(49153))]);
    write_state(temp.path(), &openclaw);
    write_state(temp.path(), &worker);

    let mut sample = BTreeMap::new();
    sample.insert(
        "openclaw".to_string(),
        serde_json::json!({"measurement.uki.SHA-384": ["openclaw-rv"]}),
    );
    sample.insert(
        "worker".to_string(),
        serde_json::json!({"measurement.uki.SHA-384": ["worker-rv"]}),
    );
    write_mesh_bundle(temp.path(), vec![openclaw, worker], sample);

    let config = render_connect_config(temp.path(), Some("worker")).unwrap();

    assert_eq!(config["add_ingress"].as_array().unwrap().len(), 1);
    assert_eq!(config["add_ingress"][0]["mapping"]["out"]["port"], 49153);
    assert_eq!(config["client_endpoints"].as_array().unwrap().len(), 1);
    assert_eq!(config["client_endpoints"][0]["service"], "worker");
}

#[test]
fn connect_service_filter_rejects_unknown_service() {
    let temp = tempfile::tempdir().unwrap();
    let openclaw = local_state("openclaw", vec![49152], vec![(confidential_agent_core::spec::ConnectPort::Plain(49152))]);
    write_state(temp.path(), &openclaw);
    write_mesh_bundle(
        temp.path(),
        vec![openclaw],
        BTreeMap::from([(
            "openclaw".to_string(),
            serde_json::json!({"measurement.uki.SHA-384": ["openclaw-rv"]}),
        )]),
    );

    let err = render_connect_config(temp.path(), Some("missing")).unwrap_err();

    assert!(err
        .to_string()
        .contains("no local state for service 'missing'"));
}

#[test]
fn connect_reference_values_prefers_sample_when_rekor_is_also_present() {
    let bundle = MeshBundle {
        schema: MESH_SCHEMA_VERSION.to_string(),
        generation: 1,
        updated_at: 0,
        reference_values: BTreeMap::from([(
            "openclaw".to_string(),
            serde_json::json!({"measurement.uki.SHA-384": ["sample-rv"]}),
        )]),
        rekor_reference_values: BTreeMap::from([(
            "openclaw".to_string(),
            serde_json::json!({
                "artifact_id": "openclaw-disk",
                "artifact_version": "20260514000000",
                "artifact_type": "uki",
                "rekor_url": "https://rekor.sigstore.dev",
                "rv_name": "measurement.uki.SHA-384"
            }),
        )]),
        services: BTreeMap::new(),
    };

    let values = connect_reference_values(&bundle, "openclaw").unwrap();

    assert_eq!(values[0]["type"], "sample");
    assert_eq!(
        values[0]["payload"]["content"]["measurement.uki.SHA-384"][0],
        "sample-rv"
    );
}

fn test_agent_card(ports: Vec<u16>) -> AgentCard {
    let confidential = AgentCardConfidential {
        id: "peer".to_string(),
        cache_ttl_sec: 300,
        public_ip: "203.0.113.8".to_string(),
        ports: ports
            .iter()
            .copied()
            .map(|port| AgentCardPort {
                name: format!("port-{port}"),
                port,
            })
            .collect(),
        reference_values: None,
        rekor: AgentCardRekor {
            rekor_url: "https://rekor.sigstore.dev".to_string(),
            artifact_id: "peer-disk".to_string(),
            artifact_type: "uki".to_string(),
            artifact_version: "20260514000000".to_string(),
            rv_name: "measurement.uki.SHA-384".to_string(),
        },
        tee: "tdx".to_string(),
    };
    AgentCard {
        protocol_version: "1.0".to_string(),
        name: "peer-openclaw".to_string(),
        description: "peer openclaw".to_string(),
        version: Some("1.0.0".to_string()),
        supported_interfaces: ports
            .iter()
            .map(|port| AgentInterface {
                url: format!("http://203.0.113.8:{port}/a2a"),
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
                uri: confidential_agent_core::agent_card::CONFIDENTIAL_AGENT_EXTENSION.to_string(),
                description: None,
                required: true,
                params: serde_json::to_value(confidential).unwrap(),
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

#[test]
fn from_card_connect_allocates_available_local_ports() {
    let card = test_agent_card(vec![18789, 18790]);

    let config =
        render_agent_card_connect_config_with_port_checker(Path::new("/state"), &card, |port| {
            port == 18789 || port == 50000
        })
        .unwrap();

    assert_eq!(config["control_interface"]["restful"]["port"], 50001);
    assert_eq!(config["add_ingress"].as_array().unwrap().len(), 2);
    assert_eq!(config["add_ingress"][0]["mapping"]["in"]["port"], 18790);
    assert_eq!(config["add_ingress"][0]["mapping"]["out"]["port"], 18789);
    assert_eq!(config["add_ingress"][0]["rats_tls"]["multiplex"], false);
    assert_eq!(
        config["add_ingress"][0]["verify"]["attestation_policy"]["type"],
        "path"
    );
    assert!(config["add_ingress"][0]["verify"].get("policy").is_none());
    assert!(config["add_ingress"][0]["verify"]
        .get("policy_ids")
        .is_none());
    assert_eq!(config["add_ingress"][1]["mapping"]["in"]["port"], 18791);
    assert_eq!(config["add_ingress"][1]["mapping"]["out"]["port"], 18790);
}

#[test]
fn a2a_add_rejects_unknown_scoped_service() {
    let temp = tempfile::tempdir().unwrap();
    let mut cli = test_cli();
    cli.state_dir = temp.path().to_path_buf();

    let err = cmd_a2a(
        &cli,
        &A2aArgs {
            command: A2aCommands::Add {
                agent_card_url: "http://127.0.0.1:8089/.well-known/agent-card.json".to_string(),
                alias: Some("beta".to_string()),
                service: vec!["missing".to_string()],
                signer_issuer: None,
                signer_subject: None,
            },
        },
    )
    .unwrap_err();

    assert!(err
        .to_string()
        .contains("a2a scoped service 'missing' does not exist locally"));
}

#[test]
fn a2a_preview_error_kind_keeps_trust_failures_distinct() {
    assert_eq!(
        a2a_cli_preview_error_kind(&AgentCardFetchError::SignatureMissing),
        "unsigned"
    );
    assert_eq!(
        a2a_cli_preview_error_kind(&AgentCardFetchError::SignatureVerification(
            "identity mismatch".to_string()
        )),
        "signature_failed"
    );
    assert_eq!(
        a2a_cli_preview_error_kind(&AgentCardFetchError::PublicIpHostMismatch {
            declared: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)),
            resolved: vec![IpAddr::V4(Ipv4Addr::new(203, 0, 113, 11))],
        }),
        "host_mismatch"
    );
    assert_eq!(
        a2a_cli_preview_error_kind(&AgentCardFetchError::HostResolution {
            host: "peer.example".to_string(),
            message: "temporary DNS failure".to_string(),
        }),
        "host_mismatch"
    );
    assert_eq!(
        a2a_cli_preview_error_kind(&AgentCardFetchError::RekorUrlNotTrusted {
            url: "https://rekor.example".to_string(),
            allowed: vec!["https://rekor.sigstore.dev".to_string()],
        }),
        "rekor_untrusted"
    );
    assert_eq!(
        a2a_cli_preview_error_kind(&AgentCardFetchError::LegacyConfidentialAgentCard),
        "invalid"
    );
}

#[test]
fn collect_sample_reference_values_uses_state_reference_value_path() {
    let temp = tempfile::tempdir().unwrap();
    let sample = temp.path().join("shelter-reference-values.json");
    fs::write(
        &sample,
        r#"{"measurement.uki.SHA-384":["from-build-result"]}"#,
    )
    .unwrap();
    let mut service = local_state("cai-e2e", vec![18789], vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))]);
    service.build.sample_rv = Some(sample);
    let services = vec![service];
    let values = collect_reference_values_from_dir(temp.path(), &services).unwrap();

    assert_eq!(
        values.sample["cai-e2e"]["measurement.uki.SHA-384"][0],
        "from-build-result"
    );
}

#[test]
fn no_proxy_preserves_existing_entries_and_adds_target_host() {
    let value = no_proxy_with_target("localhost,127.0.0.1", "39.105.64.44");

    assert_eq!(value, "localhost,127.0.0.1,39.105.64.44");
}

#[test]
fn no_proxy_does_not_duplicate_target_host() {
    let value = no_proxy_with_target("localhost,39.105.64.44", "39.105.64.44");

    assert_eq!(value, "localhost,39.105.64.44");
}

#[test]
fn direct_challenge_envs_do_not_forward_proxies() {
    let envs = challenge_inject_envs(
        true,
        "39.105.64.44",
        [
            ("HTTP_PROXY", "http://proxy.example:8080"),
            ("HTTPS_PROXY", "http://proxy.example:8080"),
            ("NO_PROXY", "localhost,127.0.0.1"),
        ],
    );

    assert!(!envs.iter().any(|(key, _)| matches!(
        key.as_str(),
        "http_proxy" | "https_proxy" | "all_proxy" | "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY"
    )));
    assert_eq!(
        envs,
        vec![(
            "NO_PROXY".to_string(),
            "localhost,127.0.0.1,39.105.64.44".to_string()
        )]
    );
}

#[test]
fn operator_peering_control_scope_rejects_direct_egress_mismatch() {
    let peerings = PeeringsFile {
        version: 1,
        peerings: vec![PeeringEntry {
            label: "ops".to_string(),
            role: PeeringRole::Operator,
            cidr: "34.84.30.0/24".to_string(),
            scope: vec![PeeringScope::Control, PeeringScope::Status],
            note: None,
            added_at: None,
            added_by: None,
        }],
    };

    assert!(!peerings
        .control_cidrs_contain("59.82.126.85".parse().unwrap())
        .unwrap());
}

#[test]
fn operator_peering_control_scope_accepts_direct_egress_match() {
    let peerings = PeeringsFile {
        version: 1,
        peerings: vec![PeeringEntry {
            label: "ops".to_string(),
            role: PeeringRole::Operator,
            cidr: "59.82.126.0/24".to_string(),
            scope: vec![PeeringScope::Control, PeeringScope::Status],
            note: None,
            added_at: None,
            added_by: None,
        }],
    };

    assert!(peerings
        .control_cidrs_contain("59.82.126.85".parse().unwrap())
        .unwrap());
}

#[test]
fn proxied_challenge_envs_allow_proxy_for_guest() {
    let envs = challenge_inject_envs(
        false,
        "39.105.64.44",
        [
            ("HTTP_PROXY", "http://proxy.example:8080"),
            ("HTTPS_PROXY", "http://proxy.example:8080"),
            ("NO_PROXY", "localhost,127.0.0.1"),
        ],
    );

    assert!(envs.contains(&(
        "HTTP_PROXY".to_string(),
        "http://proxy.example:8080".to_string()
    )));
    assert!(envs.contains(&("NO_PROXY".to_string(), "localhost,127.0.0.1".to_string())));
    assert!(!envs.iter().any(|(key, value)| {
        (key == "NO_PROXY" || key == "no_proxy") && value.contains("39.105.64.44")
    }));
}

#[test]
fn challenge_inject_args_are_wrapped_with_timeout() {
    let args = challenge_inject_tool_args(
        vec![
            OsString::from("inject-resource"),
            OsString::from("--api-url"),
            OsString::from("http://39.105.64.44:8006"),
        ],
        90,
    );

    assert_eq!(
        args,
        vec![
            OsString::from("90s"),
            OsString::from("attestation-challenge-client"),
            OsString::from("inject-resource"),
            OsString::from("--api-url"),
            OsString::from("http://39.105.64.44:8006"),
        ]
    );
}

#[test]
fn local_port_allocator_skips_occupied_ports() {
    assert_eq!(allocate_local_port(18789, |_| false).unwrap(), 18789);
    assert_eq!(
        allocate_local_port(18789, |port| port == 18789).unwrap(),
        18790
    );
}

#[test]
fn local_port_allocator_bails_when_no_port_above_preferred_is_free() {
    let err = allocate_local_port(u16::MAX, |_| true).unwrap_err();
    assert!(err
        .to_string()
        .contains("no available local port at or above 65535"));
}

#[test]
fn local_port_allocator_skips_to_first_free_port_above_preferred() {
    // Block a contiguous range [18789, 18793] and confirm the allocator
    // walks forward to the next free slot rather than returning early.
    let occupied = [18789, 18790, 18791, 18792, 18793];
    let port = allocate_local_port(18789, |port| occupied.contains(&port)).unwrap();
    assert_eq!(port, 18794);
}

#[test]
fn mounts_for_file_returns_absolute_parent_verbatim() {
    let mounts = mounts_for_file(Path::new("/tmp/resource"), Path::new("/work"));
    assert_eq!(mounts, vec![PathBuf::from("/tmp")]);
}

#[test]
fn mounts_for_file_joins_relative_parent_with_workdir() {
    let mounts = mounts_for_file(Path::new("project/file"), Path::new("/state"));
    assert_eq!(mounts, vec![PathBuf::from("/state/project")]);
}

#[test]
fn mounts_for_file_returns_empty_when_path_has_no_parent() {
    // Bare filename has parent ""; no host mount should be requested.
    let mounts = mounts_for_file(Path::new("file"), Path::new("/state"));
    assert!(mounts.is_empty());
}

#[test]
fn inherited_proxy_envs_only_pulls_recognised_proxy_keys() {
    let envs = inherited_proxy_envs_from(
        [
            ("HTTP_PROXY", "http://proxy.example:8080"),
            ("https_proxy", "http://lower.example:8080"),
            ("UNRELATED", "ignored"),
            ("NO_PROXY", "localhost"),
        ],
        None,
    );

    let keys: Vec<&str> = envs.iter().map(|(key, _)| key.as_str()).collect();
    assert!(keys.contains(&"HTTP_PROXY"));
    assert!(keys.contains(&"https_proxy"));
    assert!(keys.contains(&"no_proxy"));
    assert!(keys.contains(&"NO_PROXY"));
    assert!(!keys.contains(&"UNRELATED"));
}

#[test]
fn inherited_proxy_envs_appends_target_to_no_proxy_when_supplied() {
    let envs =
        inherited_proxy_envs_from([("NO_PROXY", "localhost,127.0.0.1")], Some("203.0.113.10"));

    let no_proxy = envs
        .iter()
        .find(|(key, _)| key == "NO_PROXY")
        .map(|(_, value)| value.as_str())
        .unwrap();
    assert_eq!(no_proxy, "localhost,127.0.0.1,203.0.113.10");
}

#[test]
fn inherited_proxy_envs_first_no_proxy_wins_when_both_cases_set() {
    // Both `no_proxy` and `NO_PROXY` exist; the first one observed wins,
    // mirroring how docker / process environments propagate the first
    // occurrence of a duplicated env var.
    let envs = inherited_proxy_envs_from(
        [("no_proxy", "first.win"), ("NO_PROXY", "second.lose")],
        None,
    );
    let no_proxy = envs
        .iter()
        .find(|(key, _)| key == "no_proxy")
        .map(|(_, value)| value.as_str())
        .unwrap();
    assert_eq!(no_proxy, "first.win");
}

#[test]
fn inherited_proxy_envs_returns_empty_when_no_proxy_state_exists() {
    // No proxy keys, no NO_PROXY, and no target host -> empty vec, so we
    // do not pollute the docker environment with unrelated knobs.
    let envs: Vec<(String, String)> = inherited_proxy_envs_from([("UNRELATED", "ignored")], None);
    assert!(envs.is_empty());
}

#[test]
fn no_proxy_with_target_dedupes_existing_target_entry() {
    let value = no_proxy_with_target("localhost,203.0.113.10,127.0.0.1", "203.0.113.10");
    assert_eq!(value, "localhost,203.0.113.10,127.0.0.1");
}

#[test]
fn no_proxy_with_target_trims_whitespace_in_existing_entries() {
    let value = no_proxy_with_target("  localhost ,  127.0.0.1 ", "203.0.113.10");
    assert_eq!(value, "localhost,127.0.0.1,203.0.113.10");
}

#[test]
fn secret_fetch_initrd_loads_tdx_guest_module() {
    let setup = secret_fetch_module_setup();
    assert!(setup.contains("instmods tdx_guest"));
    assert!(setup.contains("usr/lib/modprobe.d"));
    assert!(setup.contains("options tdx_guest tsm_api=0"));
    assert!(setup.contains("shelter-tdx-guest.conf"));
    assert!(setup.contains("usr/lib/modules-load.d"));
    assert!(setup.contains("confidential-agent-tdx.conf"));
    assert!(setup.contains("confidential-agent-configfs-mount.service"));
    assert!(setup.contains("mount -t configfs configfs /sys/kernel/config"));
    assert!(setup.contains("Before=trustiflux-api-server-initrd.service"));
    assert!(setup.contains("sysinit.target.wants"));
    assert!(setup.contains("rd.retry=300 rd.shell=0 rd.emergency=poweroff"));
    assert!(setup.contains("36-confidential-agent-fail-closed.conf"));
}
