use super::*;
use confidential_agent_core::peerings::{PeeringEntry, PeeringRole};
use confidential_agent_core::spec::AgentSpec;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const SPEC: &str = r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789]
  connect: [18789]
build:
  base_image: /images/base.qcow2
  image_name: openclaw-agent
  resize: 30G
  packages:
    - nodejs
  scripts:
    - ./install.sh
  variants:
    release:
      enabled: true
    debug:
      enabled: true
      ssh_public_key: ./secrets/debug_ssh.pub
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  disk_gb: 200
attestation:
  tee: tdx
  mode: challenge
  reference_values: rekor
  rekor:
    artifact_id: openclaw-agent
    artifact_type: uki
    slsa_generator: ./slsa-generator
resources: {}
"#;

fn assets() -> GuestAssets {
    GuestAssets {
        agentd_bin: PathBuf::from("/build/confidential-agentd"),
        agentd_service: PathBuf::from("/build/confidential-agentd.service"),
        gateway_bin: PathBuf::from("/build/cai-gateway"),
        gateway_service: PathBuf::from("/build/cai-gateway.service"),
        tng_service: PathBuf::from("/build/trusted-network-gateway.service"),
        initrd_secret_fetch_module: PathBuf::from("/build/99confidential-agent-secret-fetch"),
        fde_config_file: PathBuf::from("/build/fde.toml"),
        policy_default: PathBuf::from("/build/trustee-opa-default.rego"),
        policy_local_dev: PathBuf::from("/build/trustee-opa-local-dev.rego"),
        guest_setup_script: None,
        extra_files: Vec::new(),
    }
}

fn operator_peerings() -> PeeringsFile {
    PeeringsFile {
        version: 1,
        peerings: vec![PeeringEntry {
            label: "ops".to_string(),
            role: PeeringRole::Operator,
            cidr: "203.0.113.0/24".to_string(),
            scope: Vec::new(),
            note: None,
            added_at: None,
            added_by: None,
        }],
    }
}

#[test]
fn build_config_omits_deploy_by_default() {
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            peerings: operator_peerings(),
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    assert!(rendered.contains("from: /images/base.qcow2"));
    assert!(rendered.contains("disk:"));
    assert!(rendered.contains("size: 30G"));
    assert!(!rendered.contains("resize:"));
    assert!(!rendered.contains("variants:"));
    assert!(!rendered.contains("deploy:"));
    assert!(!rendered.contains("backend: terraform"));
    assert!(!rendered.contains("security_group_ports:"));
    assert!(!rendered.contains("control_8006_peer_203_0_113_0_24"));
}

#[test]
fn build_config_is_identical_for_challenge_and_trustee_modes() {
    let challenge = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let trustee = AgentSpec::from_yaml(
        &SPEC.replace("mode: challenge", "mode: trustee"),
        Path::new("/project"),
    )
    .unwrap();

    let challenge_rendered =
        render_build_config(&challenge, &assets(), &ShelterRenderOptions::default()).unwrap();
    let trustee_rendered =
        render_build_config(&trustee, &assets(), &ShelterRenderOptions::default()).unwrap();

    assert_eq!(challenge_rendered, trustee_rendered);
    assert!(!challenge_rendered.contains("user_data"));
}

#[test]
fn deploy_user_data_is_runtime_only_passthrough() {
    let spec = AgentSpec::from_yaml(
        &SPEC.replace("mode: challenge", "mode: trustee"),
        Path::new("/project"),
    )
    .unwrap();
    let runtime = r#"{"kbs_url":"https://trustee.example","schema":"confidential-agent/trustee-runtime/v1","service_id":"openclaw"}"#;

    let build = render_build_config(&spec, &assets(), &ShelterRenderOptions::default()).unwrap();
    let deploy = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            include_deploy: true,
            deploy_user_data: Some(runtime.to_string()),
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    assert!(!build.contains("user_data"));
    let yaml: serde_yaml::Value = serde_yaml::from_str(&deploy).unwrap();
    assert_eq!(yaml["deploy"]["user_data"].as_str(), Some(runtime));
}

#[test]
fn renders_release_shelter_deploy_without_ssh_key_name() {
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            include_deploy: true,
            peerings: operator_peerings(),
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    assert!(rendered.contains("from: /images/base.qcow2"));
    assert!(rendered.contains("disk:"));
    assert!(rendered.contains("size: 30G"));
    assert!(!rendered.contains("resize:"));
    assert!(!rendered.contains("variants:"));
    assert!(!rendered.contains("name: debug"));
    assert!(!rendered.contains("harden_mode: partial"));
    assert!(!rendered.contains("ssh_key:"));
    assert!(!rendered.contains("/project/secrets/debug_ssh.pub"));
    assert!(rendered.contains("trustiflux:"));
    assert!(rendered.contains("tng: true"));
    assert!(rendered.contains("cryptpilot-fde-host:"));
    assert!(rendered.contains("cryptpilot-fde-guest:"));
    assert!(rendered.contains("disk:"));
    assert!(rendered.contains("engine: cryptpilot"));
    assert!(rendered.contains("cryptpilot:"));
    assert!(!rendered.contains("disk-crypt:"));
    assert!(rendered.contains("fde_config_file: /build/fde.toml"));
    assert!(rendered.contains("99confidential-agent-secret-fetch"));
    assert!(!rendered.contains("security_group_ports:"));
    assert!(!rendered.contains("security_group_allowed_cidr:"));
    assert!(rendered.contains("backend: terraform"));
    assert!(!rendered.contains("image_id:"));
    assert!(rendered.contains("image:"));
    assert!(rendered.contains("name: openclaw-agent-release"));
    assert!(rendered.contains("cloud: alicloud"));
    assert!(rendered.contains("region: cn-beijing"));
    assert!(rendered.contains("zone_id: cn-beijing-l"));
    assert!(rendered.contains("cc_mode: tdx"));
    assert!(!rendered.contains("cc: tdx"));
    assert!(!rendered.contains("tdx: true"));
    assert!(rendered.contains("name: control_8006_peer_203_0_113_0_24"));
    assert!(rendered.contains("port_range: 8006/8006"));
    assert!(rendered.contains("name: status_8088_peer_203_0_113_0_24"));
    assert!(rendered.contains("port_range: 8088/8088"));
    assert!(rendered.contains("name: connect_18789_peer_203_0_113_0_24"));
    assert!(!rendered.contains("name: mesh_18789"));
    assert!(!rendered.contains("cidr: vpc"));
    assert!(!rendered.contains("openssh-server"));
    assert!(!rendered.contains("sshd.service"));
    assert!(!rendered.contains("22/22"));
    assert!(!rendered.contains("ssh_key_name"));
    assert!(!rendered.contains("backend_port"));
    assert!(!rendered.contains("runtime:"));
}

#[test]
fn renders_mkosi_build_without_from_or_legacy_variants() {
    let spec = AgentSpec::from_yaml(
        &SPEC.replace("  base_image: /images/base.qcow2\n", ""),
        Path::new("/project"),
    )
    .unwrap();
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            include_deploy: true,
            peerings: operator_peerings(),
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    assert!(!rendered.contains("from:"));
    assert!(!rendered.contains("variants:"));
    assert!(rendered.contains("packages:"));
    assert!(rendered.contains("libtdx-verify"));
    assert!(rendered.contains("engine: cryptpilot"));
    assert!(!rendered.contains("disk-crypt:"));
    assert!(rendered.contains("image:"));
    assert!(rendered.contains("name: openclaw-agent-release"));
}

#[test]
fn renders_debug_deploy_with_ssh_security_group() {
    let spec = AgentSpec::from_yaml(
        &SPEC.replace("image_variant: release", "image_variant: debug"),
        Path::new("/project"),
    )
    .unwrap();
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            include_deploy: true,
            peerings: operator_peerings(),
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    assert!(!rendered.contains("variants:"));
    assert!(!rendered.contains("name: debug"));
    assert!(rendered.contains("mode: partial"));
    assert!(rendered.contains("openssh-server"));
    assert!(rendered.contains("sshd.service"));
    assert!(rendered.contains("ssh_key:"));
    assert!(rendered.contains("/project/secrets/debug_ssh.pub"));
    assert!(!rendered.contains("image_id:"));
    assert!(rendered.contains("name: openclaw-agent-debug"));
    assert!(rendered.contains("22/22"));
}

#[test]
fn renders_mesh_peer_rules_for_all_service_ports() {
    let spec = AgentSpec::from_yaml(
        &SPEC.replace(
            "ports: [18789]\n  connect: [18789]",
            "ports: [3001]\n  connect: []",
        ),
        Path::new("/project"),
    )
    .unwrap();
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            include_deploy: true,
            mesh_peer_cidrs: vec!["39.105.93.168/32".to_string()],
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    assert!(rendered.contains("name: mesh_3001_peer_39_105_93_168_32"));
    assert!(rendered.contains("port_range: 3001/3001"));
    assert!(rendered.contains("cidr: 39.105.93.168/32"));
    assert!(!rendered.contains("cidr: vpc"));
    assert!(!rendered.contains("name: connect_3001"));
}

#[test]
fn renders_peerings_for_agent_card_and_connect_ports() {
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let mut peerings = operator_peerings();
    peerings.peerings.push(PeeringEntry {
        label: "beta".to_string(),
        role: PeeringRole::Peer,
        cidr: "198.51.100.10/32".to_string(),
        scope: Vec::new(),
        note: None,
        added_at: None,
        added_by: None,
    });
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            include_deploy: true,
            peerings,
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    assert!(rendered.contains("name: agent_card_8089_peer_198_51_100_10_32"));
    assert!(rendered.contains("port_range: 8089/8089"));
    assert!(rendered.contains("name: connect_18789_peer_198_51_100_10_32"));
    assert!(rendered.contains("port_range: 18789/18789"));
    assert!(!rendered.contains("name: mesh_18789_peer_198_51_100_10_32"));
    assert!(rendered.contains("cidr: 198.51.100.10/32"));
}

#[test]
fn renders_public_mesh_peer_cidrs_and_stable_resource_names() {
    let spec = AgentSpec::from_yaml(
        &SPEC.replace(
            "ports: [18789]\n  connect: [18789]",
            "ports: [18789, 18800]\n  connect: [18789]",
        ),
        Path::new("/project"),
    )
    .unwrap();
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            include_deploy: true,
            images_dir: Some(PathBuf::from("/state/services/openclaw/artifacts")),
            cache_dir: Some(PathBuf::from("/state/services/openclaw/cache")),
            deploy_resource_name: Some("openclaw-20260429201011".to_string()),
            local_image_source: Some(PathBuf::from(
                "/var/lib/shelter/images/openclaw-agent/final-debug.qcow2",
            )),
            local_image_import_name: Some("openclaw-agent-debug-20260429201011".to_string()),
            mesh_peer_cidrs: vec!["39.105.93.168/32".to_string()],
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    assert!(rendered.contains("name: openclaw-20260429201011"));
    assert!(!rendered.contains("images_dir:"));
    assert!(!rendered.contains("cache_dir:"));
    assert!(rendered.contains("name: openclaw-agent-debug-20260429201011"));
    assert!(!rendered.contains("bucket:"));
    assert!(rendered.contains("name: mesh_18800_peer_39_105_93_168_32"));
    assert!(!rendered.contains("name: mesh_18789_peer_39_105_93_168_32"));
    assert!(rendered.contains("cidr: 39.105.93.168/32"));
    assert!(!rendered.contains("cidr: vpc"));
}

#[test]
fn renders_gateway_and_guest_setup_without_tng_binary_override() {
    let mut assets = assets();
    assets.guest_setup_script = Some(PathBuf::from("/build/guest-setup.sh"));

    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let rendered = render_build_config(&spec, &assets, &ShelterRenderOptions::default()).unwrap();

    assert!(rendered.contains("destination: /usr/local/bin/cai-gateway"));
    assert!(rendered.contains("destination: /etc/systemd/system/cai-gateway.service"));
    assert!(rendered.contains("destination: /etc/systemd/system/trusted-network-gateway.service"));
    assert!(!rendered.contains("destination: /opt/confidential-agent/hack/tng-"));
    assert!(rendered.contains("path: /build/guest-setup.sh"));
    assert!(!rendered.contains("destination: /usr/bin/tng"));
    assert!(!rendered.contains("/usr/local/bin/tng"));
    assert!(!rendered.contains("trusted-network-gateway.service.d"));
}

#[test]
fn renders_extra_guest_files_with_destination_and_executable_flag() {
    let mut assets = assets();
    assets.extra_files.push(GuestFileAsset {
        source: PathBuf::from("/project/files/cai-pep"),
        destination: "/usr/local/bin/cai-pep".to_string(),
        executable: true,
    });
    assets.extra_files.push(GuestFileAsset {
        source: PathBuf::from("/project/files/cai-pep-plugin"),
        destination: "/usr/local/share/confidential-agent/openclaw/cai-pep-plugin".to_string(),
        executable: false,
    });

    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let rendered = render_build_config(&spec, &assets, &ShelterRenderOptions::default()).unwrap();

    assert!(rendered.contains("source: /project/files/cai-pep"));
    assert!(rendered.contains("destination: /usr/local/bin/cai-pep"));
    assert!(rendered.contains("executable: true"));
    assert!(rendered.contains("source: /project/files/cai-pep-plugin"));
    assert!(rendered
        .contains("destination: /usr/local/share/confidential-agent/openclaw/cai-pep-plugin"));
}

#[test]
fn renders_rekor_config_from_attestation() {
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let rendered = render_build_config(&spec, &assets(), &ShelterRenderOptions::default()).unwrap();

    assert!(rendered.contains("rekor:"));
    assert!(rendered.contains("artifact_id: openclaw-agent"));
    assert!(rendered.contains("slsa_generator:"));
    assert!(rendered.contains("/project/slsa-generator"));
}

#[test]
fn renders_vllm_uki_append_cmdline() {
    let spec = AgentSpec::from_yaml(
        r#"
schema: confidential-agent/v1
service:
  id: openclaw-vllm
  ports: [18789]
  connect: [18789]
build:
  image_name: openclaw-vllm
  kernel_cmdline_append: swiotlb=4194304,any
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
resources: {}
"#,
        Path::new("/project"),
    )
    .unwrap();

    let rendered = render_build_config(&spec, &assets(), &ShelterRenderOptions::default()).unwrap();

    assert!(rendered.contains("uki_append_cmdline: swiotlb=4194304,any"));
}

#[test]
fn local_image_import_name_defaults_to_build_id() {
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            include_deploy: true,
            local_image_source: Some(PathBuf::from(
                "/var/lib/shelter/images/openclaw-agent/final-release-202604281803.qcow2",
            )),
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    assert!(rendered.contains("name: openclaw-agent-release"));
    assert!(!rendered.contains("bucket:"));
}

#[test]
fn preferred_tool_path_uses_first_existing_candidate() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "confidential-agent-shelter-tool-test-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    let missing = dir.join("missing-tool");
    let candidate = dir.join("cryptpilot-fde-host");
    fs::write(&candidate, b"test").unwrap();

    let selected =
        preferred_existing_tool_path([missing.as_path(), candidate.as_path()], "fallback");

    assert_eq!(selected, candidate.to_string_lossy());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn preferred_tool_path_falls_back_when_no_candidate_exists() {
    let selected =
        preferred_existing_tool_path(["/definitely/missing/cryptpilot-fde-host"], "fallback");

    assert_eq!(selected, "fallback");
}

#[test]
fn renders_explicit_rootfs_integrity_for_modern_shelter() {
    // Shelter infers reference-value extraction from
    // `security.disk.cryptpilot.rootfs.integrity`.
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let rendered = render_build_config(&spec, &assets(), &ShelterRenderOptions::default()).unwrap();

    assert!(rendered.contains("engine: cryptpilot"));
    assert!(rendered.contains("cryptpilot:"));
    assert!(rendered.contains("rootfs:"));
    assert!(rendered.contains("integrity: true"));
    assert!(!rendered.contains("disk-crypt:"));
    assert!(!rendered.contains("extract_reference_values"));
}

#[test]
fn render_deploy_with_cloud_image_id_omits_local_image_import() {
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            include_deploy: true,
            cloud_image_id: Some("m-2ze123abc".to_string()),
            peerings: operator_peerings(),
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    let deploy_section = rendered.split("deploy:").nth(1).unwrap();
    assert!(deploy_section.contains("image_id: m-2ze123abc"));
    assert!(!deploy_section.contains("image:\n"));
    assert!(!deploy_section.contains("nvme_support:"));
}

#[test]
fn render_deploy_without_cloud_image_id_keeps_local_image_import() {
    let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
    let rendered = render_build_config(
        &spec,
        &assets(),
        &ShelterRenderOptions {
            include_deploy: true,
            peerings: operator_peerings(),
            ..ShelterRenderOptions::default()
        },
    )
    .unwrap();

    let deploy_section = rendered.split("deploy:").nth(1).unwrap();
    assert!(!deploy_section.contains("image_id:"));
    assert!(deploy_section.contains("image:\n"));
    assert!(deploy_section.contains("nvme_support: supported"));
}
