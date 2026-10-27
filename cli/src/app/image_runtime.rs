//! CA compatibility requirements carried in publisher-authenticated metadata.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImageRuntimeContract {
    schema_version: String,
    bootstrap_schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_variant: Option<String>,
    capabilities: Vec<String>,
    rootfs_integrity: bool,
    writable_layer_encryption: String,
    key_release: String,
    min_disk_gb: u32,
    app_services: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    configuration_schema: Option<String>,
    #[serde(default)]
    software_versions: BTreeMap<String, String>,
    #[serde(default)]
    guest_binary_sha256: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    rekor_entry_uuids: Vec<String>,
}

impl ImageRuntimeContract {
    pub(super) fn ensure_export_variant(&self, variant: &str) -> Result<()> {
        anyhow::ensure!(
            self.image_variant.as_deref() == Some(variant),
            "saved runtime contract does not match exported image variant '{variant}'; rebuild before exporting"
        );
        Ok(())
    }

    pub(super) fn set_rekor_entry_uuids(&mut self, uuids: Vec<String>) -> Result<()> {
        anyhow::ensure!(
            uuids.iter().all(|uuid| !uuid.is_empty()
                && uuid.len() <= 256
                && uuid.bytes().all(|byte| byte.is_ascii_hexdigit())),
            "invalid Rekor entry identifier in image runtime metadata"
        );
        self.rekor_entry_uuids = uuids;
        Ok(())
    }

    pub(super) fn rekor_entry_uuids(&self) -> Vec<String> {
        self.rekor_entry_uuids
            .iter()
            .filter(|uuid| {
                !uuid.is_empty()
                    && uuid.len() <= 256
                    && uuid.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            .cloned()
            .collect()
    }

    pub(super) fn for_build(spec: &AgentSpec, guest: &Path) -> Result<Self> {
        let output = Command::new(guest).arg("describe-runtime").output()
            .context("failed to inspect the guest runtime; update the cagent RPM guest binaries before building a new release")?;
        anyhow::ensure!(
            output.status.success(),
            "guest binary does not expose CA protocol capabilities; update the cagent RPM guest binaries"
        );
        let guest_runtime: serde_json::Value = serde_json::from_slice(&output.stdout).context(
            "guest binary returned invalid runtime capabilities; update the cagent RPM guest binaries",
        )?;
        anyhow::ensure!(
            guest_runtime["schema_version"] == "confidential-agent/guest-runtime/v1"
                && guest_runtime["bootstrap_schema"] == BOOTSTRAP_SCHEMA_VERSION,
            "guest binary uses an incompatible CA protocol"
        );
        let mut contract = if let Some(path) = &spec.build.runtime_contract {
            let bytes = fs::read(path).with_context(|| {
                format!("failed to read image runtime contract {}", path.display())
            })?;
            serde_json::from_slice::<Self>(&bytes)
                .with_context(|| format!("invalid image runtime contract {}", path.display()))?
        } else {
            let mut capabilities = vec![
                "external-fde-v1".into(),
                "resources-v1".into(),
                "mesh-client-ports-v1".into(),
                "mesh-client-ports-v2".into(),
            ];
            if spec.service.ssh {
                capabilities.push("instance-ssh-v1".into());
            }
            Self {
                schema_version: "confidential-agent/image-runtime/v1".into(),
                bootstrap_schema: BOOTSTRAP_SCHEMA_VERSION.into(),
                image_variant: Some(spec.image_variant().into()),
                capabilities,
                rootfs_integrity: true,
                writable_layer_encryption: "luks2".into(),
                key_release: "ca-attested".into(),
                min_disk_gb: spec.deployment_disk_gb()?,
                app_services: spec.service.app_service.iter().cloned().collect(),
                configuration_schema: None,
                software_versions: BTreeMap::new(),
                guest_binary_sha256: String::new(),
                rekor_entry_uuids: Vec::new(),
            }
        };
        contract.image_variant = Some(spec.image_variant().into());
        contract.rekor_entry_uuids.clear();
        contract.guest_binary_sha256 = sha256_file(guest)?;
        for capability in ["connect-ohttp-v1", "replica-peer-sharing-v1"] {
            if guest_runtime["capabilities"]
                .as_array()
                .is_some_and(|values| values.iter().any(|v| v == capability))
                && !contract.capabilities.iter().any(|v| v == capability)
            {
                contract.capabilities.push(capability.into());
            }
        }

        for capability in ["external-fde-v1", "resources-v1", "mesh-client-ports-v1"] {
            anyhow::ensure!(guest_runtime["capabilities"].as_array().is_some_and(|values| values.iter().any(|v| v == capability)),
                "guest binary does not support required capability {capability}; update the cagent RPM guest binaries");
        }
        for capability in [
            "application-runtime-v1",
            "instance-ssh-credentials-v1",
            "mesh-client-ports-v2",
        ] {
            if contract
                .capabilities
                .iter()
                .any(|value| value == capability)
            {
                anyhow::ensure!(guest_runtime["capabilities"].as_array().is_some_and(|values| values.iter().any(|value| value == capability)),
                    "guest binary does not support required capability {capability}; rebuild the CA guest binary");
            }
        }
        contract.software_versions.insert(
            "confidential-agentd".into(),
            guest_runtime["version"]
                .as_str()
                .filter(|v| !v.is_empty())
                .context("guest runtime did not report its software version")?
                .into(),
        );
        contract.validate_protocol()?;
        Ok(contract)
    }

    fn validate_protocol(&self) -> Result<()> {
        anyhow::ensure!(
            self.rekor_entry_uuids().len() == self.rekor_entry_uuids.len(),
            "invalid Rekor entry identifier in image runtime metadata"
        );
        anyhow::ensure!(
            self.schema_version == "confidential-agent/image-runtime/v1"
                && self.bootstrap_schema == BOOTSTRAP_SCHEMA_VERSION,
            "image runtime contract uses an incompatible CA protocol"
        );
        for required in ["external-fde-v1", "resources-v1", "mesh-client-ports-v1"] {
            anyhow::ensure!(
                self.capabilities.iter().any(|v| v == required),
                "image runtime contract lacks required capability {required}"
            );
        }
        anyhow::ensure!(self.rootfs_integrity && self.writable_layer_encryption == "luks2" && self.key_release == "ca-attested",
            "image runtime contract does not provide CA integrity, encrypted writable storage and attested key release");
        anyhow::ensure!(
            self.min_disk_gb > 0
                && self.guest_binary_sha256.len() == 64
                && self
                    .guest_binary_sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit()),
            "invalid image runtime disk or guest identity requirements"
        );
        Ok(())
    }

    pub(super) fn validate_deploy(
        &self,
        spec: &AgentSpec,
        security: &serde_json::Value,
    ) -> Result<()> {
        self.validate_protocol()?;
        if let Some(variant) = &self.image_variant {
            anyhow::ensure!(
                variant == spec.image_variant(),
                "published image variant differs from the requested deployment variant"
            );
        } else {
            anyhow::ensure!(
                !spec.deploys_debug_image(),
                "published debug image has no authenticated variant metadata; rebuild and export it before deploying"
            );
        }
        anyhow::ensure!(
            security.get("rootfs_verity").and_then(|v| v.as_bool()) == Some(true),
            "published CA image has no authenticated root filesystem integrity protection"
        );
        let disk_gb = spec
            .deploy
            .disk_gb
            .context("published images require explicit deploy.disk_gb")?;
        anyhow::ensure!(disk_gb >= self.min_disk_gb,
            "image requires at least {} GB of disk including its encrypted writable layer; deploy.disk_gb is {}", self.min_disk_gb, disk_gb);
        if spec
            .service
            .connect
            .iter()
            .any(|p| p.encryption() == EncryptionMode::Ohttp)
        {
            anyhow::ensure!(self.capabilities.iter().any(|v| v == "connect-ohttp-v1"),
                "published image does not support OHTTP; select an image built with a compatible guest runtime");
        }
        if spec.deploy.replicas > 1 {
            anyhow::ensure!(
                self.capabilities
                    .iter()
                    .any(|v| v == "replica-peer-sharing-v1"),
                "published image does not support service replicas; select a compatible image"
            );
        }
        if spec.service.ssh {
            anyhow::ensure!(
                self.capabilities.iter().any(|v| v == "instance-ssh-v1"),
                "image does not support CA instance SSH credentials"
            );
        }
        if let Some(service) = &spec.service.app_service {
            anyhow::ensure!(
                self.app_services.contains(service),
                "image does not support application service {service}"
            );
        }
        Ok(())
    }
    pub(super) fn supports_shared_ports(&self) -> bool {
        self.capabilities
            .iter()
            .any(|v| v == "mesh-client-ports-v2")
    }
}
