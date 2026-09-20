use super::*;

pub(super) fn context_paths(state_dir: &Path, service_id: &str) -> ContextPaths {
    let state_dir = absolute_path_for_state(state_dir);
    let service_dir = state_dir.join("services").join(service_id);
    let artifacts_dir = service_dir.join("artifacts");
    let shelter_work_dir = service_dir.join("shelter");
    ContextPaths {
        shelter_work_dir,
        artifacts_dir,
        cache_dir: service_dir.join("cache"),
        guest_staging_dir: service_dir.join("guest"),
        secrets_dir: service_dir.join("secrets"),
        rendered_config: service_dir.join("shelter.yaml"),
        manifest: service_dir.join("manifest.json"),
        bootstrap_file: service_dir.join("bootstrap.json"),
        service_state: service_dir.join("state.json"),
        agent_card: service_dir.join("agent-card.json"),
        service_dir,
    }
}

pub(super) fn shelter_build_result_path(work_dir: &Path, build_id: &str) -> PathBuf {
    work_dir
        .join("images")
        .join(build_id)
        .join("build-result.json")
}

pub(super) fn shelter_deploy_result_path(terraform_dir: &Path) -> PathBuf {
    terraform_dir.join("deploy-result.json")
}

pub(super) fn read_shelter_build_result(
    path: &Path,
    expected_id: &str,
) -> Result<ShelterBuildResult> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read Shelter build result '{}'", path.display()))?;
    let result: ShelterBuildResult = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse Shelter build result '{}'", path.display()))?;
    if result.id != expected_id {
        bail!(
            "Shelter build result '{}' contains id '{}', expected '{}'",
            path.display(),
            result.id,
            expected_id
        );
    }
    Ok(result)
}

pub(super) fn materialize_shelter_build_artifacts(
    paths: &ContextPaths,
    result_path: &Path,
    build_id: &str,
) -> Result<ShelterBuildArtifacts> {
    let result = read_shelter_build_result(result_path, build_id)?;
    fs::create_dir_all(&paths.service_dir)
        .with_context(|| format!("failed to create '{}'", paths.service_dir.display()))?;
    let sample_rv = write_json_artifact(
        result.reference_value.as_ref(),
        &paths.service_dir.join("shelter-reference-values.json"),
    )?;
    let rekor_meta = write_json_artifact(
        result.rekor_value.as_ref(),
        &paths.service_dir.join("shelter-rekor-meta.json"),
    )?;
    Ok(ShelterBuildArtifacts {
        image_path: result.image_path,
        sample_rv,
        rekor_meta,
    })
}

pub(super) fn write_json_artifact(
    value: Option<&serde_json::Value>,
    path: &Path,
) -> Result<Option<PathBuf>> {
    match value {
        Some(value) => {
            fs::write(path, serde_json::to_string_pretty(value)?)
                .with_context(|| format!("failed to write '{}'", path.display()))?;
            Ok(Some(path.to_path_buf()))
        }
        None => {
            if path.exists() {
                fs::remove_file(path)
                    .with_context(|| format!("failed to remove stale '{}'", path.display()))?;
            }
            Ok(None)
        }
    }
}

pub(super) fn absolute_path_for_state(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

pub(super) fn run_shelter(cli: &Cli, args: &mut [OsString]) -> Result<()> {
    let shelter_bin = effective_shelter_bin(cli);
    let sigstore_bin = ensure_sigstore_tool_wrappers(cli)?;
    let mut command = shelter_command_with_bin(&shelter_bin, args);
    command
        .env("PATH", path_with_prepended_dir(&sigstore_bin)?)
        .env("CA_TOOLS_IMAGE", &cli.tools_image);
    let status = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to execute '{}'", shelter_bin.display()))?;

    if !status.success() {
        bail!(
            "shelter exited with status {}; check rendered Shelter work directories under '{}'",
            status,
            cli.state_dir.display()
        );
    }
    Ok(())
}

fn path_with_prepended_dir(dir: &Path) -> Result<std::ffi::OsString> {
    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let mut parts = vec![dir.to_path_buf()];
    parts.extend(std::env::split_paths(&current_path));
    std::env::join_paths(parts).context("failed to construct PATH for Shelter")
}

fn shelter_command_with_bin(bin: &Path, args: &mut [OsString]) -> Command {
    let mut command = Command::new(bin);
    command.args(args);
    command
}

fn effective_shelter_bin(cli: &Cli) -> PathBuf {
    effective_shelter_bin_from_candidates(
        cli,
        &[
            PathBuf::from("/usr/bin/shelter"),
            PathBuf::from("/usr/local/bin/shelter"),
        ],
    )
}

pub(super) fn effective_shelter_bin_from_candidates(cli: &Cli, candidates: &[PathBuf]) -> PathBuf {
    if cli.shelter_bin != PathBuf::from("shelter") || std::env::var_os("CA_SHELTER_BIN").is_some() {
        return cli.shelter_bin.clone();
    }

    candidates
        .iter()
        .find(|candidate| candidate.is_file())
        .cloned()
        .unwrap_or_else(|| cli.shelter_bin.clone())
}

pub(super) fn resolve_deploy_observation(
    prepared: &PreparedConfig,
    spec: &AgentSpec,
) -> Result<DeployObservation> {
    let deploy_result =
        read_shelter_deploy_result(&prepared.deploy_result, &prepared.shelter_build_id)?;
    let public_ip = deploy_result_value_as_string(
        deploy_result
            .deploy
            .public_ip
            .as_ref()
            .or_else(|| deploy_result.deploy.outputs.get("public_ip")),
    );
    let private_ip = deploy_result_value_as_string(
        deploy_result
            .deploy
            .private_ip
            .as_ref()
            .or_else(|| deploy_result.deploy.outputs.get("private_ip")),
    )
    .or_else(|| spec.deploy.private_ip.clone());
    let instance_ids = deploy_result_string_list(deploy_result.deploy.outputs.get("instance_ids"));
    let instance_private_ips =
        deploy_result_string_list(deploy_result.deploy.outputs.get("instance_private_ips"));
    let slb_vip =
        deploy_result_value_as_string(deploy_result.deploy.outputs.get("slb_vip"));
    Ok(DeployObservation {
        instance_id: deploy_result_value_as_string(
            deploy_result
                .deploy
                .instance_id
                .as_ref()
                .or_else(|| deploy_result.deploy.outputs.get("instance_id")),
        ),
        security_group_id: deploy_result_value_as_string(
            deploy_result.deploy.outputs.get("security_group_id"),
        ),
        public_ip,
        private_ip,
        instance_ids,
        instance_private_ips,
        slb_vip,
    })
}

pub(super) fn read_shelter_deploy_result(
    path: &Path,
    expected_id: &str,
) -> Result<ShelterDeployResult> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read Shelter deploy result '{}'", path.display()))?;
    let result: ShelterDeployResult = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse Shelter deploy result '{}'", path.display()))?;
    if result.id != expected_id {
        bail!(
            "Shelter deploy result '{}' contains id '{}', expected '{}'",
            path.display(),
            result.id,
            expected_id
        );
    }
    Ok(result)
}

pub(super) fn deploy_result_value_as_string(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::String(value) => non_empty_string(value),
        serde_json::Value::Object(map) => map.get("value").and_then(|value| match value {
            serde_json::Value::String(value) => non_empty_string(value),
            other => non_empty_string(&other.to_string()),
        }),
        serde_json::Value::Null => None,
        other => non_empty_string(&other.to_string()),
    }
}

/// Extracts a list of non-empty strings from a Terraform-style deploy result output.
/// Handles both bare arrays (`["a","b"]`) and wrapped objects (`{"value": ["a","b"]}`).
pub(super) fn deploy_result_string_list(value: Option<&serde_json::Value>) -> Vec<String> {
    let array = match value {
        Some(serde_json::Value::Array(arr)) => arr,
        Some(serde_json::Value::Object(map)) => match map.get("value") {
            Some(serde_json::Value::Array(arr)) => arr,
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    array
        .iter()
        .filter_map(|item| match item {
            serde_json::Value::String(s) => non_empty_string(s),
            _ => None,
        })
        .collect()
}

pub(super) fn non_empty_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty() && trimmed != "null").then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn context_paths_structure() {
        let paths = context_paths(Path::new("/state"), "openclaw");
        assert_eq!(paths.service_dir, PathBuf::from("/state/services/openclaw"));
        assert_eq!(
            paths.shelter_work_dir,
            PathBuf::from("/state/services/openclaw/shelter")
        );
        assert_eq!(
            paths.secrets_dir,
            PathBuf::from("/state/services/openclaw/secrets")
        );
        assert_eq!(
            paths.service_state,
            PathBuf::from("/state/services/openclaw/state.json")
        );
        assert_eq!(
            paths.agent_card,
            PathBuf::from("/state/services/openclaw/agent-card.json")
        );
        assert_eq!(
            paths.manifest,
            PathBuf::from("/state/services/openclaw/manifest.json")
        );
        assert_eq!(
            paths.rendered_config,
            PathBuf::from("/state/services/openclaw/shelter.yaml")
        );
    }

    #[test]
    fn shelter_build_result_path_structure() {
        let path = shelter_build_result_path(Path::new("/work"), "build-123");
        assert_eq!(
            path,
            PathBuf::from("/work/images/build-123/build-result.json")
        );
    }

    #[test]
    fn shelter_deploy_result_path_structure() {
        let path = shelter_deploy_result_path(Path::new("/terraform"));
        assert_eq!(path, PathBuf::from("/terraform/deploy-result.json"));
    }

    #[test]
    fn read_shelter_build_result_parses_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("build-result.json");
        let data = json!({
            "id": "build-42",
            "image_path": "/images/disk.qcow2",
            "reference_value": null,
            "rekor_value": null
        });
        fs::write(&path, serde_json::to_string(&data).unwrap()).unwrap();
        let result = read_shelter_build_result(&path, "build-42").unwrap();
        assert_eq!(result.id, "build-42");
    }

    #[test]
    fn read_shelter_build_result_rejects_id_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("build-result.json");
        let data = json!({
            "id": "build-42",
            "image_path": "/images/disk.qcow2",
            "reference_value": null,
            "rekor_value": null
        });
        fs::write(&path, serde_json::to_string(&data).unwrap()).unwrap();
        let err = read_shelter_build_result(&path, "wrong-id").unwrap_err();
        assert!(err.to_string().contains("wrong-id"));
    }

    #[test]
    fn write_json_artifact_writes_some() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact.json");
        let value = json!({"key": "value"});
        let result = write_json_artifact(Some(&value), &path).unwrap();
        assert_eq!(result, Some(path.clone()));
        let content: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["key"], "value");
    }

    #[test]
    fn write_json_artifact_removes_stale_on_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact.json");
        fs::write(&path, "old").unwrap();
        assert!(path.exists());
        let result = write_json_artifact(None, &path).unwrap();
        assert!(result.is_none());
        assert!(!path.exists());
    }

    #[test]
    fn write_json_artifact_none_nonexistent_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact.json");
        assert!(write_json_artifact(None, &path).unwrap().is_none());
    }

    #[test]
    fn absolute_path_for_state_keeps_absolute() {
        assert_eq!(
            absolute_path_for_state(Path::new("/absolute")),
            PathBuf::from("/absolute")
        );
    }

    #[test]
    fn absolute_path_for_state_resolves_relative() {
        let result = absolute_path_for_state(Path::new("relative"));
        assert!(result.is_absolute());
    }

    #[test]
    fn effective_shelter_bin_prefers_existing_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let candidate = dir.path().join("shelter");
        fs::write(&candidate, "#!/bin/sh").unwrap();
        let cli = crate::cli::Cli {
            command: crate::cli::Commands::Version,
            shelter_bin: PathBuf::from("shelter"),
            state_dir: PathBuf::from("/state"),
            tools_image: "test".to_string(),
        };
        let result = effective_shelter_bin_from_candidates(&cli, &[candidate.clone()]);
        assert_eq!(result, candidate);
    }

    #[test]
    fn effective_shelter_bin_returns_cli_default_when_no_candidate() {
        let cli = crate::cli::Cli {
            command: crate::cli::Commands::Version,
            shelter_bin: PathBuf::from("shelter"),
            state_dir: PathBuf::from("/state"),
            tools_image: "test".to_string(),
        };
        let result =
            effective_shelter_bin_from_candidates(&cli, &[PathBuf::from("/nonexistent/shelter")]);
        assert_eq!(result, PathBuf::from("shelter"));
    }

    #[test]
    fn deploy_result_value_as_string_plain_string() {
        let v = json!("10.0.0.1");
        assert_eq!(
            deploy_result_value_as_string(Some(&v)),
            Some("10.0.0.1".to_string())
        );
    }

    #[test]
    fn deploy_result_value_as_string_object_with_value() {
        let v = json!({"value": "10.0.0.1"});
        assert_eq!(
            deploy_result_value_as_string(Some(&v)),
            Some("10.0.0.1".to_string())
        );
    }

    #[test]
    fn deploy_result_value_as_string_null_returns_none() {
        assert!(deploy_result_value_as_string(Some(&json!(null))).is_none());
        assert!(deploy_result_value_as_string(None).is_none());
    }

    #[test]
    fn deploy_result_value_as_string_number() {
        let v = json!(42);
        assert_eq!(
            deploy_result_value_as_string(Some(&v)),
            Some("42".to_string())
        );
    }

    #[test]
    fn non_empty_string_filters() {
        assert_eq!(non_empty_string("hello"), Some("hello".to_string()));
        assert!(non_empty_string("").is_none());
        assert!(non_empty_string("  ").is_none());
        assert!(non_empty_string("null").is_none());
    }
}
