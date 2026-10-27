use crate::cli::{
    A2aArgs, A2aCommands, BuildArgs, Cli, Commands, ConnectArgs, ConnectCommands, ConnectStartArgs,
    ConnectStopArgs, DeployArgs, DestroyArgs, DocsArgs, DocsTopic, ImageArgs, ImageCommands,
    ImagePruneArgs, ImagePublishArgs, ImageUnpublishArgs, InitArgs, InitReferenceValues, InitStage,
    InitTarget, InjectArgs, KeyArgs, KeyCommands, MeshArgs, MeshCommands, MigrateArgs,
    OutputFormat, PeeringArgs, PeeringCommands, ReportArgs, SpecArgs, SpecCommands, SshArgs,
    StatusArgs, TrusteeArgs, TrusteeCommands, TuiArgs, UpgradeArgs, UpgradeCommands,
};
use anyhow::{bail, Context, Result};
use base64::{
    engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD},
    Engine as _,
};
use confidential_agent_core::a2a::{
    A2aBundle, A2aBundlePeer, A2aCardSummary, A2aCliPreview, A2aCliPreviewError, A2aSignerPin,
    A2aStateFile, A2aStatePeer,
};
use confidential_agent_core::agent_card::{
    confidential_extension, derive_tng_client_config_with_local_ports,
};
use confidential_agent_core::agent_card_fetch::{
    fetch_agent_card, fetch_agent_card_with_signer, parse_agent_card_url, AgentCardFetchError,
};
use confidential_agent_core::peerings::{PeeringEntry, PeeringRole, PeeringScope, PeeringsFile};
use confidential_agent_core::schema::{
    confidential_ports, AgentCard, AppliedResourceState, BootstrapConfig, DaemonStatus,
    GatewayIdentity, GuestResource, LocalBuildState, LocalDebugSshKey, LocalDeployState,
    LocalGatewayIdentity, LocalResourceState, LocalServiceNetwork, LocalServiceState,
    LocalSpecState, MeshBundle, PublishedImage, AGENT_CARD_PORT, BOOTSTRAP_SCHEMA_VERSION,
    DAEMON_STATUS_PORT, LOCAL_SERVICE_STATE_SCHEMA_VERSION,
};
#[cfg(test)]
use confidential_agent_core::schema::{
    AgentCardCapabilities, AgentCardConfidential, AgentExtension, AgentInterface,
    MESH_SCHEMA_VERSION,
};
use confidential_agent_core::schema::{AgentCardPort, AgentCardRekor, DaemonA2aPeerStatus};
#[cfg(test)]
use confidential_agent_core::spec::ConnectPort;
use confidential_agent_core::spec::{
    AgentSpec, AttestationMode, AttestationTee, EncryptionMode, ReferenceValueMode,
};
use confidential_agent_core::util::{hex_encode, rekor_payload, sha256_file};
use confidential_agent_shelter::{
    render_build_config, render_runtime_config, shelter_build_id, GuestAssets, GuestFileAsset,
    ShelterRenderOptions,
};
use curve25519_dalek::{constants::ED25519_BASEPOINT_POINT, scalar::Scalar};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::unix::{
    fs::{chown, MetadataExt, PermissionsExt},
    io::AsRawFd,
};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug)]
struct ContextPaths {
    service_dir: PathBuf,
    shelter_work_dir: PathBuf,
    artifacts_dir: PathBuf,
    cache_dir: PathBuf,
    guest_staging_dir: PathBuf,
    secrets_dir: PathBuf,
    rendered_config: PathBuf,
    manifest: PathBuf,
    bootstrap_file: PathBuf,
    service_state: PathBuf,
    agent_card: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct BuildManifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preparation_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runtime_contract: Option<image_runtime::ImageRuntimeContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deployment: Option<DeploymentSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ssh: Option<instance_ssh::InstanceSsh>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image: Option<images::ImageDeployment>,
    service_id: String,
    shelter_build_id: String,
    shelter_work_dir: PathBuf,
    build_result: PathBuf,
    deploy_result: PathBuf,
    shelter_config: PathBuf,
    agentd_bin: PathBuf,
    agentd_service: PathBuf,
    gateway_bin: PathBuf,
    gateway_service: PathBuf,
    tng_service: PathBuf,
    fde_config_file: PathBuf,
    policy_default: PathBuf,
    policy_local_dev: PathBuf,
    images_dir: PathBuf,
    cache_dir: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    guest_setup_script: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    extra_files: Vec<GuestFileAsset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    debug_ssh: Option<LocalDebugSshKey>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    variants: BTreeMap<String, BuildManifestVariant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeploymentSnapshot {
    source: LocalSpecState,
    spec: AgentSpec,
    user_data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    inputs: Option<BTreeMap<String, DeploymentInput>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeploymentInput {
    source: PathBuf,
    sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    snapshot: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BuildManifestVariant {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preparation_dir: Option<PathBuf>,
    shelter_build_id: String,
    build_result: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    extra_files: Vec<GuestFileAsset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    debug_ssh: Option<LocalDebugSshKey>,
}

struct PreparedConfig {
    image: Option<images::ImageDeployment>,
    rendered_config: PathBuf,
    shelter_build_id: String,
    shelter_work_dir: PathBuf,
    build_result: PathBuf,
    deploy_result: PathBuf,
    deploy_names: Option<DeployNames>,
    terraform_dir: Option<PathBuf>,
    debug_ssh: Option<LocalDebugSshKey>,
    cloud_image_id: Option<String>,
}

impl PreparedConfig {
    fn runtime_config(&self) -> PathBuf {
        if self.image.is_some() {
            self.rendered_config.clone()
        } else {
            shelter_runtime_config_path(&self.rendered_config)
        }
    }
}

fn shelter_runtime_config_path(build_config: &Path) -> PathBuf {
    build_config.with_file_name("shelter-runtime.yaml")
}

#[derive(Debug, Clone, Default)]
struct PrepareOptions {
    preparation_dir: Option<PathBuf>,
    build_id: Option<String>,
    image_variant: Option<String>,
    include_deploy: bool,
    resolve_container_images: bool,
    deploy_names: Option<DeployNames>,
    mesh_peer_cidrs: Vec<String>,
    peerings: PeeringsFile,
    cloud_image_id: Option<String>,
    deploy_user_data: Option<String>,
    deployer_cidr: Option<String>,
}

impl BuildManifest {
    fn fallback_variant(&self) -> BuildManifestVariant {
        BuildManifestVariant {
            preparation_dir: self.preparation_dir.clone(),
            shelter_build_id: self.shelter_build_id.clone(),
            build_result: self.build_result.clone(),
            extra_files: self.extra_files.clone(),
            debug_ssh: self.debug_ssh.clone(),
        }
    }

    fn guest_assets(&self) -> GuestAssets {
        GuestAssets {
            agentd_bin: self.agentd_bin.clone(),
            agentd_service: self.agentd_service.clone(),
            gateway_bin: self.gateway_bin.clone(),
            gateway_service: self.gateway_service.clone(),
            tng_service: self.tng_service.clone(),
            fde_config_file: self.fde_config_file.clone(),
            policy_default: self.policy_default.clone(),
            policy_local_dev: self.policy_local_dev.clone(),
            guest_setup_script: self.guest_setup_script.clone(),
            extra_files: self.extra_files.clone(),
        }
    }

    fn variant(&self, name: &str, fallback_name: Option<&str>) -> Result<BuildManifestVariant> {
        if let Some(variant) = self.variants.get(name) {
            return Ok(variant.clone());
        }
        if self.variants.is_empty() && fallback_name == Some(name) {
            return Ok(self.fallback_variant());
        }
        bail!("local build for variant '{name}' is missing; run build first")
    }
}

fn manifest_variant_from(manifest: &BuildManifest) -> BuildManifestVariant {
    manifest.fallback_variant()
}

#[derive(Debug, Clone)]
struct DeployNames {
    run_id: String,
    resource_name: String,
    image_import_name: String,
}

struct StateDirLock {
    file: File,
}

const LOCK_EX: i32 = 2;
const LOCK_SH: i32 = 1;
const LOCK_UN: i32 = 8;

extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

impl Drop for StateDirLock {
    fn drop(&mut self) {
        unsafe {
            let _ = flock(self.file.as_raw_fd(), LOCK_UN);
        }
    }
}

fn with_state_dir_lock<T>(state_dir: &Path, action: impl FnOnce() -> Result<T>) -> Result<T> {
    let _guard = lock_state_dir(state_dir)?;
    action()
}

fn lock_state_dir(state_dir: &Path) -> Result<StateDirLock> {
    lock_operation_file(&state_dir.join(".lock"))
}

fn lock_service_operation(state_dir: &Path, service_id: &str) -> Result<StateDirLock> {
    validate_service_id(service_id)?;
    let directory = context_paths(state_dir, service_id).service_dir;
    if let Ok(metadata) = directory.symlink_metadata() {
        anyhow::ensure!(
            !metadata.file_type().is_symlink(),
            "service directory must not be a symlink"
        );
    }
    lock_operation_file(
        &absolute_path_for_state(state_dir)
            .join("operation-locks")
            .join(service_id),
    )
}

fn validate_service_id(service_id: &str) -> Result<()> {
    anyhow::ensure!(
        !service_id.is_empty()
            && !matches!(service_id, "." | "..")
            && service_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')),
        "invalid service ID; expected a name, not a path"
    );
    Ok(())
}

fn lock_operation_file(lock_path: &Path) -> Result<StateDirLock> {
    lock_operation_file_with_mode(lock_path, LOCK_EX)
}

fn lock_image_operation(
    state_dir: &Path,
    region: &str,
    image_id: &str,
    shared: bool,
) -> Result<StateDirLock> {
    let digest = hex_encode(&Sha256::digest(format!("{region}\0{image_id}").as_bytes()));
    lock_operation_file_with_mode(
        &absolute_path_for_state(state_dir)
            .join("image-locks")
            .join(digest),
        if shared { LOCK_SH } else { LOCK_EX },
    )
}

fn lock_operation_file_with_mode(lock_path: &Path, mode: i32) -> Result<StateDirLock> {
    use std::os::unix::fs::OpenOptionsExt;
    let parent = lock_path.parent().context("operation lock has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create '{}'", parent.display()))?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(lock_path)
        .with_context(|| format!("failed to open '{}'", lock_path.display()))?;
    loop {
        let rc = unsafe { flock(file.as_raw_fd(), mode) };
        if rc == 0 {
            break;
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(err).with_context(|| format!("failed to lock '{}'", lock_path.display()));
    }
    Ok(StateDirLock { file })
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let content = serde_json::to_vec_pretty(value)?;
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

impl DeployNames {
    fn new(spec: &AgentSpec) -> Self {
        let mut nonce = [0u8; 8];
        OsRng.fill_bytes(&mut nonce);
        Self::from_run_id(
            spec,
            &format!("{}-{}", current_run_id(), hex_encode(&nonce)),
        )
    }

    fn from_run_id(spec: &AgentSpec, run_id: &str) -> Self {
        let service = sanitize_cloud_name_component(&spec.service.id);
        let image = sanitize_cloud_name_component(spec.image_id());
        let variant = sanitize_cloud_name_component(spec.image_variant());
        let resource_name = timestamped_resource_name(&service, run_id);
        let image_import_name = format!("{image}-{variant}-{run_id}");
        Self {
            run_id: run_id.to_string(),
            resource_name,
            image_import_name,
        }
    }
}

struct NativeToolSpec {
    tool: &'static str,
    tool_args: Vec<OsString>,
    envs: Vec<(String, String)>,
    workdir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConnectClientEndpoint {
    service: String,
    guest_port: u16,
    local_host: String,
    local_port: u16,
    protocol: String,
    http_base_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ConnectReadyFile {
    schema: String,
    process: native_process::ProcessIdentity,
    log_file: PathBuf,
    started_at: String,
    client_endpoints: Vec<ConnectClientEndpoint>,
}

#[derive(Debug)]
struct ReferenceValueArtifacts {
    sample: BTreeMap<String, serde_json::Value>,
    rekor: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Default)]
struct DeployObservation {
    instance_id: Option<String>,
    security_group_id: Option<String>,
    public_ip: Option<String>,
    private_ip: Option<String>,
    instance_ids: Vec<String>,
    instance_private_ips: Vec<String>,
    instance_public_ips: Vec<String>,
    slb_vip: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ShelterBuildResult {
    #[serde(rename = "tag", alias = "id")]
    id: String,
    image_path: PathBuf,
    reference_value: Option<serde_json::Value>,
    #[serde(default)]
    rekor_value: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ShelterDeployResult {
    #[serde(rename = "deployment_id", alias = "id")]
    id: String,
    deploy: ShelterDeployResultInfo,
}

#[derive(Debug, Deserialize)]
struct ShelterDeployResultInfo {
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "provider_instance_id", alias = "instance_id")]
    instance_id: Option<serde_json::Value>,
    public_ip: Option<serde_json::Value>,
    private_ip: Option<serde_json::Value>,
    outputs: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct LocalStateHeader {
    schema: String,
}

struct ShelterBuildArtifacts {
    image_path: PathBuf,
    sample_rv: Option<PathBuf>,
    rekor_meta: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct ReplicaStatusView {
    instance_id: Option<String>,
    address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon: Option<DaemonStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    live_error: Option<String>,
}

#[derive(Debug, Serialize)]
struct LiveStatusView {
    local: StatusView,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    replicas: Vec<ReplicaStatusView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon: Option<DaemonStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    live_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct StatusView {
    service_id: String,
    phase: String,
    build: StatusBuildView,
    local_image: StatusLocalImageView,
    cloud: StatusCloudView,
    service: LocalServiceNetwork,
    resources: BTreeMap<String, LocalResourceState>,
    mesh_generation: u64,
    reference_values: String,
}

#[derive(Debug, Clone, Serialize)]
struct StatusBuildView {
    build_id: String,
    image_name: String,
    variant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    debug_ssh: Option<LocalDebugSshKey>,
}

#[derive(Debug, Clone, Serialize)]
struct StatusLocalImageView {
    present: bool,
    path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    build_result: PathBuf,
    build_result_present: bool,
}

#[derive(Debug, Clone, Serialize)]
struct StatusCloudView {
    present: bool,
    /// `unknown` includes partial infrastructure operations, never proven absent.
    observation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terraform_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_import_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    security_group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    private_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    public_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    management_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tee: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ImageListEntry {
    publications: Vec<PublishedImage>,
    service_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
    build_id: String,
    current: bool,
    image_path: PathBuf,
    image_present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_size: Option<u64>,
    build_result: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    published: Option<String>,
}

const MAX_SHELTER_IMAGE_BUCKET_LEN: usize = 63;
const SHELTER_IMAGE_BUCKET_SUFFIX: &str = "-images";
const A2A_BUNDLE_RESOURCE: &str = "default/local-resources/cagent_a2a_bundle";
const REFERENCE_VALUE_LIST_RESOURCE_ID: &str = "cagent_reference_value_list";
const GUEST_REFERENCE_VALUE_LIST_PATH: &str = "/opt/confidential-agent/reference-value-list.json";

impl DeployObservation {
    fn preferred_injection_ip(&self) -> Option<String> {
        self.instance_public_ips
            .first()
            .or_else(|| self.instance_private_ips.first())
            .map(String::as_str)
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                self.public_ip
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .or_else(|| {
                        self.private_ip
                            .as_deref()
                            .filter(|value| !value.trim().is_empty())
                    })
            })
            .map(ToOwned::to_owned)
    }
}

const DEFAULT_POLICY: &str = include_str!("../../tools/policies/trustee-opa-default.rego");
const LOCAL_DEV_POLICY: &str = include_str!("../../tools/policies/trustee-opa-local-dev.rego");

mod commands;
#[cfg(test)]
use commands::deploy_shelter_args;
use commands::fetch_daemon_status_from;
pub(crate) use commands::run;

mod init;
use init::*;

mod self_describe;
use self_describe::*;

mod trustee;

fn prepare(
    cli: &Cli,
    state_dir: &Path,
    spec_path: &Path,
    options: PrepareOptions,
) -> Result<PreparedConfig> {
    let mut spec = AgentSpec::from_path(spec_path)?;
    if let Some(variant) = options.image_variant.as_ref() {
        spec.deploy.image_variant = Some(variant.clone());
    }
    if options.resolve_container_images {
        if let Some(container) = &mut spec.build.container {
            if let Some(image) = container.image.as_mut() {
                *image = resolve_build_container_image(image)?;
            }
        }
    }
    spec.ensure_mvp_supported()?;

    let mut paths = context_paths(state_dir, &spec.service.id);
    let config_alias = paths.rendered_config.clone();
    let existing = if options.include_deploy && paths.manifest.try_exists()? {
        let manifest = read_build_manifest(&paths.manifest)?;
        let variant = manifest.variant(spec.image_variant(), Some(spec.image_variant()))?;
        Some(match variant.preparation_dir {
            Some(directory) => read_build_manifest(&directory.join("manifest.json"))?,
            None => manifest,
        })
    } else {
        None
    };
    let preparation_dir = options.preparation_dir.clone().or_else(|| {
        existing
            .as_ref()
            .and_then(|manifest| manifest.preparation_dir.clone())
    });
    if let Some(directory) = &preparation_dir {
        paths.guest_staging_dir = directory.join("guest");
        paths.secrets_dir = directory.join("secrets");
        paths.rendered_config = directory.join("shelter.yaml");
        paths.bootstrap_file = directory.join("bootstrap.json");
        if !options.include_deploy {
            paths.manifest = directory.join("manifest.json");
        }
    }
    fs::create_dir_all(&paths.guest_staging_dir)
        .with_context(|| format!("failed to create '{}'", paths.guest_staging_dir.display()))?;
    fs::create_dir_all(&paths.shelter_work_dir)
        .with_context(|| format!("failed to create '{}'", paths.shelter_work_dir.display()))?;
    let debug_ssh = if let Some(manifest) = &existing {
        if let Some(key) = &manifest.debug_ssh {
            apply_debug_ssh_public_key(&mut spec, &key.public_key)?;
        }
        manifest.debug_ssh.clone()
    } else {
        ensure_debug_ssh_key(&paths, &mut spec)?
    };
    let mut assets = if let Some(manifest) = &existing {
        manifest.guest_assets()
    } else {
        prepare_guest_assets(cli, &paths.guest_staging_dir)?
    };
    let runtime_contract = if let Some(manifest) = &existing {
        manifest.runtime_contract.clone()
    } else {
        Some(image_runtime::ImageRuntimeContract::for_build(
            &spec,
            &assets.agentd_bin,
        )?)
    };
    if existing.is_none() {
        if let Some(contract) = &runtime_contract {
            let contract_file = paths.guest_staging_dir.join("image-runtime.json");
            write_json_atomic(&contract_file, contract)?;
            assets.extra_files.push(GuestFileAsset {
                source: contract_file,
                destination: "/usr/local/share/confidential-agent/image-runtime.json".into(),
                executable: false,
            });
        }
        assets
            .extra_files
            .extend(spec.build.files.iter().map(|file| GuestFileAsset {
                source: file.source.clone(),
                destination: file.target.clone(),
                executable: file.executable,
            }));
        if spec.deploys_debug_image() {
            stage_mkosi_debug_ssh_authorized_keys(
                &mut assets,
                &paths,
                spec.build
                    .variants
                    .debug
                    .as_ref()
                    .and_then(|debug| debug.ssh_public_key.as_deref()),
            )?;
        }
    }
    let deploy_names = options.deploy_names.clone();
    // Managed runs allocate their own deployment ID and Terraform directory.
    let terraform_dir = None;
    let build_id = options
        .build_id
        .clone()
        .unwrap_or_else(|| shelter_build_id(&spec));
    let render_options = ShelterRenderOptions {
        build_id: Some(build_id.clone()),
        images_dir: Some(paths.artifacts_dir.clone()),
        cache_dir: Some(paths.cache_dir.clone()),
        terraform_dir: terraform_dir.clone(),
        include_deploy: options.include_deploy,
        local_image_source: None,
        deploy_resource_name: options
            .deploy_names
            .as_ref()
            .map(|names| names.resource_name.clone()),
        local_image_import_name: options
            .deploy_names
            .as_ref()
            .map(|names| names.image_import_name.clone()),
        mesh_peer_cidrs: options.mesh_peer_cidrs.clone(),
        peerings: options.peerings.clone(),
        cloud_image_id: options.cloud_image_id.clone(),
        deploy_user_data: options.deploy_user_data.clone(),
        deployer_cidr: options.deployer_cidr.clone(),
    };

    fs::create_dir_all(&paths.service_dir)
        .with_context(|| format!("failed to create '{}'", paths.service_dir.display()))?;
    // Preserve the build recipe associated with the immutable image. Runtime
    // changes belong to a separate document consumed only when creating a VM.
    let (rendered_path, rendered) = if options.include_deploy {
        (
            shelter_runtime_config_path(&paths.rendered_config),
            render_runtime_config(&spec, &render_options)?,
        )
    } else {
        (
            paths.rendered_config.clone(),
            render_build_config(&spec, &assets, &render_options)?,
        )
    };
    fs::write(&rendered_path, rendered)
        .with_context(|| format!("failed to write '{}'", rendered_path.display()))?;

    let build_result = shelter_build_result_path(&paths.shelter_work_dir, &build_id);
    let deploy_result = shelter_deploy_result_path(&paths.shelter_work_dir);

    let manifest = BuildManifest {
        preparation_dir,
        runtime_contract,
        deployment: None,
        ssh: if options.include_deploy {
            instance_ssh::identity(&context_paths(state_dir, &spec.service.id), &spec)?
        } else {
            None
        },
        image: None,
        service_id: spec.service.id.clone(),
        shelter_build_id: build_id.clone(),
        shelter_work_dir: paths.shelter_work_dir.clone(),
        build_result: build_result.clone(),
        deploy_result: deploy_result.clone(),
        shelter_config: paths.rendered_config.clone(),
        agentd_bin: assets.agentd_bin,
        agentd_service: assets.agentd_service,
        gateway_bin: assets.gateway_bin,
        gateway_service: assets.gateway_service,
        tng_service: assets.tng_service,
        fde_config_file: assets.fde_config_file,
        policy_default: assets.policy_default,
        policy_local_dev: assets.policy_local_dev,
        images_dir: paths.artifacts_dir.clone(),
        cache_dir: paths.cache_dir.clone(),
        guest_setup_script: assets.guest_setup_script,
        extra_files: assets.extra_files,
        debug_ssh: debug_ssh.clone(),
        variants: BTreeMap::new(),
    };
    fs::write(&paths.manifest, serde_json::to_string_pretty(&manifest)?)
        .with_context(|| format!("failed to write '{}'", paths.manifest.display()))?;
    if options.include_deploy {
        link_committed_config(&config_alias, &paths.rendered_config)?;
        link_committed_config(&shelter_runtime_config_path(&config_alias), &rendered_path)?;
    }

    Ok(PreparedConfig {
        image: None,
        rendered_config: paths.rendered_config,
        shelter_build_id: build_id,
        shelter_work_dir: paths.shelter_work_dir,
        build_result,
        deploy_result,
        deploy_names,
        terraform_dir,
        debug_ssh,
        cloud_image_id: options.cloud_image_id,
    })
}

fn verify_operator_peering_for_direct_injection(state_dir: &Path, skip: bool) -> Result<()> {
    if skip {
        eprintln!("[ca] warning: skipping peerings.yaml operator egress check");
        return Ok(());
    }
    let path = peerings_path(state_dir);
    if !path.exists() {
        bail!(
            "no operator peering with control+status scope in {}. Add one before deploy, e.g.:\n  confidential-agent peering add --role operator --cidr <your-jumphost-cidr>/32 --label <name>",
            path.display()
        );
    }
    let peerings = PeeringsFile::from_path(&path)?;
    if !peerings.has_operator_control_status() {
        bail!(
            "no operator peering with control+status scope in {}. Add one before deploy, e.g.:\n  confidential-agent peering add --role operator --cidr <your-jumphost-cidr>/32 --label <name>",
            path.display()
        );
    }

    let Some(egress_ip) = resolve_direct_egress_ip()? else {
        eprintln!(
            "[ca] warning: could not determine direct public egress IP; peerings.yaml must include the host that will connect to guest :8006/:8088"
        );
        return Ok(());
    };

    if !peerings.control_cidrs_contain(egress_ip)? {
        bail!(
            "this host's outbound IP {egress_ip} is not covered by any operator peering with control scope. Add it first, or rerun with --skip-peering-check."
        );
    }
    Ok(())
}

fn resolve_direct_egress_ip() -> Result<Option<Ipv4Addr>> {
    if let Some(value) = std::env::var_os("CA_OPERATOR_EGRESS_IP") {
        let value = value.to_string_lossy();
        return value.trim().parse::<Ipv4Addr>().map(Some).with_context(|| {
            format!("CA_OPERATOR_EGRESS_IP is not a valid IPv4 address: {value}")
        });
    }

    for endpoint in ["https://ipinfo.io/ip", "https://api.ipify.org"] {
        if let Some(ip) = query_direct_egress_ip(endpoint)? {
            return Ok(Some(ip));
        }
    }
    Ok(None)
}

fn query_direct_egress_ip(endpoint: &str) -> Result<Option<Ipv4Addr>> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(5))
        .redirects(0)
        .try_proxy_from_env(false)
        .build();
    let response = match agent.get(endpoint).call() {
        Ok(response) => response,
        Err(_) => return Ok(None),
    };
    let body = response
        .into_string()
        .with_context(|| format!("failed to read egress IP response from {endpoint}"))?;
    match body.trim().parse::<Ipv4Addr>() {
        Ok(ip) => Ok(Some(ip)),
        Err(_) => Ok(None),
    }
}

fn peerings_path(state_dir: &Path) -> PathBuf {
    absolute_path_for_state(state_dir).join("peerings.yaml")
}

fn a2a_state_path(state_dir: &Path) -> PathBuf {
    absolute_path_for_state(state_dir).join("a2a.json")
}

fn a2a_bundle_path(state_dir: &Path) -> PathBuf {
    absolute_path_for_state(state_dir).join("a2a-bundle.json")
}

fn read_peerings_or_empty(state_dir: &Path) -> Result<PeeringsFile> {
    let path = peerings_path(state_dir);
    if path.exists() {
        PeeringsFile::from_path(&path)
    } else {
        Ok(PeeringsFile::empty())
    }
}

fn current_utc_timestamp() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| unix_timestamp().to_string())
}

mod debug_ssh;
use debug_ssh::*;
mod guest_assets;
use guest_assets::*;

mod state;
use state::*;

mod workflows;
use workflows::*;

mod cloud_credentials;
mod image_runtime;
mod images;
mod instance_ssh;
mod native_process;
mod oss_endpoint;
mod publish;
mod upgrade;

mod report;
use report::*;

mod tui;
use tui::*;

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn current_run_id() -> String {
    let output = Command::new("date")
        .arg("+%Y%m%d%H%M%S")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| value.len() == 14 && value.chars().all(|ch| ch.is_ascii_digit()));
    output.unwrap_or_else(|| unix_timestamp().to_string())
}

fn current_build_run_id() -> String {
    let output = Command::new("date")
        .arg("+%Y%m%d%H%M%S%3N")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| value.len() == 17 && value.chars().all(|ch| ch.is_ascii_digit()));
    output.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string()
    })
}

#[cfg(test)]
fn timestamped_shelter_build_id(spec: &AgentSpec, run_id: &str) -> String {
    format!("{}-{run_id}", shelter_build_id(spec))
}

fn shelter_build_id_for_variant(spec: &AgentSpec, variant: &str) -> String {
    format!("{}-{variant}", spec.image_id())
}

fn timestamped_shelter_build_id_for_variant(
    spec: &AgentSpec,
    variant: &str,
    run_id: &str,
) -> String {
    format!("{}-{run_id}", shelter_build_id_for_variant(spec, variant))
}

fn enabled_build_variants(spec: &AgentSpec) -> Vec<String> {
    let mut variants = Vec::new();
    if spec.build.variants.release.enabled {
        variants.push("release".to_string());
    }
    if spec
        .build
        .variants
        .debug
        .as_ref()
        .map(|debug| debug.enabled)
        .unwrap_or(false)
    {
        variants.push("debug".to_string());
    }
    variants
}

fn sanitize_cloud_name_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let collapsed = sanitized
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if collapsed.is_empty() {
        "svc".to_string()
    } else {
        collapsed
    }
}

fn timestamped_resource_name(service: &str, run_id: &str) -> String {
    let suffix = format!("-{run_id}");
    let max_service_len = MAX_SHELTER_IMAGE_BUCKET_LEN
        .saturating_sub(SHELTER_IMAGE_BUCKET_SUFFIX.len())
        .saturating_sub(suffix.len());
    let mut service = service.to_string();
    if service.len() > max_service_len {
        service.truncate(max_service_len);
        service = service.trim_matches('-').to_string();
    }
    if service.is_empty() {
        service = "svc".to_string();
    }
    format!("{service}{suffix}")
}

fn render_bootstrap(paths: &ContextPaths, spec: &AgentSpec) -> Result<BootstrapConfig> {
    let runtime_spec = instance_ssh::runtime_spec(paths, spec)?;
    let spec = &runtime_spec;
    let gateway_identity = ensure_gateway_identity(paths)?;
    let resources = spec
        .resources
        .iter()
        .map(|(id, resource)| {
            Ok(GuestResource {
                id: id.clone(),
                resource_path: resource_path(id),
                target: PathBuf::from(resource.target.clone()),
                owner: resource.owner.clone(),
                group: resource.group.clone(),
                mode: resource.mode.clone().unwrap_or_else(|| "0600".to_string()),
                required: resource.required,
                mutable: resource.mutable,
                sha256: Some(sha256_file(&resource.source)?),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let generation = read_service_state_file(&paths.service_state)
        .ok()
        .flatten()
        .map(|state| state.generation + 1)
        .unwrap_or(1);

    Ok(BootstrapConfig {
        schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
        generation,
        service_id: spec.service.id.clone(),
        mode: attestation_mode_name(spec.attestation.mode).to_string(),
        ports: spec.service.ports.clone(),
        connect: spec.service.connect.clone(),
        mcp_ports: spec.service.mcp_ports.clone(),
        gateway_identity: Some(GatewayIdentity {
            public_key: gateway_identity.public_key,
            private_key: fs::read_to_string(&gateway_identity.private_key_path)
                .with_context(|| {
                    format!(
                        "failed to read '{}'",
                        gateway_identity.private_key_path.display()
                    )
                })?
                .trim()
                .to_string(),
        }),
        resources,
        app_service: spec.service.app_service.clone(),
        peers: Vec::new(),
        agent_card: None,
        tng_peers: Vec::new(),
    })
}

fn resource_path(id: &str) -> String {
    format!("default/local-resources/{id}")
}

fn reference_value_mode_name(mode: ReferenceValueMode) -> &'static str {
    match mode {
        ReferenceValueMode::Sample => "sample",
        ReferenceValueMode::Rekor => "rekor",
    }
}

fn attestation_mode_name(mode: AttestationMode) -> &'static str {
    match mode {
        AttestationMode::Challenge => "challenge",
        AttestationMode::Trustee => "trustee",
    }
}

fn tee_name(tee: AttestationTee) -> &'static str {
    match tee {
        AttestationTee::Tdx => "tdx",
    }
}

fn ensure_disk_passphrase(paths: &ContextPaths) -> Result<PathBuf> {
    let path = paths.secrets_dir.join("disk_passphrase");
    if path.exists() {
        return Ok(path);
    }

    let mut bytes = [0u8; 32];
    File::open("/dev/urandom")
        .context("failed to open /dev/urandom")?
        .read_exact(&mut bytes)
        .context("failed to read disk passphrase entropy")?;

    fs::write(&path, format!("{}\n", hex_encode(&bytes)))
        .with_context(|| format!("failed to write '{}'", path.display()))?;
    set_mode(&path, 0o600)?;
    Ok(path)
}

fn ensure_gateway_identity(paths: &ContextPaths) -> Result<LocalGatewayIdentity> {
    ensure_private_context_dirs(paths)?;
    let private_key_path = paths.secrets_dir.join("gateway_identity.seed");
    let public_key_path = paths.secrets_dir.join("gateway_identity.pub");
    match (private_key_path.exists(), public_key_path.exists()) {
        (true, true) => {
            let public_key = fs::read_to_string(&public_key_path)
                .with_context(|| format!("failed to read '{}'", public_key_path.display()))?
                .trim()
                .to_string();
            let private_key = fs::read_to_string(&private_key_path)
                .with_context(|| format!("failed to read '{}'", private_key_path.display()))?
                .trim()
                .to_string();
            validate_gateway_identity_pair(&private_key, &public_key)?;
            set_mode(&private_key_path, 0o600)?;
            set_mode(&public_key_path, 0o644)?;
            Ok(LocalGatewayIdentity {
                public_key,
                private_key_path,
            })
        }
        (false, false) => {
            let mut seed = [0u8; 32];
            File::open("/dev/urandom")
                .context("failed to open /dev/urandom")?
                .read_exact(&mut seed)
                .context("failed to read gateway identity entropy")?;
            let (public_key, _) = ed25519_keypair_from_seed(&seed);
            let private_key = URL_SAFE_NO_PAD.encode(seed);
            let public_key = URL_SAFE_NO_PAD.encode(public_key);
            fs::write(&private_key_path, format!("{private_key}\n"))
                .with_context(|| format!("failed to write '{}'", private_key_path.display()))?;
            fs::write(&public_key_path, format!("{public_key}\n"))
                .with_context(|| format!("failed to write '{}'", public_key_path.display()))?;
            set_mode(&private_key_path, 0o600)?;
            set_mode(&public_key_path, 0o644)?;
            Ok(LocalGatewayIdentity {
                public_key,
                private_key_path,
            })
        }
        _ => bail!(
            "incomplete generated gateway identity under '{}'; remove both gateway_identity files before retrying",
            paths.secrets_dir.display()
        ),
    }
}

fn validate_gateway_identity_pair(private_key: &str, public_key: &str) -> Result<()> {
    let seed = URL_SAFE_NO_PAD
        .decode(private_key)
        .context("gateway private key must be base64url without padding")?;
    let seed: [u8; 32] = seed
        .try_into()
        .map_err(|_| anyhow::anyhow!("gateway private key must decode to 32 bytes"))?;
    let expected_public = URL_SAFE_NO_PAD.encode(ed25519_keypair_from_seed(&seed).0);
    if expected_public != public_key {
        bail!("gateway public key does not match private key seed");
    }
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn ensure_private_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                bail!(
                    "refusing to use private directory symlink '{}'",
                    path.display()
                );
            }
            if !file_type.is_dir() {
                bail!(
                    "refusing to use private directory over non-directory '{}'",
                    path.display()
                );
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .with_context(|| format!("failed to create '{}'", path.display()))?;
        }
        Err(err) => {
            return Err(err).with_context(|| format!("failed to stat '{}'", path.display()));
        }
    }
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to stat '{}'", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        bail!(
            "refusing to use unsafe private directory '{}'",
            path.display()
        );
    }
    set_mode(path, 0o700).with_context(|| format!("failed to chmod '{}'", path.display()))
}

fn ensure_private_context_dirs(paths: &ContextPaths) -> Result<()> {
    if let Some(services_dir) = paths.service_dir.parent() {
        ensure_private_dir(services_dir)?;
    }
    ensure_private_dir(&paths.service_dir)?;
    ensure_private_dir(&paths.secrets_dir)
}

fn write_private_file(path: &Path, content: &[u8], mode: u32) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                bail!(
                    "refusing to write private file through symlink '{}'",
                    path.display()
                );
            }
            if !file_type.is_file() {
                bail!(
                    "refusing to write private file over non-file '{}'",
                    path.display()
                );
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| format!("failed to stat '{}'", path.display()));
        }
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("failed to open '{}'", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to stat open file '{}'", path.display()))?;
    if !metadata.file_type().is_file() {
        bail!(
            "refusing to write private file over non-file '{}'",
            path.display()
        );
    }
    file.set_permissions(fs::Permissions::from_mode(mode))
        .with_context(|| format!("failed to chmod '{}'", path.display()))?;
    file.write_all(content)
        .with_context(|| format!("failed to write '{}'", path.display()))?;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .with_context(|| format!("failed to chmod '{}'", path.display()))?;
    Ok(())
}

mod tools;
use tools::*;

fn agentd_service_unit() -> &'static str {
    r#"[Unit]
Description=Confidential Agent Daemon
Wants=network-online.target attestation-agent.service confidential-data-hub-daemon.service trustiflux-api-server.service
After=network-online.target attestation-agent.service confidential-data-hub-daemon.service trustiflux-api-server.service

[Service]
Type=simple
Environment=RUST_LOG=info
ExecStart=/usr/local/bin/confidential-agentd run
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
"#
}

fn gateway_service_unit() -> &'static str {
    r#"[Unit]
Description=Confidential Agent Gateway
Wants=network-online.target
After=network-online.target
StartLimitIntervalSec=0

[Service]
Type=simple
Environment=RUST_LOG=info
Environment=CA_GATEWAY_CONFIG=/etc/cai/gateway.json
ExecStart=/usr/local/bin/cai-gateway serve --config /etc/cai/gateway.json
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
"#
}

fn tng_service_unit() -> &'static str {
    r#"[Unit]
Description=Trusted Network Gateway
Wants=network-online.target attestation-agent.service
After=network-online.target attestation-agent.service
StartLimitIntervalSec=0

[Service]
Type=simple
Environment=RUST_LOG=info
ExecStart=/usr/bin/tng launch --config-file /etc/tng/config.json
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
"#
}

fn guest_setup_script() -> &'static str {
    r#"#!/bin/bash
set -euo pipefail

install_attestation_challenge_client() {
    if [ -x /usr/bin/attestation-challenge-client ]; then
        return 0
    fi
    if [ ! -x /opt/confidential-agent/hack/attestation-challenge-client ]; then
        echo "staged attestation-challenge-client is missing" >&2
        exit 1
    fi
    install -m 0755 /opt/confidential-agent/hack/attestation-challenge-client /usr/bin/attestation-challenge-client
    if [ ! -x /usr/bin/attestation-challenge-client ]; then
        echo "attestation-challenge-client was installed but /usr/bin/attestation-challenge-client is missing" >&2
        exit 1
    fi
}
install_attestation_challenge_client
# Application logs stay inside the guest. Volatile storage also avoids writing
# business output to an unprotected log partition supplied by a base image.
install -d -m 0755 /etc/systemd/journald.conf.d
cat > /etc/systemd/journald.conf.d/99-confidential-agent.conf <<'EOF'
[Journal]
Storage=volatile
ForwardToConsole=no
ForwardToKMsg=no
ForwardToSyslog=no
EOF
expected_tng_nevra='trusted-network-gateway-2.9.2-1.al8.x86_64'
installed_tng_nevra="$(rpm -q trusted-network-gateway 2>/dev/null || true)"
if [ "$installed_tng_nevra" != "$expected_tng_nevra" ]; then
    echo "unexpected TNG package: got '$installed_tng_nevra', expected '$expected_tng_nevra'" >&2
    exit 1
fi
expected_tng_version='tng 2.9.2'
installed_tng_version="$(/usr/bin/tng --version | sed -n '1p')"
if [ "$installed_tng_version" != "$expected_tng_version" ]; then
    echo "unexpected TNG binary: got '$installed_tng_version', expected '$expected_tng_version'" >&2
    exit 1
fi
mkdir -p /etc/systemd/system-preset
cat > /etc/systemd/system-preset/00-confidential-agent-tng.preset <<'EOF'
disable trusted-network-gateway.service
EOF

if command -v ssh-keygen >/dev/null 2>&1 && systemctl list-unit-files sshd.service >/dev/null 2>&1; then
    ssh-keygen -A || true
    mkdir -p /etc/systemd/system/sshd.service.d
    cat > /etc/systemd/system/sshd.service.d/10-confidential-agent-debug.conf <<'EOF'
[Service]
ExecStartPre=/usr/bin/mkdir -p /run/sshd
ExecStartPre=/usr/bin/ssh-keygen -A
EOF
    systemctl enable sshd.service || true
fi

mkdir -p /etc/systemd/system/trusted-network-gateway.service.d
cat > /etc/systemd/system/trusted-network-gateway.service.d/10-confidential-agent-wait-aa.conf <<'EOF'
[Unit]
Wants=network-online.target attestation-agent.service
After=network-online.target attestation-agent.service
StartLimitIntervalSec=0

[Service]
Restart=always
RestartSec=5s
ExecStartPre=/bin/bash -c 'for i in $(seq 1 120); do if [ -S /run/confidential-containers/attestation-agent/attestation-agent.sock ]; then exit 0; fi; sleep 1; done; echo "attestation-agent socket is not ready" >&2; exit 1'
EOF

systemctl daemon-reload || true
systemctl disable trusted-network-gateway.service || true
systemctl reset-failed trusted-network-gateway.service || true
"#
}

fn cryptpilot_fde_config() -> &'static str {
    r#"[rootfs]
delta_location = "disk-persist"
delta_backend = "dm-snapshot"

[delta]
integrity = false

[delta.encrypt.exec]
command = "/bin/sh"
args = ["-c", "CA_SECRET_WAIT_TIMEOUT_SEC=210 /usr/local/bin/confidential-agentd initrd-fetch >/dev/console 2>&1 && /bin/cat /run/cai/secrets/disk_key"]
"#
}

#[cfg(test)]
mod tests;
