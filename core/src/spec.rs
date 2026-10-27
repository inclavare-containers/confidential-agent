pub use crate::peerings::{ipv4_cidr_contains, validate_ipv4_cidr};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// CA's system disk size when an AppSpec does not choose one explicitly.
pub const DEFAULT_DEPLOY_DISK_GB: u32 = 32;

pub const SCHEMA_VERSION: &str = "confidential-agent/v1";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    pub schema: String,
    pub service: ServiceSpec,
    #[serde(default)]
    pub build: BuildSpec,
    pub deploy: DeploySpec,
    pub attestation: AttestationSpec,
    #[serde(default)]
    pub secrets: SecretsSpec,
    pub resources: BTreeMap<String, ResourceSpec>,
    #[serde(default)]
    pub a2a: Option<A2aSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceSpec {
    pub id: String,
    pub ports: Vec<u16>,
    #[serde(default)]
    pub connect: Vec<ConnectPort>,
    #[serde(default)]
    pub mcp_ports: Vec<u16>,
    #[serde(default)]
    pub app_service: Option<String>,
    /// Deliver instance SSH credentials through the attested resource flow.
    /// The image must provide a service using these credentials.
    #[serde(default)]
    pub ssh: bool,
}

/// Per-port connect configuration. Supports two YAML forms:
/// - `8080` (plain integer, defaults to `rats-tls` encryption)
/// - `{port: 8080, encryption: ohttp}` (explicit per-port config)
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ConnectPort {
    Plain(u16),
    Config {
        port: u16,
        #[serde(default = "default_encryption")]
        encryption: EncryptionMode,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum EncryptionMode {
    RatsTls,
    Ohttp,
}

fn default_encryption() -> EncryptionMode {
    EncryptionMode::RatsTls
}

impl ConnectPort {
    pub fn port(&self) -> u16 {
        match self {
            ConnectPort::Plain(port) | ConnectPort::Config { port, .. } => *port,
        }
    }

    pub fn encryption(&self) -> EncryptionMode {
        match self {
            ConnectPort::Plain(_) => EncryptionMode::RatsTls,
            ConnectPort::Config { encryption, .. } => encryption.clone(),
        }
    }
}

impl Default for EncryptionMode {
    fn default() -> Self {
        EncryptionMode::RatsTls
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildSpec {
    pub image_name: String,
    /// Image-owned runtime requirements, captured at build time and signed on export.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_contract: Option<PathBuf>,
    #[serde(default)]
    pub kernel_cmdline_append: Option<String>,
    #[serde(default)]
    pub resize: Option<String>,
    #[serde(default)]
    pub container: Option<BuildContainerSpec>,
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<BuildRepositorySpec>,
    #[serde(default)]
    pub files: Vec<BuildFileSpec>,
    #[serde(default)]
    pub scripts: Vec<PathBuf>,
    #[serde(default)]
    pub variants: BuildVariantsSpec,
    #[serde(default, skip_serializing_if = "BuildCleanupSpec::is_empty")]
    pub cleanup: BuildCleanupSpec,
}

/// Additional signed RPM repositories passed to Shelter's native build input.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildRepositorySpec {
    pub name: String,
    pub baseurl: String,
    pub gpgkey: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BuildCleanupSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove_static_libs: Option<bool>,
}

impl BuildCleanupSpec {
    pub fn is_empty(&self) -> bool {
        self.remove_static_libs.is_none()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BuildContainerSpec {
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub archive: Option<PathBuf>,
    #[serde(default)]
    pub mode: ContainerMode,
    #[serde(default)]
    pub runtime: ContainerRuntime,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub pull: ContainerPullPolicy,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub network: Option<bool>,
    #[serde(default)]
    pub mounts: Vec<ContainerMountSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContainerMountSpec {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerMode {
    #[default]
    Runtime,
    Rootfs,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerRuntime {
    #[default]
    Containerd,
    Podman,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerPullPolicy {
    #[default]
    Missing,
    Always,
    Never,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildFileSpec {
    pub source: PathBuf,
    pub target: String,
    #[serde(default)]
    pub executable: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildVariantsSpec {
    #[serde(default = "default_enabled_variant")]
    pub release: BuildVariantSpec,
    #[serde(default)]
    pub debug: Option<BuildVariantSpec>,
}

impl Default for BuildVariantsSpec {
    fn default() -> Self {
        Self {
            release: default_enabled_variant(),
            debug: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildVariantSpec {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub ssh_public_key: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeploySpec {
    pub provider: DeployProvider,
    /// A publisher-authenticated cloud image; no local build is needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<PublishedImageSpec>,
    pub instance_type: String,
    #[serde(default)]
    pub image_variant: Option<String>,
    #[serde(default)]
    pub disk_gb: Option<u32>,
    #[serde(default)]
    pub private_ip: Option<String>,
    pub region: String,
    #[serde(alias = "availability_zone")]
    pub zone_id: String,
    #[serde(default)]
    pub vpc_id: Option<String>,
    #[serde(default)]
    pub vswitch_id: Option<String>,
    #[serde(default)]
    pub security_group_id: Option<String>,
    #[serde(default = "default_replica_count")]
    pub replicas: u32,
    #[serde(default)]
    pub slb: Option<SlbSpec>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SlbSpec {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_slb_type")]
    pub slb_type: SlbType,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SlbType {
    #[default]
    Nlb,
    Alb,
}

fn default_replica_count() -> u32 {
    1
}

fn default_slb_type() -> SlbType {
    SlbType::Nlb
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedImageSpec {
    pub release: PathBuf,
    #[serde(default)]
    pub trusted_key: PathBuf,
    /// Optional publisher manifest pin; otherwise use the authenticated release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
    /// Legacy state compatibility only; never used for consumer authorization.
    #[serde(
        default,
        rename = "account_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub legacy_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_id: Option<String>,
    /// Explicit opt-out for release signatures, never runtime attestation.
    #[serde(default, skip_serializing_if = "is_false")]
    pub skip_signature_verification: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DeployProvider {
    Aliyun,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttestationSpec {
    pub tee: AttestationTee,
    pub mode: AttestationMode,
    #[serde(default)]
    pub reference_values: ReferenceValueMode,
    #[serde(default)]
    pub rekor: Option<RekorSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RekorSpec {
    #[serde(default)]
    pub artifact_id: Option<String>,
    #[serde(default = "default_rekor_artifact_type")]
    pub artifact_type: String,
    #[serde(default)]
    pub artifact_version: Option<String>,
    #[serde(default = "default_rekor_url")]
    pub rekor_url: String,
    #[serde(default)]
    pub cosign_key: Option<PathBuf>,
    #[serde(default = "default_slsa_generator")]
    pub slsa_generator: PathBuf,
    #[serde(default)]
    pub rv_name: Option<String>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AttestationMode {
    Challenge,
    Trustee,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AttestationTee {
    Tdx,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReferenceValueMode {
    Sample,
    Rekor,
}

impl Default for ReferenceValueMode {
    fn default() -> Self {
        Self::Sample
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsSpec {
    #[serde(default)]
    pub disk_passphrase: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSpec {
    pub source: PathBuf,
    pub target: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default = "default_true")]
    pub required: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mutable: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct A2aSpec {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub provider: Option<serde_json::Value>,
    #[serde(default = "default_a2a_cache_ttl_sec", rename = "cacheTtlSec")]
    pub cache_ttl_sec: u64,
    #[serde(default)]
    pub interfaces: Vec<A2aInterfaceSpec>,
    #[serde(default)]
    pub signing: A2aSigningSpec,
    #[serde(default)]
    pub skills: Vec<A2aSkillSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct A2aInterfaceSpec {
    #[serde(default = "default_a2a_protocol_binding")]
    pub protocol_binding: String,
    pub port: u16,
    #[serde(default = "default_a2a_interface_path")]
    pub path: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct A2aSigningSpec {
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub expected_issuer: Option<String>,
    #[serde(default)]
    pub expected_subject: Option<String>,
    #[serde(default)]
    pub oidc_issuer: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct A2aSkillSpec {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub examples: Vec<String>,
    #[serde(default)]
    pub input_modes: Vec<String>,
    #[serde(default)]
    pub output_modes: Vec<String>,
}

impl AgentSpec {
    pub fn from_path(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read spec '{}'", path.display()))?;
        let base_dir = absolute_base_dir(path.parent().unwrap_or_else(|| Path::new(".")))?;
        Self::from_yaml(&content, &base_dir)
    }

    pub fn from_yaml(content: &str, base_dir: &Path) -> Result<Self> {
        let mut spec: Self = serde_yaml::from_str(content).context("failed to parse agent spec")?;
        spec.resolve_paths_against(base_dir);
        spec.validate()?;
        Ok(spec)
    }

    pub fn image_id(&self) -> &str {
        self.deploy
            .image
            .as_ref()
            .map(|image| image.manifest.as_deref().unwrap_or("published-image"))
            .unwrap_or(&self.build.image_name)
    }

    pub fn image_variant(&self) -> &str {
        self.deploy.image_variant.as_deref().unwrap_or("release")
    }

    pub fn deploys_debug_image(&self) -> bool {
        self.image_variant() == "debug"
    }

    /// Freeze a provider disk size that can contain the configured image.
    pub fn deployment_disk_gb(&self) -> Result<u32> {
        let image_minimum = self
            .build
            .resize
            .as_deref()
            .map(configured_image_disk_gb)
            .transpose()?
            .unwrap_or(0);
        if let Some(size) = self.deploy.disk_gb {
            anyhow::ensure!(size >= 20, "deploy.disk_gb must be at least 20 GiB");
            anyhow::ensure!(size >= image_minimum,
                "deploy.disk_gb ({size} GiB) is smaller than build.resize ({image_minimum} GiB rounded up)");
            Ok(size)
        } else {
            Ok(DEFAULT_DEPLOY_DISK_GB.max(image_minimum))
        }
    }

    pub fn ensure_mvp_supported(&self) -> Result<()> {
        Ok(())
    }

    fn resolve_paths_against(&mut self, base_dir: &Path) {
        if let Some(path) = &mut self.build.runtime_contract {
            resolve_pathbuf(path, base_dir);
        }
        if let Some(image) = &mut self.deploy.image {
            if !image.release.as_os_str().is_empty() {
                resolve_pathbuf(&mut image.release, base_dir);
            }
            if !image.trusted_key.as_os_str().is_empty() {
                resolve_pathbuf(&mut image.trusted_key, base_dir);
            }
        }
        for path in &mut self.build.scripts {
            resolve_pathbuf(path, base_dir);
        }
        for file in &mut self.build.files {
            resolve_pathbuf(&mut file.source, base_dir);
        }
        if let Some(container) = &mut self.build.container {
            if let Some(archive) = &mut container.archive {
                resolve_pathbuf(archive, base_dir);
            }
        }
        if let Some(debug) = &mut self.build.variants.debug {
            if let Some(path) = &mut debug.ssh_public_key {
                resolve_pathbuf(path, base_dir);
            }
        }
        if let Some(path) = &mut self.secrets.disk_passphrase {
            resolve_pathbuf(path, base_dir);
        }
        for resource in self.resources.values_mut() {
            resolve_pathbuf(&mut resource.source, base_dir);
        }
        if let Some(rekor) = &mut self.attestation.rekor {
            if let Some(path) = &mut rekor.cosign_key {
                resolve_pathbuf(path, base_dir);
            }
            resolve_pathbuf(&mut rekor.slsa_generator, base_dir);
        }
    }

    fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA_VERSION {
            bail!(
                "unsupported schema '{}'; expected '{}'",
                self.schema,
                SCHEMA_VERSION
            );
        }
        validate_id("service.id", &self.service.id)?;
        self.deployment_disk_gb()?;
        if self.attestation.mode == AttestationMode::Trustee {
            crate::trustee::validate_trustee_service_id(&self.service.id)?;
        }
        if self
            .service
            .app_service
            .as_deref()
            .is_some_and(|service| service.trim().is_empty())
        {
            bail!("service.app_service must not be empty when set");
        }
        if self.service.ssh && self.deploys_debug_image() {
            bail!("service.ssh requires a release image; debug images use their existing SSH mechanism");
        }
        if let Some(image) = &self.deploy.image {
            if image.release.as_os_str().is_empty()
                || (!image.skip_signature_verification && image.trusted_key.as_os_str().is_empty())
            {
                bail!("deploy.image requires release and a trusted_key unless skip_signature_verification is explicit");
            }
            if let Some(manifest) = &image.manifest {
                let digest = manifest.strip_prefix("sha256:").unwrap_or_default();
                if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    bail!("deploy.image.manifest must be a full sha256: manifest identity");
                }
            }
            if let Some(id) = &image.image_id {
                if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                    bail!("deploy.image.image_id must be a cloud image ID");
                }
            }
            if !self.build.image_name.is_empty()
                || !self.build.packages.is_empty()
                || !self.build.files.is_empty()
                || !self.build.scripts.is_empty()
                || !self.build.repos.is_empty()
                || self.build.container.is_some()
                || self.build.resize.is_some()
                || self.build.runtime_contract.is_some()
                || self.build.kernel_cmdline_append.is_some()
                || self.build.cleanup != BuildCleanupSpec::default()
                || self.build.variants.debug.is_some()
            {
                bail!("deploy.image reuses a published image; omit build inputs");
            }
        } else {
            validate_id("build.image_name", &self.build.image_name)?;
        }
        validate_ports("service.ports", &self.service.ports)?;
        validate_connect_ports(&self.service.ports, &self.service.connect)?;
        validate_mcp_ports(
            &self.service.ports,
            &self.service.connect,
            &self.service.mcp_ports,
        )?;
        validate_build_packages(&self.build)?;
        let mut repo_names = BTreeSet::new();
        for repo in &self.build.repos {
            validate_id("build.repos.name", &repo.name)?;
            validate_https_url("build.repos.baseurl", &repo.baseurl)?;
            validate_https_url("build.repos.gpgkey", &repo.gpgkey)?;
            if !repo_names.insert(&repo.name) {
                bail!("duplicate build.repos.name: {}", repo.name);
            }
        }
        if let Some(container) = &self.build.container {
            validate_build_container(container)?;
        }
        if self.deploy.instance_type.trim().is_empty() {
            bail!("deploy.instance_type must not be empty");
        }
        if self.deploy.region.trim().is_empty() {
            bail!("deploy.region must not be empty");
        }
        if self.deploy.zone_id.trim().is_empty() {
            bail!("deploy.zone_id must not be empty");
        }
        if self.deploy.replicas == 0 {
            bail!("deploy.replicas must be greater than 0");
        }
        if self.deploy.replicas > 1 && self.deploy.slb.as_ref().map_or(true, |s| !s.enabled) {
            bail!("deploy.replicas > 1 requires deploy.slb.enabled = true (TNG has no client-side load balancing)");
        }
        if self.deploy.replicas > 1 {
            anyhow::ensure!(
                self.service
                    .connect
                    .iter()
                    .filter(|p| p.encryption() == EncryptionMode::Ohttp)
                    .count()
                    <= 1,
                "service replicas currently support one OHTTP connect port per service"
            );
        }
        if let Some(slb) = &self.deploy.slb {
            if slb.enabled {
                anyhow::ensure!(
                    self.deploy.replicas > 1,
                    "an enabled SLB requires replicas > 1"
                );
                anyhow::ensure!(
                    !self.service.connect.is_empty(),
                    "SLB requires at least one protected connect port"
                );
            }
            if slb.enabled && slb.slb_type == SlbType::Alb {
                let has_rats_tls = self
                    .service
                    .connect
                    .iter()
                    .any(|cp| cp.encryption() == EncryptionMode::RatsTls);
                if has_rats_tls {
                    bail!("deploy.slb.slb_type alb (L7) is incompatible with RATS-TLS (L4 TLS); use slb_type: nlb or switch to OHTTP encryption");
                }
            }
        }
        validate_variant("release", &self.build.variants.release)?;
        if let Some(debug) = &self.build.variants.debug {
            validate_variant("debug", debug)?;
        }
        for (index, file) in self.build.files.iter().enumerate() {
            if file.source.as_os_str().is_empty() {
                bail!("build.files[{index}].source must not be empty");
            }
            if !file.target.starts_with('/') {
                bail!("build.files[{index}].target must be an absolute path");
            }
        }
        match self.image_variant() {
            "release" if self.deploy.image.is_none() && !self.build.variants.release.enabled => {
                bail!("deploy.image_variant=release requires build.variants.release.enabled=true")
            }
            "debug" if self.deploy.image.is_none() => {
                let Some(debug) = &self.build.variants.debug else {
                    bail!("deploy.image_variant=debug requires build.variants.debug");
                };
                if !debug.enabled {
                    bail!("deploy.image_variant=debug requires build.variants.debug.enabled=true");
                }
            }
            "release" | "debug" => {}
            other => bail!("deploy.image_variant '{}' is not supported", other),
        }
        for (name, resource) in &self.resources {
            validate_id("resources key", name)?;
            anyhow::ensure!(
                !matches!(name.as_str(), "disk_passphrase" | "data_passphrase"
                    | "cagent_bootstrap_config" | "cagent_mesh_bundle"
                    | "cagent_a2a_bundle" | "cagent_reference_value_list"),
                "resources.{name} is reserved for CA platform delivery; application resources cannot overwrite instance secrets or control bundles"
            );
            if resource.source.as_os_str().is_empty() {
                bail!("resources.{}.source must not be empty", name);
            }
            if !resource.target.starts_with('/') {
                bail!("resources.{}.target must be an absolute path", name);
            }
        }
        if let Some(rekor) = &self.attestation.rekor {
            if rekor.artifact_type.trim().is_empty() {
                bail!("attestation.rekor.artifact_type must not be empty");
            }
            if rekor.rekor_url.trim().is_empty() {
                bail!("attestation.rekor.rekor_url must not be empty");
            }
            if rekor.required && rekor.cosign_key.is_none() {
                bail!(
                    "attestation.rekor.cosign_key is required when attestation.rekor.required=true"
                );
            }
        }
        if self.attestation.reference_values == ReferenceValueMode::Rekor
            && self.attestation.rekor.is_none()
        {
            bail!("attestation.rekor is required when attestation.reference_values=rekor");
        }
        if let Some(a2a) = &self.a2a {
            validate_id("a2a.id", &a2a.id)?;
            if a2a.name.trim().is_empty() {
                bail!("a2a.name must not be empty");
            }
            if a2a.cache_ttl_sec == 0 {
                bail!("a2a.cacheTtlSec must be greater than 0");
            }
            for (idx, interface) in a2a.interfaces.iter().enumerate() {
                if interface.protocol_binding.trim().is_empty() {
                    bail!("a2a.interfaces[{idx}].protocol_binding must not be empty");
                }
                if interface.port == 0 {
                    bail!("a2a.interfaces[{idx}].port must be greater than 0");
                }
                if !self
                    .service
                    .connect
                    .iter()
                    .any(|cp| cp.port() == interface.port)
                {
                    bail!("a2a.interfaces[{idx}].port must be listed in service.connect");
                }
                if !interface.path.starts_with('/') {
                    bail!("a2a.interfaces[{idx}].path must start with '/'");
                }
            }
            if let Some(issuer) = a2a.signing.expected_issuer.as_deref() {
                validate_https_url("a2a.signing.expected_issuer", issuer)?;
            }
            if let Some(issuer) = a2a.signing.oidc_issuer.as_deref() {
                validate_https_url("a2a.signing.oidc_issuer", issuer)?;
            }
            if a2a.signing.required {
                if a2a.signing.mode.as_deref() != Some("sigstore-keyless") {
                    bail!("a2a.signing.mode must be sigstore-keyless when required=true");
                }
                if a2a
                    .signing
                    .expected_issuer
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or("")
                    .is_empty()
                {
                    bail!("a2a.signing.expected_issuer is required when signing.required=true");
                }
                if a2a
                    .signing
                    .expected_subject
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or("")
                    .is_empty()
                {
                    bail!("a2a.signing.expected_subject is required when signing.required=true");
                }
            }
            for (idx, skill) in a2a.skills.iter().enumerate() {
                validate_id(&format!("a2a.skills[{idx}].id"), &skill.id)?;
                if skill.name.trim().is_empty() {
                    bail!("a2a.skills[{idx}].name must not be empty");
                }
                for (tag_idx, tag) in skill.tags.iter().enumerate() {
                    if tag.trim().is_empty() {
                        bail!("a2a.skills[{idx}].tags[{tag_idx}] must not be empty");
                    }
                }
            }
        }
        Ok(())
    }
}

fn validate_ports(field: &str, ports: &[u16]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for port in ports {
        if *port == 0 {
            bail!("{field} must contain ports greater than 0");
        }
        if !seen.insert(*port) {
            bail!("{field} contains duplicate port {port}");
        }
    }
    Ok(())
}

fn validate_connect_ports(ports: &[u16], connect: &[ConnectPort]) -> Result<()> {
    let allowed = ports.iter().copied().collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for cp in connect {
        let port = cp.port();
        if port == 0 {
            bail!("service.connect must contain ports greater than 0");
        }
        if !allowed.contains(&port) {
            bail!("service.connect port {port} must be listed in service.ports");
        }
        if !seen.insert(port) {
            bail!("service.connect contains duplicate port {port}");
        }
    }
    Ok(())
}

fn validate_mcp_ports(ports: &[u16], connect: &[ConnectPort], mcp_ports: &[u16]) -> Result<()> {
    let allowed = ports.iter().copied().collect::<BTreeSet<_>>();
    let connect = connect.iter().map(|cp| cp.port()).collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for port in mcp_ports {
        if *port == 0 {
            bail!("service.mcp_ports must contain ports greater than 0");
        }
        if !allowed.contains(port) {
            bail!("service.mcp_ports port {port} must be listed in service.ports");
        }
        if connect.contains(port) {
            bail!("service.mcp_ports port {port} must not be listed in service.connect");
        }
        if !seen.insert(*port) {
            bail!("service.mcp_ports contains duplicate port {port}");
        }
    }
    Ok(())
}

fn validate_build_packages(build: &BuildSpec) -> Result<()> {
    for (index, package) in build.packages.iter().enumerate() {
        let package = package.trim();
        if package.is_empty() {
            bail!("build.packages[{index}] must not be empty");
        }
        if let Some(replacement) = alinux_package_substitution(package) {
            bail!(
                "build.packages[{index}]='{package}' uses a Debian/Ubuntu package name; default mkosi builds use Alinux/RHEL dnf packages, so use {replacement} instead"
            );
        }
    }
    Ok(())
}

fn validate_build_container(container: &BuildContainerSpec) -> Result<()> {
    if container
        .image
        .as_deref()
        .is_some_and(|image| image.trim().is_empty())
    {
        bail!("build.container.image must not be empty when set");
    }
    if container.image.is_none() && container.archive.is_none() {
        bail!("build.container.image or build.container.archive is required");
    }
    if container.archive.is_some() && container.pull != ContainerPullPolicy::Missing {
        bail!("build.container.pull is only valid when build.container.archive is not set");
    }
    if let Some(name) = container.name.as_deref() {
        if name.trim().is_empty() {
            bail!("build.container.name must not be empty");
        }
        if name.len() > 64 {
            bail!("build.container.name must be at most 64 bytes");
        }
        if !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        {
            bail!("build.container.name may only contain ASCII letters, digits, '.', '_' and '-'");
        }
    }
    for key in container.env.keys() {
        if key.trim().is_empty()
            || key.chars().next().is_some_and(|ch| ch.is_ascii_digit())
            || !key
                .chars()
                .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        {
            bail!(
                "build.container.env key '{key}' must be a valid shell environment variable name"
            );
        }
    }
    if let Some(working_dir) = container.working_dir.as_deref() {
        if !working_dir.starts_with('/') {
            bail!("build.container.working_dir must be an absolute path");
        }
    }
    if container.mode == ContainerMode::Rootfs && !container.mounts.is_empty() {
        bail!("build.container.mounts is only supported with build.container.mode: runtime");
    }
    let mut mount_targets = BTreeSet::new();
    for (index, mount) in container.mounts.iter().enumerate() {
        validate_container_mount_path(
            &format!("build.container.mounts[{index}].source"),
            &mount.source,
        )?;
        validate_container_mount_path(
            &format!("build.container.mounts[{index}].target"),
            &mount.target,
        )?;
        if mount.source.contains([':', ',']) || mount.target.contains([':', ',']) {
            bail!(
                "build.container.mounts[{index}] paths must not contain ':' or ',' because runtime mount syntax is delimiter-based"
            );
        }
        let target_path = comparable_mount_path(&mount.target);
        if !mount_targets.insert(target_path) {
            bail!("build.container.mounts[{index}].target duplicates an earlier mount target");
        }
    }
    Ok(())
}

fn comparable_mount_path(value: &str) -> &str {
    let trimmed = value.trim_end_matches('/');
    if trimmed.is_empty() {
        "/"
    } else {
        trimmed
    }
}

fn validate_container_mount_path(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must not be empty");
    }
    if value.bytes().any(|byte| byte < 0x20) {
        bail!("{field} must not contain control characters");
    }
    let path = Path::new(value);
    if !path.is_absolute() {
        bail!("{field} must be an absolute guest path");
    }
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::CurDir
                | std::path::Component::Prefix(_)
        )
    }) {
        bail!("{field} must not contain '.', '..', or platform prefixes");
    }
    Ok(())
}

fn alinux_package_substitution(package: &str) -> Option<&'static str> {
    match package {
        "build-essential" => Some("'gcc', 'gcc-c++', and 'make'"),
        "python3-dev" => Some("'python3-devel' or a versioned devel package such as 'python3.11-devel'"),
        "python3-venv" => Some("the matching Alinux Python runtime package; remove this package unless a custom repository provides it"),
        "python-dev" => Some("'python3-devel' or a versioned devel package such as 'python3.11-devel'"),
        "python-dev-is-python3" => Some("'python3-devel' or a versioned devel package such as 'python3.11-devel'"),
        "libc-dev" => Some("'glibc-devel'"),
        "libc6-dev" => Some("'glibc-devel'"),
        "libffi-dev" => Some("'libffi-devel'"),
        "libssl-dev" => Some("'openssl-devel'"),
        "openssh-client" => Some("'openssh-clients'"),
        "procps" => Some("'procps-ng'"),
        "xz-utils" => Some("'xz'"),
        "docker-cli" => Some("'podman' only if the workload actually needs a container runtime; otherwise remove it"),
        "docker.io" => Some("'podman' only if the workload actually needs a container runtime; otherwise remove it"),
        "ffmpeg" => Some("a repository that provides ffmpeg; otherwise remove it from build.packages"),
        _ => None,
    }
}

fn validate_variant(name: &str, variant: &BuildVariantSpec) -> Result<()> {
    if name == "release" && variant.ssh_public_key.is_some() {
        bail!("build.variants.release.ssh_public_key is not allowed; release image must not enable SSH");
    }
    Ok(())
}

fn resolve_pathbuf(path: &mut PathBuf, base_dir: &Path) {
    if !path.is_absolute() {
        *path = base_dir.join(&*path);
    }
    *path = normalize_path(path);
}

fn validate_https_url(field: &str, value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{field} must not be empty when set");
    }
    if value.chars().any(char::is_whitespace) {
        bail!("{field} must not contain whitespace");
    }
    let Some(rest) = value.strip_prefix("https://") else {
        bail!("{field} must be an https URL");
    };
    let authority = rest
        .split_once('/')
        .map(|(authority, _)| authority)
        .unwrap_or(rest);
    if authority.is_empty() || authority.contains('@') {
        bail!("{field} must include a plain host");
    }
    if value.contains('?') || value.contains('#') {
        bail!("{field} must not include query or fragment");
    }
    Ok(())
}

// Match Shelter's V1 image sizing: short K/M/G/T units are decimal; complete
// IEC units are binary. Round upward only when converting bytes to cloud GiB.
fn configured_image_disk_gb(input: &str) -> Result<u32> {
    let value = input.trim();
    let split = value
        .find(|character: char| !(character.is_ascii_digit() || character == '.'))
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let multiplier: u128 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1_000,
        "m" | "mb" => 1_000_000,
        "g" | "gb" => 1_000_000_000,
        "t" | "tb" => 1_000_000_000_000,
        "ki" | "kib" => 1_024,
        "mi" | "mib" => 1_024_u128.pow(2),
        "gi" | "gib" => 1_024_u128.pow(3),
        "ti" | "tib" => 1_024_u128.pow(4),
        _ => bail!("build.resize has an unsupported size unit"),
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    anyhow::ensure!(
        !(whole.is_empty() && fraction.is_empty())
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.bytes().all(|byte| byte.is_ascii_digit()),
        "build.resize must contain a valid nonnegative decimal size"
    );
    let numerator = format!("{whole}{fraction}")
        .parse::<u128>()
        .context("build.resize is too large")?;
    let denominator = 10_u128
        .checked_pow(
            fraction
                .len()
                .try_into()
                .context("build.resize precision is too large")?,
        )
        .context("build.resize precision is too large")?;
    let bytes = numerator
        .checked_mul(multiplier)
        .context("build.resize is too large")?
        / denominator;
    anyhow::ensure!(bytes > 0, "build.resize must be greater than zero");
    let gib = 1_024_u128.pow(3);
    let size = bytes
        .checked_add(gib - 1)
        .context("build.resize is too large")?
        / gib;
    size.try_into()
        .context("build.resize exceeds supported cloud disk size")
}

fn absolute_base_dir(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .context("failed to resolve current working directory")?
            .join(path))
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn validate_id(field: &str, value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{field} must not be empty");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        bail!("{field} may only contain letters, numbers, underscores, and hyphens");
    }
    Ok(())
}

fn default_rekor_artifact_type() -> String {
    "uki".to_string()
}

fn default_rekor_url() -> String {
    "https://rekor.sigstore.dev".to_string()
}

fn default_a2a_cache_ttl_sec() -> u64 {
    300
}

fn default_a2a_protocol_binding() -> String {
    "JSONRPC".to_string()
}

fn default_a2a_interface_path() -> String {
    "/a2a".to_string()
}

fn default_slsa_generator() -> PathBuf {
    PathBuf::from("tools/slsa/slsa-generator")
}

fn default_true() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn default_enabled_variant() -> BuildVariantSpec {
    BuildVariantSpec {
        enabled: true,
        ssh_public_key: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn published_spec() -> serde_yaml::Value {
        let mut value: serde_yaml::Value = serde_yaml::from_str(SPEC).unwrap();
        value
            .as_mapping_mut()
            .unwrap()
            .remove(serde_yaml::Value::from("build"));
        value["deploy"]["image"] = serde_yaml::from_str(&format!(
            "release: ./image.json\ntrusted_key: ./publisher.pem\nmanifest: sha256:{}\naccount_id: '1234567890'\n",
            "a".repeat(64)
        )).unwrap();
        value
    }

    #[test]
    fn application_resources_cannot_overwrite_platform_delivery_slots() {
        let yaml = "schema: confidential-agent/v1\nservice: {id: app, ports: []}\nbuild: {image_name: app}\ndeploy: {provider: aliyun, region: cn-beijing, zone_id: cn-beijing-i, instance_type: ecs.g9i.xlarge}\nattestation: {tee: tdx, mode: challenge, reference_values: sample}\nresources: {}";
        let mut spec = AgentSpec::from_yaml(yaml, Path::new("/project")).unwrap();
        for id in [
            "disk_passphrase",
            "data_passphrase",
            "cagent_bootstrap_config",
            "cagent_mesh_bundle",
            "cagent_a2a_bundle",
            "cagent_reference_value_list",
        ] {
            spec.resources.clear();
            spec.resources.insert(
                id.into(),
                ResourceSpec {
                    source: PathBuf::from("/project/token"),
                    target: "/opt/data/token".into(),
                    owner: None,
                    group: None,
                    mode: None,
                    required: true,
                    mutable: false,
                },
            );
            assert!(spec
                .validate()
                .unwrap_err()
                .to_string()
                .contains("reserved for CA platform delivery"));
        }
        spec.resources.clear();
        spec.resources.insert(
            "api_token".into(),
            ResourceSpec {
                source: PathBuf::from("/project/token"),
                target: "/opt/data/token".into(),
                owner: None,
                group: None,
                mode: None,
                required: true,
                mutable: false,
            },
        );
        spec.validate().unwrap();
    }

    #[test]
    fn published_image_manifest_pin_is_optional_but_explicit_values_are_validated() {
        let mut value = published_spec();
        value["deploy"]["image"]
            .as_mapping_mut()
            .unwrap()
            .remove(serde_yaml::Value::from("manifest"));
        let spec = AgentSpec::from_yaml(
            &serde_yaml::to_string(&value).unwrap(),
            Path::new("/project"),
        )
        .unwrap();
        assert!(spec.deploy.image.as_ref().unwrap().manifest.is_none());
        assert_eq!(spec.image_id(), "published-image");
        assert!(serde_json::to_value(&spec.deploy.image)
            .unwrap()
            .get("manifest")
            .is_none());
        value["deploy"]["image"]["manifest"] = "".into();
        assert!(AgentSpec::from_yaml(
            &serde_yaml::to_string(&value).unwrap(),
            Path::new("/project"),
        )
        .is_err());
    }

    #[test]
    fn published_image_needs_no_local_build_and_resolves_public_metadata_paths() {
        let spec = AgentSpec::from_yaml(
            &serde_yaml::to_string(&published_spec()).unwrap(),
            Path::new("/project"),
        )
        .unwrap();
        assert!(spec.build.image_name.is_empty());
        let image = spec.deploy.image.unwrap();
        assert_eq!(image.release, Path::new("/project/image.json"));
        assert_eq!(image.trusted_key, Path::new("/project/publisher.pem"));
    }

    #[test]
    fn published_image_rejects_ambiguous_builds_and_invalid_identity() {
        for field in ["build", "manifest", "image_id", "release", "variant"] {
            let mut value = published_spec();
            match field {
                "build" => {
                    value["build"] = serde_yaml::from_str("image_name: another-image").unwrap()
                }
                "manifest" => value["deploy"]["image"]["manifest"] = "sha256:123".into(),
                "image_id" => value["deploy"]["image"]["image_id"] = "bad image!".into(),
                "release" => value["deploy"]["image"]["release"] = "".into(),
                _ => value["deploy"]["image_variant"] = "unknown".into(),
            }
            assert!(
                AgentSpec::from_yaml(
                    &serde_yaml::to_string(&value).unwrap(),
                    Path::new("/project")
                )
                .is_err(),
                "accepted {field}"
            );
        }
        let mut value = published_spec();
        value["deploy"]
            .as_mapping_mut()
            .unwrap()
            .remove(serde_yaml::Value::from("image"));
        assert!(AgentSpec::from_yaml(
            &serde_yaml::to_string(&value).unwrap(),
            Path::new("/project")
        )
        .is_err());
    }

    #[test]
    fn published_debug_image_needs_no_local_build_or_private_ssh_key() {
        let mut value = published_spec();
        value["deploy"]["image_variant"] = "debug".into();
        let spec = AgentSpec::from_yaml(
            &serde_yaml::to_string(&value).unwrap(),
            Path::new("/project"),
        )
        .unwrap();
        assert!(spec.deploys_debug_image());
        assert!(spec.build.variants.debug.is_none());
        assert!(!spec.service.ssh);
        value["service"]["ssh"] = true.into();
        assert!(AgentSpec::from_yaml(
            &serde_yaml::to_string(&value).unwrap(),
            Path::new("/project"),
        )
        .unwrap_err()
        .to_string()
        .contains("service.ssh requires a release image"));
        value["service"]["ssh"] = false.into();
        value["build"] =
            serde_yaml::from_str("image_name: local-debug\nvariants: {debug: {enabled: true}}")
                .unwrap();
        assert!(AgentSpec::from_yaml(
            &serde_yaml::to_string(&value).unwrap(),
            Path::new("/project"),
        )
        .unwrap_err()
        .to_string()
        .contains("omit build inputs"));
    }

    const SPEC: &str = r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789, 18800]
  connect: [18789]
  mcp_ports: [18800]
  app_service: cai-openclaw-gateway.service
build:
  image_name: openclaw-agent
  resize: 30G
  packages:
    - nodejs
  files:
    - source: ./image/skill.md
      target: /usr/local/share/confidential-agent/openclaw/skill.md
  scripts:
    - ./image/install-openclaw.sh
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
  private_ip: 10.0.1.20
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
secrets:
  disk_passphrase: ./secrets/disk_passphrase
resources:
  openclaw_config:
    source: ./secrets/openclaw.json
    target: /root/.openclaw/openclaw.json
    required: true
"#;

    #[test]
    fn parses_and_resolves_relative_paths() {
        let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();

        assert_eq!(spec.service.id, "openclaw");
        assert_eq!(spec.service.ports, vec![18789, 18800]);
        assert_eq!(spec.service.connect, vec![ConnectPort::Plain(18789)]);
        assert_eq!(spec.service.mcp_ports, vec![18800]);
        assert_eq!(
            spec.service.app_service.as_deref(),
            Some("cai-openclaw-gateway.service")
        );
        assert_eq!(spec.image_id(), "openclaw-agent");
        assert_eq!(spec.image_variant(), "release");
        assert_eq!(
            spec.build.scripts[0],
            PathBuf::from("/project/image/install-openclaw.sh")
        );
        assert_eq!(
            spec.build.files[0].source,
            PathBuf::from("/project/image/skill.md")
        );
        assert_eq!(
            spec.build.files[0].target,
            "/usr/local/share/confidential-agent/openclaw/skill.md"
        );
        assert_eq!(
            spec.build
                .variants
                .debug
                .as_ref()
                .and_then(|variant| variant.ssh_public_key.clone()),
            Some(PathBuf::from("/project/secrets/debug_ssh.pub"))
        );
        assert_eq!(
            spec.secrets.disk_passphrase,
            Some(PathBuf::from("/project/secrets/disk_passphrase"))
        );
        assert_eq!(
            spec.resources["openclaw_config"].source,
            PathBuf::from("/project/secrets/openclaw.json")
        );
        assert!(spec.resources["openclaw_config"].required);
        assert!(!spec.resources["openclaw_config"].mutable);
    }

    #[test]
    fn parses_mutable_resource_policy() {
        let yaml = SPEC.replace(
            "    required: true\n",
            "    required: true\n    mutable: true\n",
        );

        let spec = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap();

        assert!(spec.resources["openclaw_config"].mutable);
    }

    #[test]
    fn deployment_disk_size_follows_v1_units_without_overflow_or_shrinking_images() {
        for (size, gib) in [
            ("80G", 75),
            ("80GiB", 80),
            ("80.5GiB", 81),
            ("85899345921B", 81),
            ("81920 MiB", 80),
        ] {
            assert_eq!(configured_image_disk_gb(size).unwrap(), gib, "{size}");
        }
        for invalid in [
            "",
            "0",
            "1.2.3G",
            "-1G",
            "1PB",
            "NaN",
            "999999999999999999999999999999999999999999999999TB",
        ] {
            assert!(configured_image_disk_gb(invalid).is_err(), "{invalid}");
        }
        let omitted = SPEC
            .replace("  disk_gb: 200\n", "")
            .replace("resize: 30G", "resize: 80GiB");
        let spec = AgentSpec::from_yaml(&omitted, Path::new("/project")).unwrap();
        assert_eq!(spec.deployment_disk_gb().unwrap(), 80);
        let too_small = SPEC.replace("disk_gb: 200", "disk_gb: 20");
        assert!(AgentSpec::from_yaml(&too_small, Path::new("/project"))
            .unwrap_err()
            .to_string()
            .contains("smaller than build.resize"));
    }

    #[test]
    fn rejects_removed_build_fields() {
        for field in ["base_image: /images/base.qcow2", "with_network: true"] {
            let yaml = SPEC.replace("build:\n", &format!("build:\n  {field}\n"));
            let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();
            assert!(format!("{err:#}").contains("unknown field"));
        }
    }

    #[test]
    fn parses_and_resolves_build_container() {
        let yaml = SPEC.replace(
            "  image_name: openclaw-agent\n",
            r#"  image_name: openclaw-agent
  container:
    archive: ./containers/hermes.oci.tar
    mode: runtime
    runtime: containerd
    name: hermes-agent
    command: ["gateway", "run"]
    env:
      API_SERVER_ENABLED: "true"
    working_dir: /opt/data
    network: true
    mounts:
      - source: /var/lib/hermes/data
        target: /opt/data
      - source: /var/lib/hermes/readonly
        target: /etc/hermes/readonly
        read_only: true
"#,
        );
        let spec = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap();
        let container = spec.build.container.as_ref().unwrap();

        assert_eq!(
            container.archive.as_deref(),
            Some(Path::new("/project/containers/hermes.oci.tar"))
        );
        assert_eq!(container.mode, ContainerMode::Runtime);
        assert_eq!(container.runtime, ContainerRuntime::Containerd);
        assert_eq!(container.name.as_deref(), Some("hermes-agent"));
        assert_eq!(container.command, vec!["gateway", "run"]);
        assert_eq!(
            container.env.get("API_SERVER_ENABLED").map(String::as_str),
            Some("true")
        );
        assert_eq!(container.working_dir.as_deref(), Some("/opt/data"));
        assert_eq!(container.network, Some(true));
        assert_eq!(container.mounts.len(), 2);
        assert_eq!(container.mounts[0].source, "/var/lib/hermes/data");
        assert_eq!(container.mounts[0].target, "/opt/data");
        assert!(!container.mounts[0].read_only);
        assert_eq!(container.mounts[1].source, "/var/lib/hermes/readonly");
        assert_eq!(container.mounts[1].target, "/etc/hermes/readonly");
        assert!(container.mounts[1].read_only);
    }

    #[test]
    fn rejects_invalid_build_container_configs() {
        for (snippet, expected) in [
            ("  container: {}\n", "build.container.image or build.container.archive is required"),
            (
                "  container:\n    image: nousresearch/hermes-agent:v2026.6.5\n    working_dir: opt/data\n",
                "build.container.working_dir must be an absolute path",
            ),
            (
                "  container:\n    image: nousresearch/hermes-agent:v2026.6.5\n    name: bad/name\n",
                "build.container.name may only contain ASCII letters",
            ),
            (
                "  container:\n    image: nousresearch/hermes-agent:v2026.6.5\n    name: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
                "build.container.name must be at most 64 bytes",
            ),
            (
                "  container:\n    image: nousresearch/hermes-agent:v2026.6.5\n    env:\n      1BAD: value\n",
                "build.container.env key '1BAD'",
            ),
            (
                "  container:\n    archive: ./containers/hermes.oci.tar\n    pull: always\n",
                "build.container.pull is only valid when build.container.archive is not set",
            ),
            (
                "  container:\n    image: nousresearch/hermes-agent:v2026.6.5\n    mounts:\n      - source: var/lib/hermes/data\n        target: /opt/data\n",
                "build.container.mounts[0].source must be an absolute guest path",
            ),
            (
                "  container:\n    image: nousresearch/hermes-agent:v2026.6.5\n    mounts:\n      - source: /var/lib/hermes,data\n        target: /opt/data\n",
                "build.container.mounts[0] paths must not contain ':' or ','",
            ),
            (
                "  container:\n    image: nousresearch/hermes-agent:v2026.6.5\n    mounts:\n      - source: \"/var/lib/hermes\\ndata\"\n        target: /opt/data\n",
                "build.container.mounts[0].source must not contain control characters",
            ),
            (
                "  container:\n    image: nousresearch/hermes-agent:v2026.6.5\n    mounts:\n      - source: /var/lib/hermes/data\n        target: /opt/data\n      - source: /var/lib/hermes/other\n        target: /opt/data\n",
                "build.container.mounts[1].target duplicates an earlier mount target",
            ),
            (
                "  container:\n    image: nousresearch/hermes-agent:v2026.6.5\n    mounts:\n      - source: /var/lib/hermes/data\n        target: /opt/data\n      - source: /var/lib/hermes/other\n        target: /opt/data/\n",
                "build.container.mounts[1].target duplicates an earlier mount target",
            ),
            (
                "  container:\n    image: nousresearch/hermes-agent:v2026.6.5\n    mode: rootfs\n    mounts:\n      - source: /var/lib/hermes/data\n        target: /opt/data\n",
                "build.container.mounts is only supported with build.container.mode: runtime",
            ),
        ] {
            let yaml = SPEC.replace("  image_name: openclaw-agent\n", &format!("  image_name: openclaw-agent\n{snippet}"));
            let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();
            assert!(
                err.to_string().contains(expected),
                "expected {expected:?}, got {err:#}"
            );
        }
    }

    #[test]
    fn rejects_debian_package_names_for_default_mkosi_builds() {
        let yaml = SPEC.replace("    - nodejs", "    - build-essential");
        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();

        assert!(err
            .to_string()
            .contains("build.packages[0]='build-essential' uses a Debian/Ubuntu package name"));
    }

    #[test]
    fn rejects_whitespace_padded_debian_package_names() {
        let yaml = SPEC.replace("    - nodejs", "    - \" libffi-dev \"");
        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();

        assert!(err.to_string().contains("use 'libffi-devel' instead"));
    }

    #[test]
    fn rejects_additional_debian_package_names() {
        for package in [
            "python3-venv",
            "python-dev-is-python3",
            "libc-dev",
            "docker.io",
        ] {
            let yaml = SPEC.replace("    - nodejs", &format!("    - {package}"));
            let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();

            assert!(err
                .to_string()
                .contains(&format!("build.packages[0]='{package}'")));
        }
    }

    #[test]
    fn rejects_packages_unavailable_in_default_alinux_repos() {
        let yaml = SPEC.replace("    - nodejs", "    - ffmpeg");
        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();

        assert!(err.to_string().contains("repository that provides ffmpeg"));
    }

    #[test]
    fn rejects_empty_app_service() {
        let err = AgentSpec::from_yaml(
            &SPEC.replace(
                "  app_service: cai-openclaw-gateway.service",
                "  app_service: '   '",
            ),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("service.app_service must not be empty"));
    }

    #[test]
    fn parses_vllm_kernel_cmdline_and_resource_ownership() {
        let yaml = r#"
schema: confidential-agent/v1
service:
  id: openclaw-vllm
  ports: [18789]
  connect: [18789]
build:
  image_name: openclaw-vllm
  kernel_cmdline_append: swiotlb=4194304,any
  cleanup:
    remove_static_libs: false
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
  reference_values: rekor
  rekor:
    cosign_key: ./cosign.key
resources:
  openclaw_config:
    source: ./openclaw-vllm.json
    target: /home/openclaw/.openclaw/openclaw.json
    owner: openclaw
    group: openclaw
    mode: "0600"
    required: true
"#;

        let spec = AgentSpec::from_yaml(yaml, Path::new("/project")).unwrap();

        assert_eq!(
            spec.build.kernel_cmdline_append.as_deref(),
            Some("swiotlb=4194304,any")
        );
        assert_eq!(spec.build.cleanup.remove_static_libs, Some(false));
        let resource = &spec.resources["openclaw_config"];
        assert_eq!(resource.owner.as_deref(), Some("openclaw"));
        assert_eq!(resource.group.as_deref(), Some("openclaw"));
        assert_eq!(
            resource.source,
            PathBuf::from("/project/openclaw-vllm.json")
        );
    }

    #[test]
    fn rejects_legacy_endpoint_backend_port_spec() {
        let yaml = r#"
schema: confidential-agent/v1
service:
  id: openclaw
  type: agent
  endpoints:
    gateway:
      port: 18789
      backend_port: 18790
image:
  base: /images/alinux3.qcow2
deploy:
  provider: aliyun
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
delivery:
  mode: challenge
"#;
        let err = AgentSpec::from_yaml(yaml, Path::new("/project")).unwrap_err();
        assert!(err.to_string().contains("failed to parse agent spec"));
    }

    #[test]
    fn rejects_connect_port_not_in_ports() {
        let err = AgentSpec::from_yaml(
            &SPEC.replace("connect: [18789]", "connect: [3001]"),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("service.connect port 3001 must be listed in service.ports"));
    }

    #[test]
    fn rejects_mcp_port_not_in_ports() {
        let yaml = r#"
schema: confidential-agent/v1
service:
  id: app
  ports: [8080]
  mcp_ports: [9090]
build:
  image_name: app
deploy:
  provider: aliyun
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  disk_gb: 200
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#;
        let err = AgentSpec::from_yaml(yaml, Path::new("/project")).unwrap_err();
        assert!(err
            .to_string()
            .contains("service.mcp_ports port 9090 must be listed in service.ports"));
    }

    #[test]
    fn rejects_mcp_port_in_connect_ports() {
        let yaml = r#"
schema: confidential-agent/v1
service:
  id: app
  ports: [8080]
  connect: [8080]
  mcp_ports: [8080]
build:
  image_name: app
deploy:
  provider: aliyun
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
  disk_gb: 200
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#;
        let err = AgentSpec::from_yaml(yaml, Path::new("/project")).unwrap_err();
        assert!(err
            .to_string()
            .contains("service.mcp_ports port 8080 must not be listed in service.connect"));
    }

    #[test]
    fn rejects_specs_without_explicit_resources() {
        let err = AgentSpec::from_yaml(
            SPEC.replace(
                "resources:\n  openclaw_config:\n    source: ./secrets/openclaw.json\n    target: /root/.openclaw/openclaw.json\n    required: true\n",
                "",
            )
            .as_str(),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(format!("{err:?}").contains("missing field `resources`"));
    }

    #[test]
    fn accepts_trustee_mode_without_deployment_configuration() {
        let spec = AgentSpec::from_yaml(
            &SPEC.replace("mode: challenge", "mode: trustee"),
            Path::new("/project"),
        )
        .unwrap();

        assert_eq!(spec.attestation.mode, AttestationMode::Trustee);
    }

    #[test]
    fn rejects_specs_without_explicit_region() {
        let err = AgentSpec::from_yaml(
            &SPEC.replace("  region: cn-beijing\n", ""),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(format!("{err:?}").contains("missing field `region`"));
    }

    #[test]
    fn rejects_legacy_deploy_security() {
        let err = AgentSpec::from_yaml(
            &SPEC.replace(
                "  private_ip: 10.0.1.20\n",
                "  private_ip: 10.0.1.20\n  security:\n    allowed_cidr: 203.0.113.0/24\n",
            ),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(format!("{err:?}").contains("unknown field `security`"));
    }

    #[test]
    fn ipv4_cidr_contains_matches_prefixes() {
        assert!(ipv4_cidr_contains("59.82.126.0/24", "59.82.126.85".parse().unwrap()).unwrap());
        assert!(ipv4_cidr_contains("59.82.126.85/32", "59.82.126.85".parse().unwrap()).unwrap());
        assert!(ipv4_cidr_contains("0.0.0.0/0", "203.0.113.10".parse().unwrap()).unwrap());
        assert!(!ipv4_cidr_contains("34.84.30.0/24", "59.82.126.85".parse().unwrap()).unwrap());
    }

    #[test]
    fn rejects_specs_without_explicit_tee() {
        let err = AgentSpec::from_yaml(&SPEC.replace("  tee: tdx\n", ""), Path::new("/project"))
            .unwrap_err();

        assert!(format!("{err:?}").contains("missing field `tee`"));
    }

    #[test]
    fn parses_spec_with_a2a() {
        let yaml = format!(
            "{}\n{}",
            SPEC.trim(),
            r#"
a2a:
  id: openclaw-agent
  name: openclaw-agent
  version: "1.0.0"
  description: "OpenClaw confidential agent"
  skills:
    - id: chat
      name: Chat
      description: "General conversation"
"#
        );

        let spec = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap();

        let a2a = spec.a2a.unwrap();
        assert_eq!(a2a.name, "openclaw-agent");
        assert_eq!(a2a.version.as_deref(), Some("1.0.0"));
        assert_eq!(a2a.skills.len(), 1);
        assert_eq!(a2a.skills[0].id, "chat");
    }

    #[test]
    fn validates_a2a_signing_issuer_urls() {
        let yaml = format!(
            "{}\n{}",
            SPEC.trim(),
            r#"
a2a:
  id: openclaw-agent
  name: openclaw-agent
  signing:
    mode: sigstore-keyless
    required: true
    expected_issuer: https://token.actions.githubusercontent.com
    expected_subject: repo:example/project:ref:refs/heads/main
    oidc_issuer: https://token.actions.githubusercontent.com
"#
        );

        AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap();
    }

    #[test]
    fn rejects_invalid_a2a_signing_oidc_issuer() {
        let yaml = format!(
            "{}\n{}",
            SPEC.trim(),
            r#"
a2a:
  id: openclaw-agent
  name: openclaw-agent
  signing:
    oidc_issuer: token.actions.githubusercontent.com
"#
        );

        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();
        assert!(format!("{err:?}").contains("a2a.signing.oidc_issuer must be an https URL"));
    }

    #[test]
    fn rejects_legacy_peers_field() {
        let yaml = format!(
            "{}\n{}",
            SPEC.trim(),
            r#"
peers:
  - id: remote
    url: http://1.2.3.4:8089/.well-known/agent-card.json
"#
        );
        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();
        assert!(format!("{err:?}").contains("unknown field `peers`"));
    }

    #[test]
    fn spec_without_a2a_still_parses() {
        let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();
        assert!(spec.a2a.is_none());
    }

    #[test]
    fn rejects_duplicate_service_ports() {
        let err = AgentSpec::from_yaml(
            &SPEC.replace("ports: [18789, 18800]", "ports: [18789, 18789]"),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(err.to_string().contains("duplicate port 18789"));
    }

    #[test]
    fn rejects_zero_service_port() {
        let err = AgentSpec::from_yaml(
            &SPEC
                .replace("ports: [18789, 18800]", "ports: [0]")
                .replace("connect: [18789]", "connect: []"),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(err.to_string().contains("ports greater than 0"));
    }

    #[test]
    fn rejects_duplicate_connect_ports() {
        let err = AgentSpec::from_yaml(
            &SPEC.replace("connect: [18789]", "connect: [18789, 18789]"),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(err.to_string().contains("duplicate port 18789"));
    }

    #[test]
    fn rejects_zero_connect_port() {
        let err = AgentSpec::from_yaml(
            &SPEC.replace("connect: [18789]", "connect: [0]"),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(err.to_string().contains("greater than 0"));
    }

    #[test]
    fn rejects_resource_target_relative_path() {
        let err = AgentSpec::from_yaml(
            &SPEC.replace(
                "target: /root/.openclaw/openclaw.json",
                "target: root/.openclaw/openclaw.json",
            ),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("resources.openclaw_config.target must be an absolute path"));
    }

    #[test]
    fn rejects_build_file_target_relative_path() {
        let err = AgentSpec::from_yaml(
            &SPEC.replace(
                "target: /usr/local/share/confidential-agent/openclaw/skill.md",
                "target: usr/local/share/confidential-agent/openclaw/skill.md",
            ),
            Path::new("/project"),
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("build.files[0].target must be an absolute path"));
    }

    #[test]
    fn rejects_a2a_interface_port_not_in_connect() {
        let yaml = format!(
            "{}\n{}",
            SPEC.trim(),
            r#"
a2a:
  id: openclaw-agent
  name: openclaw-agent
  interfaces:
    - port: 18800
"#
        );
        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();

        assert!(err
            .to_string()
            .contains("a2a.interfaces[0].port must be listed in service.connect"));
    }

    #[test]
    fn rejects_zero_a2a_interface_port() {
        let yaml = format!(
            "{}\n{}",
            SPEC.trim(),
            r#"
a2a:
  id: openclaw-agent
  name: openclaw-agent
  interfaces:
    - port: 0
"#
        );
        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();

        assert!(err
            .to_string()
            .contains("a2a.interfaces[0].port must be greater than 0"));
    }

    #[test]
    fn rejects_signing_required_without_sigstore_mode() {
        let yaml = format!(
            "{}\n{}",
            SPEC.trim(),
            r#"
a2a:
  id: openclaw-agent
  name: openclaw-agent
  signing:
    required: true
    expected_issuer: https://issuer.example
    expected_subject: subject@example.com
"#
        );
        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();

        assert!(err.to_string().contains("mode must be sigstore-keyless"));
    }

    #[test]
    fn rejects_signing_required_without_expected_issuer() {
        let yaml = format!(
            "{}\n{}",
            SPEC.trim(),
            r#"
a2a:
  id: openclaw-agent
  name: openclaw-agent
  signing:
    mode: sigstore-keyless
    required: true
    expected_subject: subject@example.com
"#
        );
        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();

        assert!(err.to_string().contains("expected_issuer is required"));
    }

    #[test]
    fn rejects_signing_required_without_expected_subject() {
        let yaml = format!(
            "{}\n{}",
            SPEC.trim(),
            r#"
a2a:
  id: openclaw-agent
  name: openclaw-agent
  signing:
    mode: sigstore-keyless
    required: true
    expected_issuer: https://issuer.example
"#
        );
        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();

        assert!(err.to_string().contains("expected_subject is required"));
    }

    #[test]
    fn allows_empty_service_ports_for_client_only_workload() {
        let spec = AgentSpec::from_yaml(
            &SPEC
                .replace("ports: [18789, 18800]", "ports: []")
                .replace("connect: [18789]", "connect: []")
                .replace("mcp_ports: [18800]", "mcp_ports: []"),
            Path::new("/project"),
        )
        .unwrap();

        assert!(spec.service.ports.is_empty());
        assert!(spec.service.connect.is_empty());
        assert!(spec.service.mcp_ports.is_empty());
    }

    #[test]
    fn rejects_zero_a2a_cache_ttl() {
        let yaml = format!(
            "{}\n{}",
            SPEC.trim(),
            "a2a:\n  id: agent\n  name: agent\n  cacheTtlSec: 0\n"
        );
        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();

        assert!(err
            .to_string()
            .contains("cacheTtlSec must be greater than 0"));
    }

    #[test]
    fn rejects_a2a_interface_non_absolute_path() {
        let yaml = format!(
            "{}\n{}",
            SPEC.trim(),
            r#"
a2a:
  id: agent
  name: agent
  interfaces:
    - port: 18789
      path: a2a
"#
        );
        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();

        assert!(err
            .to_string()
            .contains("a2a.interfaces[0].path must start with '/'"));
    }

    #[test]
    fn rejects_whitespace_only_signing_issuer() {
        let yaml = format!(
            "{}\n{}",
            SPEC.trim(),
            r#"
a2a:
  id: agent
  name: agent
  signing:
    mode: sigstore-keyless
    required: true
    expected_issuer: "   "
    expected_subject: subject@example.com
"#
        );
        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();

        assert!(err.to_string().contains("expected_issuer"));
    }

    #[test]
    fn deploy_spec_replicas_defaults_to_one() {
        let spec = AgentSpec::from_yaml(SPEC, Path::new("/project")).unwrap();

        assert_eq!(spec.deploy.replicas, 1);
        assert!(spec.deploy.slb.is_none());
    }

    #[test]
    fn deploy_spec_parses_replicas_and_slb() {
        let yaml = SPEC.replace(
            "  disk_gb: 200\n",
            "  disk_gb: 200\n  replicas: 3\n  slb:\n    enabled: true\n    slb_type: nlb\n",
        );

        let spec = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap();

        assert_eq!(spec.deploy.replicas, 3);
        assert!(spec.deploy.slb.as_ref().unwrap().enabled);
        assert_eq!(spec.deploy.slb.as_ref().unwrap().slb_type, SlbType::Nlb);
    }

    #[test]
    fn deploy_spec_slb_defaults_to_nlb_when_type_omitted() {
        let yaml = SPEC.replace(
            "  disk_gb: 200\n",
            "  disk_gb: 200\n  replicas: 3\n  slb:\n    enabled: true\n",
        );

        let spec = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap();

        assert_eq!(spec.deploy.slb.as_ref().unwrap().slb_type, SlbType::Nlb);
    }

    #[test]
    fn rejects_zero_replicas() {
        let yaml = SPEC.replace("  disk_gb: 200\n", "  disk_gb: 200\n  replicas: 0\n");

        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();

        assert!(err
            .to_string()
            .contains("deploy.replicas must be greater than 0"));
    }

    #[test]
    fn rejects_replicas_gt_one_without_slb() {
        let yaml = SPEC.replace("  disk_gb: 200\n", "  disk_gb: 200\n  replicas: 3\n");

        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();

        assert!(err
            .to_string()
            .contains("deploy.replicas > 1 requires deploy.slb.enabled = true"));
    }

    #[test]
    fn rejects_replicas_gt_one_with_slb_disabled() {
        let yaml = SPEC.replace(
            "  disk_gb: 200\n",
            "  disk_gb: 200\n  replicas: 3\n  slb:\n    enabled: false\n    slb_type: nlb\n",
        );

        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();

        assert!(err
            .to_string()
            .contains("deploy.replicas > 1 requires deploy.slb.enabled = true"));
    }

    #[test]
    fn accepts_nlb_with_rats_tls() {
        let yaml = SPEC.replace(
            "  disk_gb: 200\n",
            "  disk_gb: 200\n  replicas: 3\n  slb:\n    enabled: true\n    slb_type: nlb\n",
        );

        // SPEC has connect: [18789] which defaults to RATS-TLS.
        AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap();
    }

    #[test]
    fn rejects_alb_with_rats_tls() {
        let yaml = SPEC.replace(
            "  disk_gb: 200\n",
            "  disk_gb: 200\n  replicas: 3\n  slb:\n    enabled: true\n    slb_type: alb\n",
        );

        let err = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap_err();

        assert!(err
            .to_string()
            .contains("deploy.slb.slb_type alb (L7) is incompatible with RATS-TLS"));
    }

    #[test]
    fn accepts_alb_with_ohttp() {
        let yaml = SPEC
            .replace(
                "  connect: [18789]",
                "  connect:\n    - port: 18789\n      encryption: ohttp",
            )
            .replace(
                "  disk_gb: 200\n",
                "  disk_gb: 200\n  replicas: 3\n  slb:\n    enabled: true\n    slb_type: alb\n",
            );

        let spec = AgentSpec::from_yaml(&yaml, Path::new("/project")).unwrap();

        assert_eq!(spec.deploy.slb.as_ref().unwrap().slb_type, SlbType::Alb);
    }
}
