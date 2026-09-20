use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub const LOCAL_SERVICE_STATE_SCHEMA_VERSION: &str = "confidential-agent/service-state/v1";
pub const BOOTSTRAP_SCHEMA_VERSION: &str = "confidential-agent/bootstrap/v1";
pub const MESH_SCHEMA_VERSION: &str = "confidential-agent/mesh-bundle/v1";
pub const SERVICE_DIRECTORY_SCHEMA_VERSION: &str = "confidential-agent/services/v1";
pub const DAEMON_STATUS_SCHEMA_VERSION: &str = "confidential-agent/daemon-status/v1";
pub const DAEMON_STATUS_PORT: u16 = 8088;
pub const AGENT_CARD_PORT: u16 = 8089;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalServiceState {
    pub schema: String,
    pub service_id: String,
    pub generation: u64,
    pub phase: String,
    /// Immutable snapshot of the attestation provider selected for the
    /// deployed service. Older challenge-only state files deserialize as
    /// `challenge`; new Trustee deployments use this marker to fail closed if
    /// their dedicated Trustee state is lost.
    #[serde(default = "default_bootstrap_mode")]
    pub attestation_mode: String,
    pub spec: LocalSpecState,
    pub build: LocalBuildState,
    pub deploy: LocalDeployState,
    pub service: LocalServiceNetwork,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_identity: Option<LocalGatewayIdentity>,
    #[serde(default)]
    pub resources: BTreeMap<String, LocalResourceState>,
    pub mesh_generation: u64,
    pub reference_values: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalSpecState {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalBuildState {
    pub build_id: String,
    pub image_name: String,
    pub variant: String,
    pub image_path: PathBuf,
    pub images_dir: PathBuf,
    pub cache_dir: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug_ssh: Option<LocalDebugSshKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rv: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rekor_meta: Option<PathBuf>,
    #[serde(default)]
    pub remote: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub published: BTreeMap<String, PublishedImage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalDebugSshKey {
    pub private_key: PathBuf,
    pub public_key: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishedImage {
    pub provider: String,
    pub region: String,
    pub variant: String,
    pub build_id: String,
    pub source_sha256: String,
    pub source_size: u64,
    pub status: String,
    pub image_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_key: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub oss_cleaned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalDeployState {
    pub provider: String,
    pub run_id: String,
    pub resource_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terraform_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_source: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_import_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security_group_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_ip: Option<String>,
    pub tee: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_image_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instance_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instance_private_ips: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slb_vip: Option<String>,
}

impl LocalDeployState {
    pub fn preferred_injection_ip(&self) -> Option<&str> {
        self.public_ip
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                self.private_ip
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
            })
    }

    pub fn private_mesh_ip(&self) -> Option<&str> {
        self.private_ip
            .as_deref()
            .filter(|value| !value.trim().is_empty())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalServiceNetwork {
    pub ports: Vec<u16>,
    #[serde(default)]
    pub connect: Vec<crate::spec::ConnectPort>,
    #[serde(default)]
    pub mcp_ports: Vec<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalGatewayIdentity {
    pub public_key: String,
    pub private_key_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalResourceState {
    pub sha256: String,
    pub target: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub mode: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mutable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BootstrapConfig {
    pub schema: String,
    pub generation: u64,
    pub service_id: String,
    #[serde(default = "default_bootstrap_mode")]
    pub mode: String,
    #[serde(default)]
    pub ports: Vec<u16>,
    #[serde(default)]
    pub connect: Vec<crate::spec::ConnectPort>,
    #[serde(default)]
    pub mcp_ports: Vec<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_identity: Option<GatewayIdentity>,
    #[serde(default)]
    pub resources: Vec<GuestResource>,
    #[serde(default)]
    pub app_service: Option<String>,
    #[serde(default)]
    pub peers: Vec<BootstrapPeer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_card: Option<AgentCard>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayIdentity {
    pub public_key: String,
    pub private_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuestResource {
    pub id: String,
    pub resource_path: String,
    pub target: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default = "default_resource_mode")]
    pub mode: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mutable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BootstrapPeer {
    pub id: String,
    pub url: String,
    #[serde(default = "default_peer_policy_str")]
    pub policy: String,
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval_sec: u64,
    #[serde(default)]
    pub ports: Vec<u16>,
    #[serde(default)]
    pub port_mappings: Vec<BootstrapPeerPortMapping>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BootstrapPeerPortMapping {
    pub remote: u16,
    pub local: u16,
}

fn default_peer_policy_str() -> String {
    "required".to_string()
}

fn default_refresh_interval() -> u64 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeshBundle {
    pub schema: String,
    pub generation: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub services: BTreeMap<String, MeshService>,
    #[serde(default)]
    pub reference_values: BTreeMap<String, Value>,
    #[serde(default)]
    pub rekor_reference_values: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MeshService {
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_ip: Option<String>,
    #[serde(default)]
    pub ports: Vec<u16>,
    #[serde(default)]
    pub connect: Vec<crate::spec::ConnectPort>,
    #[serde(default)]
    pub mcp_ports: Vec<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_public_key: Option<String>,
}

pub fn confidential_ports(ports: &[u16], connect: &[crate::spec::ConnectPort]) -> Vec<u16> {
    let connect = connect.iter().map(|cp| cp.port()).collect::<BTreeSet<_>>();
    let mut ports = ports
        .iter()
        .copied()
        .filter(|port| !connect.contains(port))
        .collect::<Vec<_>>();
    ports.sort_unstable();
    ports.dedup();
    ports
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceDirectory {
    pub schema: String,
    #[serde(default)]
    pub services: BTreeMap<String, ServiceDirectoryService>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceDirectoryService {
    #[serde(default)]
    pub ports: Vec<ServiceDirectoryPort>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceDirectoryPort {
    pub address: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppliedResourceState {
    pub sha256: String,
    pub target: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub mode: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mutable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonStatus {
    pub schema: String,
    pub service_id: String,
    pub phase: String,
    pub bootstrap_generation: u64,
    #[serde(default)]
    pub mesh_generation: u64,
    #[serde(default)]
    pub applied_resources: BTreeMap<String, AppliedResourceState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_fingerprint: Option<String>,
    pub app_ready: bool,
    pub mesh_ready: bool,
    pub debug_ssh_ready: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub a2a_peers: BTreeMap<String, DaemonA2aPeerStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonA2aPeerStatus {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fetch_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub ports: Vec<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCard {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, rename = "supportedInterfaces")]
    pub supported_interfaces: Vec<AgentInterface>,
    #[serde(
        default,
        rename = "preferredTransport",
        skip_serializing_if = "Option::is_none"
    )]
    pub preferred_transport: Option<String>,
    #[serde(default)]
    pub skills: Vec<AgentCardSkill>,
    #[serde(default, rename = "defaultInputModes")]
    pub default_input_modes: Vec<String>,
    #[serde(default, rename = "defaultOutputModes")]
    pub default_output_modes: Vec<String>,
    #[serde(default)]
    pub capabilities: AgentCardCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<serde_json::Value>,
    #[serde(
        default,
        rename = "securitySchemes",
        skip_serializing_if = "Option::is_none"
    )]
    pub security_schemes: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security: Vec<serde_json::Value>,
    #[serde(
        default,
        rename = "supportsAuthenticatedExtendedCard",
        skip_serializing_if = "Option::is_none"
    )]
    pub supports_authenticated_extended_card: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<AgentCardSignature>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentInterface {
    pub url: String,
    #[serde(rename = "protocolBinding")]
    pub protocol_binding: String,
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCardSkill {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    #[serde(default, rename = "inputModes", skip_serializing_if = "Vec::is_empty")]
    pub input_modes: Vec<String>,
    #[serde(default, rename = "outputModes", skip_serializing_if = "Vec::is_empty")]
    pub output_modes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCardCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    #[serde(
        default,
        rename = "pushNotifications",
        skip_serializing_if = "Option::is_none"
    )]
    pub push_notifications: Option<bool>,
    #[serde(
        default,
        rename = "stateTransitionHistory",
        skip_serializing_if = "Option::is_none"
    )]
    pub state_transition_history: Option<bool>,
    #[serde(
        default,
        rename = "extendedAgentCard",
        skip_serializing_if = "Option::is_none"
    )]
    pub extended_agent_card: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<AgentExtension>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentExtension {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCardSignature {
    pub protected: String,
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCardConfidential {
    pub id: String,
    #[serde(default = "default_agent_card_cache_ttl", rename = "cacheTtlSec")]
    pub cache_ttl_sec: u64,
    #[serde(rename = "publicIp")]
    pub public_ip: String,
    pub ports: Vec<AgentCardPort>,
    #[serde(
        default,
        rename = "referenceValues",
        skip_serializing_if = "Option::is_none"
    )]
    pub reference_values: Option<Value>,
    pub rekor: AgentCardRekor,
    pub tee: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCardPort {
    pub name: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCardRekor {
    #[serde(rename = "rekorUrl")]
    pub rekor_url: String,
    #[serde(rename = "artifactId")]
    pub artifact_id: String,
    #[serde(rename = "artifactType")]
    pub artifact_type: String,
    #[serde(rename = "artifactVersion")]
    pub artifact_version: String,
    #[serde(rename = "rvName")]
    pub rv_name: String,
}

fn default_agent_card_cache_ttl() -> u64 {
    300
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub fn default_bootstrap_mode() -> String {
    "challenge".to_string()
}

pub fn default_resource_mode() -> String {
    "0600".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidential_ports_excludes_connect_ports() {
        assert_eq!(confidential_ports(&[18789, 18800], &[crate::spec::ConnectPort::Plain(18789)]), vec![18800]);
    }

    #[test]
    fn confidential_ports_returns_empty_when_all_are_connect() {
        assert_eq!(confidential_ports(&[18789], &[crate::spec::ConnectPort::Plain(18789)]), Vec::<u16>::new());
    }

    #[test]
    fn confidential_ports_returns_all_when_no_connect() {
        assert_eq!(confidential_ports(&[3001, 3002], &[]), vec![3001, 3002]);
    }

    #[test]
    fn confidential_ports_deduplicates_and_sorts() {
        assert_eq!(
            confidential_ports(&[18800, 18789, 18800], &[]),
            vec![18789, 18800]
        );
    }

    #[test]
    fn preferred_injection_ip_prefers_public() {
        let state = LocalDeployState {
            provider: "aliyun".to_string(),
            run_id: "run".to_string(),
            resource_name: "res".to_string(),
            terraform_dir: None,
            image_source: None,
            image_import_name: None,
            bucket: None,
            instance_id: None,
            security_group_id: None,
            private_ip: Some("10.0.0.1".to_string()),
            public_ip: Some("1.2.3.4".to_string()),
            tee: "tdx".to_string(),
            published_image_id: None,
            instance_ids: Vec::new(),
            instance_private_ips: Vec::new(),
            slb_vip: None,
        };
        assert_eq!(state.preferred_injection_ip(), Some("1.2.3.4"));
    }

    #[test]
    fn preferred_injection_ip_falls_back_to_private() {
        let state = LocalDeployState {
            provider: "aliyun".to_string(),
            run_id: "run".to_string(),
            resource_name: "res".to_string(),
            terraform_dir: None,
            image_source: None,
            image_import_name: None,
            bucket: None,
            instance_id: None,
            security_group_id: None,
            private_ip: Some("10.0.0.1".to_string()),
            public_ip: None,
            tee: "tdx".to_string(),
            published_image_id: None,
            instance_ids: Vec::new(),
            instance_private_ips: Vec::new(),
            slb_vip: None,
        };
        assert_eq!(state.preferred_injection_ip(), Some("10.0.0.1"));
    }

    #[test]
    fn preferred_injection_ip_skips_empty_public() {
        let state = LocalDeployState {
            provider: "aliyun".to_string(),
            run_id: "run".to_string(),
            resource_name: "res".to_string(),
            terraform_dir: None,
            image_source: None,
            image_import_name: None,
            bucket: None,
            instance_id: None,
            security_group_id: None,
            private_ip: Some("10.0.0.1".to_string()),
            public_ip: Some("  ".to_string()),
            tee: "tdx".to_string(),
            published_image_id: None,
            instance_ids: Vec::new(),
            instance_private_ips: Vec::new(),
            slb_vip: None,
        };
        assert_eq!(state.preferred_injection_ip(), Some("10.0.0.1"));
    }

    #[test]
    fn preferred_injection_ip_none_when_both_absent() {
        let state = LocalDeployState {
            provider: "aliyun".to_string(),
            run_id: "run".to_string(),
            resource_name: "res".to_string(),
            terraform_dir: None,
            image_source: None,
            image_import_name: None,
            bucket: None,
            instance_id: None,
            security_group_id: None,
            private_ip: None,
            public_ip: None,
            tee: "tdx".to_string(),
            published_image_id: None,
            instance_ids: Vec::new(),
            instance_private_ips: Vec::new(),
            slb_vip: None,
        };
        assert_eq!(state.preferred_injection_ip(), None);
    }

    #[test]
    fn private_mesh_ip_skips_empty_whitespace() {
        let state = LocalDeployState {
            provider: "aliyun".to_string(),
            run_id: "run".to_string(),
            resource_name: "res".to_string(),
            terraform_dir: None,
            image_source: None,
            image_import_name: None,
            bucket: None,
            instance_id: None,
            security_group_id: None,
            private_ip: Some("  ".to_string()),
            public_ip: None,
            tee: "tdx".to_string(),
            published_image_id: None,
            instance_ids: Vec::new(),
            instance_private_ips: Vec::new(),
            slb_vip: None,
        };
        assert_eq!(state.private_mesh_ip(), None);
    }

    #[test]
    fn daemon_status_schema_version_is_stable() {
        assert_eq!(
            DAEMON_STATUS_SCHEMA_VERSION,
            "confidential-agent/daemon-status/v1"
        );
    }

    #[test]
    fn default_bootstrap_mode_is_challenge() {
        assert_eq!(default_bootstrap_mode(), "challenge");
    }

    #[test]
    fn default_resource_mode_is_0600() {
        assert_eq!(default_resource_mode(), "0600");
    }

    #[test]
    fn local_deploy_state_round_trips_multi_instance() {
        let state = LocalDeployState {
            provider: "aliyun".to_string(),
            run_id: "run".to_string(),
            resource_name: "res".to_string(),
            terraform_dir: None,
            image_source: None,
            image_import_name: None,
            bucket: None,
            instance_id: Some("i-aaa".to_string()),
            security_group_id: None,
            private_ip: Some("10.0.1.1".to_string()),
            public_ip: None,
            tee: "tdx".to_string(),
            published_image_id: None,
            instance_ids: vec!["i-aaa".to_string(), "i-bbb".to_string()],
            instance_private_ips: vec!["10.0.1.1".to_string(), "10.0.1.2".to_string()],
            slb_vip: Some("10.0.1.100".to_string()),
        };

        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: LocalDeployState = serde_json::from_str(&encoded).unwrap();

        assert_eq!(
            decoded.instance_ids,
            vec!["i-aaa".to_string(), "i-bbb".to_string()]
        );
        assert_eq!(
            decoded.instance_private_ips,
            vec!["10.0.1.1".to_string(), "10.0.1.2".to_string()]
        );
        assert_eq!(decoded.slb_vip, Some("10.0.1.100".to_string()));
    }

    #[test]
    fn local_deploy_state_omits_empty_multi_instance_fields() {
        let state = LocalDeployState {
            provider: "aliyun".to_string(),
            run_id: "run".to_string(),
            resource_name: "res".to_string(),
            terraform_dir: None,
            image_source: None,
            image_import_name: None,
            bucket: None,
            instance_id: None,
            security_group_id: None,
            private_ip: None,
            public_ip: None,
            tee: "tdx".to_string(),
            published_image_id: None,
            instance_ids: Vec::new(),
            instance_private_ips: Vec::new(),
            slb_vip: None,
        };

        let encoded = serde_json::to_string(&state).unwrap();

        assert!(!encoded.contains("instance_ids"));
        assert!(!encoded.contains("instance_private_ips"));
        assert!(!encoded.contains("slb_vip"));
    }

    #[test]
    fn local_deploy_state_deserializes_without_multi_instance_fields() {
        let json =
            r#"{"provider":"aliyun","run_id":"run","resource_name":"res","tee":"tdx"}"#;
        let state: LocalDeployState = serde_json::from_str(json).unwrap();

        assert!(state.instance_ids.is_empty());
        assert!(state.instance_private_ips.is_empty());
        assert!(state.slb_vip.is_none());
    }
}
