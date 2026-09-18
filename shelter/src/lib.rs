use anyhow::Result;
use confidential_agent_core::peerings::{PeeringScope, PeeringsFile};
use confidential_agent_core::schema::{confidential_ports, AGENT_CARD_PORT, DAEMON_STATUS_PORT};
use confidential_agent_core::spec::{
    AgentSpec, AttestationMode, AttestationTee, BuildCleanupSpec, BuildContainerSpec,
    ReferenceValueMode, RekorSpec,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct GuestAssets {
    pub agentd_bin: PathBuf,
    pub agentd_service: PathBuf,
    pub gateway_bin: PathBuf,
    pub gateway_service: PathBuf,
    pub tng_service: PathBuf,
    pub initrd_secret_fetch_module: PathBuf,
    pub fde_config_file: PathBuf,
    pub policy_default: PathBuf,
    pub policy_local_dev: PathBuf,
    pub guest_setup_script: Option<PathBuf>,
    pub extra_files: Vec<GuestFileAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestFileAsset {
    pub source: PathBuf,
    pub destination: String,
    pub executable: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ShelterRenderOptions {
    pub build_id: Option<String>,
    pub images_dir: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
    pub terraform_dir: Option<PathBuf>,
    pub include_deploy: bool,
    pub local_image_source: Option<PathBuf>,
    pub deploy_resource_name: Option<String>,
    pub local_image_import_name: Option<String>,
    pub cloud_image_id: Option<String>,
    /// Runtime-only deployment data. This is never rendered into build-only
    /// configs and is intentionally independent from Shelter's Trustee mode.
    pub deploy_user_data: Option<String>,
    pub mesh_peer_cidrs: Vec<String>,
    pub peerings: PeeringsFile,
}

pub fn render_build_config(
    spec: &AgentSpec,
    assets: &GuestAssets,
    options: &ShelterRenderOptions,
) -> Result<String> {
    spec.ensure_mvp_supported()?;

    let config = ShelterBuildConfig {
        base_image: spec.build.base_image.clone(),
        output: spec.build.resize.as_ref().map(|size| ShelterOutputConfig {
            disk_size: Some(size.clone()),
        }),
        with_network: spec.build.with_network,
        container: spec.build.container.clone(),
        packages: shelter_packages(spec, assets),
        cleanup: shelter_cleanup(&spec.build.cleanup),
        files: guest_files(assets),
        scripts: assets
            .guest_setup_script
            .iter()
            .map(|path| ShelterScript {
                path: path.clone(),
                stage: "post-install".to_string(),
            })
            .chain(spec.build.scripts.iter().map(|path| ShelterScript {
                path: path.clone(),
                stage: "post-install".to_string(),
            }))
            .collect(),
        services: shelter_services(spec),
        rekor: render_rekor_config(spec, options.build_id.as_deref()),
        tools: ShelterTools::host_defaults(),
        security: ShelterSecurity::challenge_defaults(assets, spec),
        initrd: ShelterInitrd {
            modules: vec![ShelterDracutModule {
                path: assets.initrd_secret_fetch_module.clone(),
            }],
        },
        deploy: if options.include_deploy {
            render_deploy_config(spec, options)
        } else {
            None
        },
    };

    serde_yaml::to_string(&config).map_err(Into::into)
}

fn shelter_packages(spec: &AgentSpec, _assets: &GuestAssets) -> Vec<String> {
    let mut packages = spec.build.packages.clone();
    if !packages.iter().any(|package| package == "libtdx-verify") {
        packages.push("libtdx-verify".to_string());
    }
    if spec.deploys_debug_image() && !packages.iter().any(|package| package == "openssh-server") {
        packages.push("openssh-server".to_string());
    }
    packages
}

fn shelter_cleanup(cleanup: &BuildCleanupSpec) -> Option<ShelterCleanupConfig> {
    (!cleanup.is_empty()).then(|| ShelterCleanupConfig {
        remove_static_libs: cleanup.remove_static_libs,
    })
}

fn shelter_services(spec: &AgentSpec) -> Vec<ShelterServiceUnit> {
    let mut services = vec![
        ShelterServiceUnit {
            name: "confidential-agentd.service".to_string(),
            enable: true,
        },
        ShelterServiceUnit {
            name: "cai-gateway.service".to_string(),
            enable: true,
        },
    ];
    if spec.deploys_debug_image() {
        services.push(ShelterServiceUnit {
            name: "sshd.service".to_string(),
            enable: true,
        });
    }
    services
}

fn guest_files(assets: &GuestAssets) -> Vec<ShelterFileMapping> {
    let mut files = vec![
        ShelterFileMapping {
            source: assets.agentd_bin.clone(),
            destination: Some("/usr/local/bin/confidential-agentd".to_string()),
            executable: true,
        },
        ShelterFileMapping {
            source: assets.agentd_service.clone(),
            destination: Some("/etc/systemd/system/confidential-agentd.service".to_string()),
            executable: false,
        },
        ShelterFileMapping {
            source: assets.gateway_bin.clone(),
            destination: Some("/usr/local/bin/cai-gateway".to_string()),
            executable: true,
        },
        ShelterFileMapping {
            source: assets.gateway_service.clone(),
            destination: Some("/etc/systemd/system/cai-gateway.service".to_string()),
            executable: false,
        },
        ShelterFileMapping {
            source: assets.tng_service.clone(),
            destination: Some("/etc/systemd/system/trusted-network-gateway.service".to_string()),
            executable: false,
        },
        ShelterFileMapping {
            source: assets.policy_default.clone(),
            destination: Some(
                "/opt/confidential-agent/policies/trustee-opa-default.rego".to_string(),
            ),
            executable: false,
        },
        ShelterFileMapping {
            source: assets.policy_local_dev.clone(),
            destination: Some(
                "/opt/confidential-agent/policies/trustee-opa-local-dev.rego".to_string(),
            ),
            executable: false,
        },
    ];

    files.extend(assets.extra_files.iter().map(|asset| ShelterFileMapping {
        source: asset.source.clone(),
        destination: Some(asset.destination.clone()),
        executable: asset.executable,
    }));

    files
}

fn render_deploy_config(
    spec: &AgentSpec,
    options: &ShelterRenderOptions,
) -> Option<ShelterDeployConfig> {
    let deploy = &spec.deploy;
    let mut tags = BTreeMap::new();
    tags.insert(
        "confidential-agent-service".to_string(),
        spec.service.id.clone(),
    );
    tags.insert(
        "confidential-agent-image-variant".to_string(),
        spec.image_variant().to_string(),
    );

    Some(ShelterDeployConfig {
        name: options
            .deploy_resource_name
            .clone()
            .unwrap_or_else(|| spec.service.id.clone()),
        backend: "terraform".to_string(),
        cloud: "alicloud".to_string(),
        terraform_dir: options.terraform_dir.clone(),
        region: deploy.region.clone(),
        zone_id: deploy.zone_id.clone(),
        ip: deploy.private_ip.clone(),
        instance_type: deploy.instance_type.clone(),
        cc_mode: Some(shelter_cc(spec.attestation.tee).to_string()),
        disk_size: deploy.disk_gb,
        security_group: ShelterDeploySecurityGroup {
            rules: security_group_rules(spec, options),
        },
        image_id: options.cloud_image_id.clone(),
        image: if options.cloud_image_id.is_some() {
            None
        } else {
            Some(ShelterDeployImageConfig {
                source: options.local_image_source.as_ref().map(|source| {
                    ShelterDeployImageSource {
                        path: source.clone(),
                        base: "config_dir".to_string(),
                    }
                }),
                name: Some(
                    options
                        .local_image_import_name
                        .clone()
                        .unwrap_or_else(|| shelter_build_id(spec)),
                ),
                bucket: None,
                nvme_support: Some("supported".to_string()),
            })
        },
        vpc_id: deploy.vpc_id.clone(),
        vswitch_id: deploy.vswitch_id.clone(),
        security_group_id: deploy.security_group_id.clone(),
        user_data: options.deploy_user_data.clone(),
        tags,
    })
}

pub fn shelter_build_id(spec: &AgentSpec) -> String {
    format!("{}-{}", spec.image_id(), spec.image_variant())
}

fn render_rekor_config(spec: &AgentSpec, build_id: Option<&str>) -> Option<ShelterRekorConfig> {
    if spec.attestation.reference_values != ReferenceValueMode::Rekor {
        return None;
    }

    spec.attestation.rekor.as_ref().map(|rekor| {
        let artifact_id = rekor.artifact_id.clone().unwrap_or_else(|| {
            build_id
                .map(str::to_string)
                .unwrap_or_else(|| shelter_build_id(spec))
        });
        ShelterRekorConfig::from_spec(artifact_id, rekor)
    })
}

fn security_group_rules(
    spec: &AgentSpec,
    options: &ShelterRenderOptions,
) -> Vec<ShelterDeploySecurityGroupRule> {
    let mut rules = Vec::new();
    for cidr in options.peerings.cidrs_for_scope(PeeringScope::Control) {
        rules.push(ShelterDeploySecurityGroupRule::ingress(
            format!("control_8006_peer_{}", sanitize_rule_name_component(&cidr)),
            "8006/8006",
            cidr,
        ));
    }
    for cidr in options.peerings.cidrs_for_scope(PeeringScope::Status) {
        rules.push(ShelterDeploySecurityGroupRule::ingress(
            format!(
                "status_{DAEMON_STATUS_PORT}_peer_{}",
                sanitize_rule_name_component(&cidr)
            ),
            format!("{0}/{0}", DAEMON_STATUS_PORT),
            cidr,
        ));
    }
    for cidr in options.peerings.cidrs_for_scope(PeeringScope::AgentCard) {
        rules.push(ShelterDeploySecurityGroupRule::ingress(
            format!(
                "agent_card_{AGENT_CARD_PORT}_peer_{}",
                sanitize_rule_name_component(&cidr)
            ),
            format!("{0}/{0}", AGENT_CARD_PORT),
            cidr,
        ));
    }
    if spec.deploys_debug_image() {
        for cidr in options.peerings.cidrs_for_scope(PeeringScope::Ssh) {
            rules.push(ShelterDeploySecurityGroupRule::ingress(
                format!("ssh_22_peer_{}", sanitize_rule_name_component(&cidr)),
                "22/22",
                cidr,
            ));
        }
    }
    for port in &spec.service.connect {
        for cidr in options.peerings.cidrs_for_scope(PeeringScope::Connect) {
            rules.push(ShelterDeploySecurityGroupRule::ingress(
                format!(
                    "connect_{port}_peer_{}",
                    sanitize_rule_name_component(&cidr)
                ),
                format!("{0}/{0}", port),
                cidr,
            ));
        }
    }
    let mesh_ports = confidential_ports(&spec.service.ports, &spec.service.connect);
    let peer_mesh_cidrs = options
        .mesh_peer_cidrs
        .iter()
        .cloned()
        .chain(options.peerings.cidrs_for_scope(PeeringScope::Mesh))
        .collect::<BTreeSet<_>>();
    for port in mesh_ports.iter() {
        for cidr in &peer_mesh_cidrs {
            rules.push(ShelterDeploySecurityGroupRule::ingress(
                format!("mesh_{port}_peer_{}", sanitize_rule_name_component(cidr)),
                format!("{0}/{0}", port),
                cidr.clone(),
            ));
        }
    }
    rules
}

fn sanitize_rule_name_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

#[derive(Debug, Serialize)]
struct ShelterBuildConfig {
    #[serde(rename = "from")]
    #[serde(skip_serializing_if = "Option::is_none")]
    base_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<ShelterOutputConfig>,
    #[serde(skip_serializing_if = "is_false")]
    with_network: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    container: Option<BuildContainerSpec>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    packages: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cleanup: Option<ShelterCleanupConfig>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    files: Vec<ShelterFileMapping>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    scripts: Vec<ShelterScript>,
    services: Vec<ShelterServiceUnit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rekor: Option<ShelterRekorConfig>,
    tools: ShelterTools,
    security: ShelterSecurity,
    initrd: ShelterInitrd,
    #[serde(skip_serializing_if = "Option::is_none")]
    deploy: Option<ShelterDeployConfig>,
}

#[derive(Debug, Serialize)]
struct ShelterOutputConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    disk_size: Option<String>,
}

#[derive(Debug, Serialize)]
struct ShelterCleanupConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    remove_static_libs: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ShelterRekorConfig {
    enabled: bool,
    artifact_id: String,
    artifact_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_version: Option<String>,
    rekor_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cosign_key: Option<PathBuf>,
    slsa_generator: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    rv_name: Option<String>,
    required: bool,
}

impl ShelterRekorConfig {
    fn from_spec(artifact_id: String, spec: &RekorSpec) -> Self {
        Self {
            enabled: true,
            artifact_id,
            artifact_type: spec.artifact_type.clone(),
            artifact_version: spec.artifact_version.clone(),
            rekor_url: spec.rekor_url.clone(),
            cosign_key: spec.cosign_key.clone(),
            slsa_generator: spec.slsa_generator.clone(),
            rv_name: spec.rv_name.clone(),
            required: spec.required,
        }
    }
}

#[derive(Debug, Serialize)]
struct ShelterFileMapping {
    source: PathBuf,
    destination: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    executable: bool,
}

#[derive(Debug, Serialize)]
struct ShelterScript {
    path: PathBuf,
    stage: String,
}

#[derive(Debug, Serialize)]
struct ShelterServiceUnit {
    name: String,
    enable: bool,
}

#[derive(Debug, Serialize)]
struct ShelterTools {
    #[serde(rename = "cryptpilot-enhance")]
    cryptpilot_enhance: String,
    #[serde(rename = "cryptpilot-convert")]
    cryptpilot_convert: String,
    #[serde(rename = "cryptpilot-fde-host")]
    cryptpilot_fde_host: String,
    #[serde(rename = "cryptpilot-fde-guest")]
    cryptpilot_fde_guest: String,
}

impl ShelterTools {
    fn host_defaults() -> Self {
        Self {
            cryptpilot_enhance: "cryptpilot-enhance".to_string(),
            cryptpilot_convert: "cryptpilot-convert".to_string(),
            cryptpilot_fde_host: preferred_cryptpilot_fde_host_tool(),
            cryptpilot_fde_guest: preferred_cryptpilot_fde_guest_tool(),
        }
    }
}

fn preferred_cryptpilot_fde_host_tool() -> String {
    preferred_existing_tool_path(
        [
            "/usr/bin/cryptpilot-fde-host",
            "/usr/local/bin/cryptpilot-fde-host",
        ],
        "cryptpilot-fde-host",
    )
}

fn preferred_cryptpilot_fde_guest_tool() -> String {
    preferred_existing_tool_path(
        [
            "/usr/bin/cryptpilot-fde-guest",
            "/usr/local/bin/cryptpilot-fde-guest",
        ],
        "cryptpilot-fde-guest",
    )
}

fn preferred_existing_tool_path<I, P>(candidates: I, fallback: &str) -> String
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    candidates
        .into_iter()
        .map(|candidate| candidate.as_ref().to_path_buf())
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback.to_string())
}

#[derive(Debug, Serialize)]
struct ShelterSecurity {
    harden: ShelterHarden,
    trustiflux: ShelterTrustiflux,
    tng: bool,
    disk: ShelterSecurityDisk,
}

impl ShelterSecurity {
    fn challenge_defaults(assets: &GuestAssets, spec: &AgentSpec) -> Self {
        Self {
            harden: ShelterHarden {
                enabled: true,
                mode: if spec.deploys_debug_image() {
                    "partial".to_string()
                } else {
                    "full".to_string()
                },
                ssh_key: spec
                    .build
                    .variants
                    .debug
                    .as_ref()
                    .and_then(|debug| {
                        spec.deploys_debug_image()
                            .then(|| debug.ssh_public_key.clone())
                    })
                    .flatten(),
            },
            trustiflux: ShelterTrustiflux::challenge_defaults(),
            tng: true,
            disk: ShelterSecurityDisk::cryptpilot_defaults(assets, spec),
        }
    }
}

#[derive(Debug, Serialize)]
struct ShelterHarden {
    enabled: bool,
    mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ssh_key: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct ShelterSecurityDisk {
    engine: String,
    cryptpilot: ShelterCryptpilotDisk,
}

impl ShelterSecurityDisk {
    fn cryptpilot_defaults(assets: &GuestAssets, spec: &AgentSpec) -> Self {
        Self {
            engine: "cryptpilot".to_string(),
            cryptpilot: ShelterCryptpilotDisk::writable_layer_defaults(assets, spec),
        }
    }
}

#[derive(Debug, Serialize)]
struct ShelterCryptpilotDisk {
    fde_config_file: PathBuf,
    rootfs: ShelterCryptpilotRootfs,
    #[serde(skip_serializing_if = "Option::is_none")]
    uki_append_cmdline: Option<String>,
}

impl ShelterCryptpilotDisk {
    fn writable_layer_defaults(assets: &GuestAssets, spec: &AgentSpec) -> Self {
        Self {
            fde_config_file: assets.fde_config_file.clone(),
            // Shelter drives reference-value extraction off
            // `security.disk.cryptpilot.rootfs.integrity`.
            rootfs: ShelterCryptpilotRootfs { integrity: true },
            uki_append_cmdline: spec.build.kernel_cmdline_append.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ShelterCryptpilotRootfs {
    integrity: bool,
}

#[derive(Debug, Serialize)]
struct ShelterInitrd {
    modules: Vec<ShelterDracutModule>,
}

#[derive(Debug, Serialize)]
struct ShelterDracutModule {
    path: PathBuf,
}

#[derive(Debug, Serialize)]
struct ShelterTrustiflux {
    cdh: ShelterCdh,
    aa: ShelterAa,
    api_server: ShelterApiServer,
}

impl ShelterTrustiflux {
    fn challenge_defaults() -> Self {
        Self {
            cdh: ShelterCdh {
                enabled: true,
                socket: "unix:///run/confidential-containers/cdh.sock".to_string(),
                kbc: ShelterCdhKbc {
                    name: Some("cc_kbc".to_string()),
                    url: Some("http://127.0.0.1:8081".to_string()),
                },
            },
            aa: ShelterAa {
                enabled: true,
                token_configs: ShelterAaTokenConfigs {
                    coco_as: ShelterUrl {
                        url: Some("http://127.0.0.1:8081/api/attestation-service".to_string()),
                    },
                    kbs: ShelterUrl {
                        url: Some("http://127.0.0.1:8081/api".to_string()),
                    },
                },
                aa_instance: ShelterAaInstance {
                    instance_type: Some("aliyun_ecs".to_string()),
                    heartbeat: ShelterHeartbeat {
                        enabled: false,
                        trustee_url: None,
                    },
                },
            },
            api_server: ShelterApiServer {
                enabled: true,
                bind: Some("0.0.0.0:8006".to_string()),
                enable_cdh: true,
                enable_aa: true,
                allow_remote_get_evidence: true,
                allow_remote_resource_injection: true,
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct ShelterCdh {
    enabled: bool,
    socket: String,
    kbc: ShelterCdhKbc,
}

#[derive(Debug, Serialize)]
struct ShelterCdhKbc {
    url: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Serialize)]
struct ShelterAa {
    enabled: bool,
    token_configs: ShelterAaTokenConfigs,
    aa_instance: ShelterAaInstance,
}

#[derive(Debug, Serialize)]
struct ShelterAaTokenConfigs {
    coco_as: ShelterUrl,
    kbs: ShelterUrl,
}

#[derive(Debug, Serialize)]
struct ShelterUrl {
    url: Option<String>,
}

#[derive(Debug, Serialize)]
struct ShelterAaInstance {
    instance_type: Option<String>,
    heartbeat: ShelterHeartbeat,
}

#[derive(Debug, Serialize)]
struct ShelterHeartbeat {
    enabled: bool,
    trustee_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct ShelterApiServer {
    enabled: bool,
    bind: Option<String>,
    enable_cdh: bool,
    enable_aa: bool,
    allow_remote_get_evidence: bool,
    allow_remote_resource_injection: bool,
}

#[derive(Debug, Serialize)]
struct ShelterDeployConfig {
    name: String,
    backend: String,
    cloud: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    terraform_dir: Option<PathBuf>,
    region: String,
    zone_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ip: Option<String>,
    instance_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cc_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    disk_size: Option<u32>,
    security_group: ShelterDeploySecurityGroup,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<ShelterDeployImageConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vpc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vswitch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    security_group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_data: Option<String>,
    tags: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct ShelterDeploySecurityGroup {
    rules: Vec<ShelterDeploySecurityGroupRule>,
}

#[derive(Debug, Serialize)]
struct ShelterDeploySecurityGroupRule {
    name: String,
    #[serde(rename = "type")]
    rule_type: String,
    protocol: String,
    port_range: String,
    cidr: String,
    policy: String,
    priority: u32,
    nic_type: String,
}

impl ShelterDeploySecurityGroupRule {
    fn ingress(
        name: impl Into<String>,
        port_range: impl Into<String>,
        cidr: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            rule_type: "ingress".to_string(),
            protocol: "tcp".to_string(),
            port_range: port_range.into(),
            cidr: cidr.into(),
            policy: "accept".to_string(),
            priority: 1,
            nic_type: "intranet".to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ShelterDeployImageConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<ShelterDeployImageSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bucket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nvme_support: Option<String>,
}

#[derive(Debug, Serialize)]
struct ShelterDeployImageSource {
    path: PathBuf,
    base: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub fn attestation_mode_name(mode: AttestationMode) -> &'static str {
    match mode {
        AttestationMode::Challenge => "challenge",
        AttestationMode::Trustee => "trustee",
    }
}

fn shelter_cc(tee: AttestationTee) -> &'static str {
    match tee {
        AttestationTee::Tdx => "tdx",
    }
}

#[cfg(test)]
mod tests;
