use super::*;
use std::os::unix::fs::OpenOptionsExt;

const PROVIDER_ALIYUN: &str = "aliyun";
const STATUS_UPLOADING: &str = "uploading";
const STATUS_UPLOADED: &str = "uploaded";
const STATUS_IMPORTING: &str = "importing";
const STATUS_AVAILABLE: &str = "available";
const STATUS_FAILED: &str = "failed";

fn publish_key(region: &str, variant: &str, build_id: &str, source_sha256: &str) -> String {
    format!(
        "{PROVIDER_ALIYUN}/{region}/{variant}/{build_id}/{}",
        &source_sha256[..12]
    )
}

fn publish_bucket_name(region: &str, state_dir: &Path) -> Result<String> {
    let root = absolute_path_for_state(state_dir);
    let _lock = lock_operation_file(&root.join("publication-namespace.lock"))?;
    let path = root.join("publication-namespace.json");
    // OSS names are global. A filesystem path (often identical on every host)
    // is not a workspace identity. Persist a random namespace so moving a
    // workspace keeps its bucket while a clean host gets an independent name.
    let namespace: String = if path.exists() {
        serde_json::from_slice(&fs::read(&path)?).context("invalid publication namespace")?
    } else {
        let mut bytes = [0u8; 16];
        OsRng.fill_bytes(&mut bytes);
        let value = hex_encode(&bytes);
        write_json_atomic(&path, &value)?;
        value
    };
    anyhow::ensure!(
        namespace.len() == 32 && namespace.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid publication namespace; restore the workspace metadata"
    );
    let hash = hex_encode(&Sha256::digest(namespace.as_bytes()));
    Ok(format!("ca-images-{region}-{}", &hash[..20]))
}

fn publish_object_key(
    service_id: &str,
    variant: &str,
    build_id: &str,
    source_sha256: &str,
) -> String {
    format!(
        "confidential-agent/images/{service_id}/{variant}/{build_id}/{}.qcow2",
        &source_sha256[..16]
    )
}

fn publish_image_name(
    service_id: &str,
    variant: &str,
    build_id: &str,
    source_sha256: &str,
) -> String {
    let short_sha = &source_sha256[..12];
    let raw = format!("ca-pub-{service_id}-{variant}-{build_id}-{short_sha}");
    let mut name = raw
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .collect::<String>();
    if name.len() > 128 {
        let suffix = format!("-{short_sha}");
        name.truncate(128 - suffix.len());
        name.push_str(&suffix);
    }
    name
}

fn cloud_credential_envs(region: &str) -> Result<Vec<(String, String)>> {
    let mut envs = super::cloud_credentials::load()?.environment();
    envs.push(("ALIBABA_CLOUD_REGION".to_string(), region.to_string()));
    envs.extend(inherited_proxy_envs(None));
    Ok(envs)
}

struct TempOssutilConfig {
    path: PathBuf,
    endpoint: String,
}

impl Drop for TempOssutilConfig {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn ossutil_config(cli: &Cli, region: &str) -> Result<TempOssutilConfig> {
    let super::cloud_credentials::Credentials {
        access_key,
        secret_key,
        sts_token,
    } = super::cloud_credentials::load()?;

    let dir = absolute_path_for_state(&cli.state_dir)
        .join("tmp")
        .join("ossutil");
    fs::create_dir_all(&dir).with_context(|| format!("failed to create '{}'", dir.display()))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to chmod '{}'", dir.display()))?;
    let path = dir.join(format!(
        "ossutil-{}-{}.conf",
        std::process::id(),
        current_nanos()
    ));
    let setting = std::env::var("CA_IMAGE_OSS_ENDPOINT").ok();
    let endpoint = super::oss_endpoint::resolve(region, setting.as_deref())?;
    println!("[ca] image OSS endpoint: {endpoint}");

    let mut content = format!(
        "[Credentials]\nlanguage=EN\nendpoint={endpoint}\naccessKeyID={access_key}\naccessKeySecret={secret_key}\n"
    );
    if let Some(token) = sts_token {
        content.push_str(&format!("stsToken={token}\n"));
    }

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("failed to create '{}'", path.display()))?;
    file.write_all(content.as_bytes())
        .with_context(|| format!("failed to write '{}'", path.display()))?;
    Ok(TempOssutilConfig { path, endpoint })
}

fn image_transfer_env(endpoint: &str) -> Vec<(String, String)> {
    let mut environment = inherited_proxy_envs(None);
    let overrides = super::oss_endpoint::proxy_overrides(endpoint);
    environment.retain(|(name, _)| !overrides.iter().any(|(key, _)| key == name));
    environment.extend(overrides);
    environment
}

fn current_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

fn run_native_tool_capture(cli: &Cli, spec: NativeToolSpec) -> Result<String> {
    let output = run_native_tool_output(&cli.tools_dir, spec)?;
    anyhow::ensure!(
        output.status.success(),
        "native tool exited with status {}; stderr: {}; stdout: {}",
        output.status,
        summarize_command_bytes(&output.stderr),
        summarize_command_bytes(&output.stdout)
    );
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_aliyun_cli(cli: &Cli, args: Vec<String>, region: &str) -> Result<String> {
    let workdir = std::env::current_dir().context("failed to resolve current working directory")?;
    let state_dir = absolute_path_for_state(&cli.state_dir);
    fs::create_dir_all(&state_dir)
        .with_context(|| format!("failed to create '{}'", state_dir.display()))?;
    let mut tool_args = vec![OsString::from("--region"), OsString::from(region)];
    tool_args.extend(args.into_iter().map(OsString::from));
    run_native_tool_capture(
        cli,
        NativeToolSpec {
            tool: "aliyun",
            tool_args,

            envs: cloud_credential_envs(region)?,
            workdir: Some(workdir),
        },
    )
}

fn run_ossutil(cli: &Cli, args: Vec<String>, region: &str) -> Result<()> {
    let config = ossutil_config(cli, region)?;
    let mut tool_args = vec![
        OsString::from("-c"),
        config.path.as_os_str().to_os_string(),
        OsString::from("-e"),
        OsString::from(&config.endpoint),
    ];
    tool_args.extend(args.into_iter().map(OsString::from));
    run_host_tool(
        cli,
        "ossutil64",
        tool_args,
        image_transfer_env(&config.endpoint),
        true,
    )
}

fn run_ossutil_capture(cli: &Cli, args: Vec<String>, region: &str) -> Result<String> {
    let config = ossutil_config(cli, region)?;
    let mut tool_args = vec![
        OsString::from("-c"),
        config.path.as_os_str().to_os_string(),
        OsString::from("-e"),
        OsString::from(&config.endpoint),
    ];
    tool_args.extend(args.into_iter().map(OsString::from));

    let workdir = std::env::current_dir().context("failed to resolve current working directory")?;
    let state_dir = absolute_path_for_state(&cli.state_dir);
    fs::create_dir_all(&state_dir)
        .with_context(|| format!("failed to create '{}'", state_dir.display()))?;

    run_native_tool_capture(
        cli,
        NativeToolSpec {
            tool: "ossutil64",
            tool_args,

            envs: image_transfer_env(&config.endpoint),
            workdir: Some(workdir),
        },
    )
}

fn validate_cloud_credentials(cli: &Cli, region: &str) -> Result<()> {
    println!("[ca] validating cloud credentials...");
    run_aliyun_cli(
        cli,
        vec![
            "ecs".to_string(),
            "DescribeRegions".to_string(),
            "--RegionId".to_string(),
            region.to_string(),
        ],
        region,
    )?;
    Ok(())
}

fn ensure_publish_bucket(cli: &Cli, bucket: &str, region: &str) -> Result<()> {
    let check = run_ossutil_capture(
        cli,
        vec!["stat".to_string(), format!("oss://{bucket}")],
        region,
    );
    if check.is_ok() {
        return Ok(());
    }
    println!("[ca] creating OSS bucket {bucket}...");
    run_ossutil(
        cli,
        vec!["mb".to_string(), format!("oss://{bucket}")],
        region,
    )
}

fn find_existing_published_image(
    cli: &Cli,
    region: &str,
    service_id: &str,
    variant: &str,
    build_id: &str,
    source_sha256: &str,
) -> Result<Option<String>> {
    let output = run_aliyun_cli(
        cli,
        vec![
            "ecs".to_string(),
            "DescribeImages".to_string(),
            "--RegionId".to_string(),
            region.to_string(),
            "--ImageOwnerAlias".to_string(),
            "self".to_string(),
            "--Status".to_string(),
            "Available".to_string(),
            "--Tag.1.Key".to_string(),
            "ca-managed".to_string(),
            "--Tag.1.Value".to_string(),
            "true".to_string(),
            "--Tag.2.Key".to_string(),
            "ca-service-id".to_string(),
            "--Tag.2.Value".to_string(),
            service_id.to_string(),
            "--Tag.3.Key".to_string(),
            "ca-build-id".to_string(),
            "--Tag.3.Value".to_string(),
            build_id.to_string(),
            "--Tag.4.Key".to_string(),
            "ca-variant".to_string(),
            "--Tag.4.Value".to_string(),
            variant.to_string(),
            "--Tag.5.Key".to_string(),
            "ca-source-sha256".to_string(),
            "--Tag.5.Value".to_string(),
            source_sha256.to_string(),
        ],
        region,
    )?;
    let resp: serde_json::Value =
        serde_json::from_str(&output).context("failed to parse DescribeImages response")?;
    Ok(resp["Images"]["Image"]
        .as_array()
        .and_then(|images| {
            images
                .iter()
                .find(|image| image["Features"]["NvmeSupport"].as_str() == Some("supported"))
        })
        .and_then(|image| image["ImageId"].as_str())
        .map(str::to_string))
}

fn describe_image_status(cli: &Cli, region: &str, image_id: &str) -> Result<Option<String>> {
    let output = run_aliyun_cli(
        cli,
        vec![
            "ecs".to_string(),
            "DescribeImages".to_string(),
            "--RegionId".to_string(),
            region.to_string(),
            "--ImageId".to_string(),
            image_id.to_string(),
            "--Status".to_string(),
            "ALL".to_string(),
            "--ImageOwnerAlias".to_string(),
            "self".to_string(),
        ],
        region,
    )?;
    let resp: serde_json::Value =
        serde_json::from_str(&output).context("failed to parse DescribeImages response")?;
    Ok(resp["Images"]["Image"]
        .as_array()
        .and_then(|images| images.first())
        .and_then(|image| image["Status"].as_str())
        .map(str::to_string))
}

fn cleanup_oss_object(cli: &Cli, bucket: &str, object_key: &str, region: &str) -> Result<()> {
    run_ossutil(
        cli,
        vec![
            "rm".to_string(),
            format!("oss://{bucket}/{object_key}"),
            "--force".to_string(),
        ],
        region,
    )
}

fn delete_cloud_image(cli: &Cli, region: &str, image_id: &str, force: bool) -> Result<()> {
    run_aliyun_cli(
        cli,
        vec![
            "ecs".to_string(),
            "DeleteImage".to_string(),
            "--RegionId".to_string(),
            region.to_string(),
            "--ImageId".to_string(),
            image_id.to_string(),
            "--Force".to_string(),
            force.to_string(),
        ],
        region,
    )?;
    Ok(())
}

fn image_snapshot_ids(cli: &Cli, region: &str, image_id: &str) -> Result<Vec<String>> {
    let output = run_aliyun_cli(
        cli,
        vec![
            "ecs".into(),
            "DescribeImages".into(),
            "--RegionId".into(),
            region.into(),
            "--ImageId".into(),
            image_id.into(),
            "--Status".into(),
            "ALL".into(),
            "--ImageOwnerAlias".into(),
            "self".into(),
        ],
        region,
    )?;
    let value: serde_json::Value = serde_json::from_str(&output)?;
    let images = value["Images"]["Image"]
        .as_array()
        .context("invalid cloud image inventory")?;
    let Some(image) = images.first() else {
        return Ok(Vec::new());
    };
    anyhow::ensure!(
        image["ImageId"] == image_id,
        "snapshot inventory belongs to a different image"
    );
    let ids = image["DiskDeviceMappings"]["DiskDeviceMapping"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|disk| disk["SnapshotId"].as_str())
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    anyhow::ensure!(image["Status"] != "Available" || !ids.is_empty(), "available imported image has no recorded backing snapshots; refusing to lose cleanup custody");
    Ok(ids)
}

fn delete_import_snapshot(cli: &Cli, region: &str, snapshot: &str) -> Result<()> {
    run_aliyun_cli(
        cli,
        vec![
            "ecs".into(),
            "DeleteSnapshot".into(),
            "--SnapshotId".into(),
            snapshot.into(),
            "--Force".into(),
            "false".into(),
        ],
        region,
    )?;
    Ok(())
}

struct ImportImageResponse {
    image_id: String,
    task_id: Option<String>,
}

trait PublishCloudOps {
    fn image_import_timeout(&self) -> Duration {
        Duration::from_secs(
            std::env::var("CA_IMAGE_IMPORT_TIMEOUT_SEC")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(7200),
        )
    }

    fn image_import_poll_interval(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn validate_credentials(&self, cli: &Cli, region: &str) -> Result<()>;

    fn find_existing_published_image(
        &self,
        cli: &Cli,
        region: &str,
        service_id: &str,
        variant: &str,
        build_id: &str,
        source_sha256: &str,
    ) -> Result<Option<String>>;

    fn describe_image_status(
        &self,
        cli: &Cli,
        region: &str,
        image_id: &str,
    ) -> Result<Option<String>>;

    fn ensure_publish_bucket(&self, cli: &Cli, bucket: &str, region: &str) -> Result<()>;

    fn upload_image(
        &self,
        cli: &Cli,
        image_path: &Path,
        bucket: &str,
        object_key: &str,
        region: &str,
    ) -> Result<()>;

    fn import_image(
        &self,
        cli: &Cli,
        request: ImportImageRequest<'_>,
    ) -> Result<ImportImageResponse>;

    fn cleanup_oss_object(
        &self,
        cli: &Cli,
        bucket: &str,
        object_key: &str,
        region: &str,
    ) -> Result<()>;

    fn delete_cloud_image(
        &self,
        cli: &Cli,
        region: &str,
        image_id: &str,
        force: bool,
    ) -> Result<()>;
    fn image_snapshot_ids(&self, cli: &Cli, region: &str, image_id: &str) -> Result<Vec<String>>;
    fn delete_import_snapshot(&self, cli: &Cli, region: &str, snapshot: &str) -> Result<()>;
}

struct RealPublishCloudOps;

impl PublishCloudOps for RealPublishCloudOps {
    fn validate_credentials(&self, cli: &Cli, region: &str) -> Result<()> {
        validate_cloud_credentials(cli, region)
    }

    fn find_existing_published_image(
        &self,
        cli: &Cli,
        region: &str,
        service_id: &str,
        variant: &str,
        build_id: &str,
        source_sha256: &str,
    ) -> Result<Option<String>> {
        find_existing_published_image(cli, region, service_id, variant, build_id, source_sha256)
    }

    fn describe_image_status(
        &self,
        cli: &Cli,
        region: &str,
        image_id: &str,
    ) -> Result<Option<String>> {
        describe_image_status(cli, region, image_id)
    }

    fn ensure_publish_bucket(&self, cli: &Cli, bucket: &str, region: &str) -> Result<()> {
        ensure_publish_bucket(cli, bucket, region)
    }

    fn upload_image(
        &self,
        cli: &Cli,
        image_path: &Path,
        bucket: &str,
        object_key: &str,
        region: &str,
    ) -> Result<()> {
        run_ossutil(
            cli,
            vec![
                "cp".to_string(),
                image_path.to_string_lossy().to_string(),
                format!("oss://{bucket}/{object_key}"),
                "--force".to_string(),
            ],
            region,
        )
    }

    fn import_image(
        &self,
        cli: &Cli,
        request: ImportImageRequest<'_>,
    ) -> Result<ImportImageResponse> {
        let region = request.region;
        let output = run_aliyun_cli(cli, import_image_args(request), region)?;
        let resp: serde_json::Value =
            serde_json::from_str(&output).context("failed to parse ImportImage response")?;
        let image_id = resp["ImageId"]
            .as_str()
            .context("ImportImage response missing ImageId")?
            .to_string();
        Ok(ImportImageResponse {
            image_id,
            task_id: resp["TaskId"].as_str().map(str::to_string),
        })
    }

    fn cleanup_oss_object(
        &self,
        cli: &Cli,
        bucket: &str,
        object_key: &str,
        region: &str,
    ) -> Result<()> {
        cleanup_oss_object(cli, bucket, object_key, region)
    }

    fn delete_cloud_image(
        &self,
        cli: &Cli,
        region: &str,
        image_id: &str,
        force: bool,
    ) -> Result<()> {
        delete_cloud_image(cli, region, image_id, force)
    }
    fn image_snapshot_ids(&self, cli: &Cli, region: &str, image_id: &str) -> Result<Vec<String>> {
        image_snapshot_ids(cli, region, image_id)
    }
    fn delete_import_snapshot(&self, cli: &Cli, region: &str, snapshot: &str) -> Result<()> {
        delete_import_snapshot(cli, region, snapshot)
    }
}

struct PublishEntrySeed<'a> {
    region: &'a str,
    variant: &'a str,
    build_id: &'a str,
    source_sha256: &'a str,
    source_size: u64,
    bucket: &'a str,
    object_key: &'a str,
    image_name: &'a str,
}

fn published_base_entry(seed: PublishEntrySeed<'_>) -> PublishedImage {
    let now = current_utc_timestamp();
    PublishedImage {
        provider: PROVIDER_ALIYUN.to_string(),
        region: seed.region.to_string(),
        variant: seed.variant.to_string(),
        build_id: seed.build_id.to_string(),
        source_sha256: seed.source_sha256.to_string(),
        source_size: seed.source_size,
        status: STATUS_UPLOADING.to_string(),
        image_name: seed.image_name.to_string(),
        image_id: None,
        import_task_id: None,
        bucket: Some(seed.bucket.to_string()),
        object_key: Some(seed.object_key.to_string()),
        created_at: now.clone(),
        updated_at: now,
        oss_cleaned: false,
        exported: false,
        owned: true,
        import_token: new_import_token(),
        retiring: false,
        import_snapshot_ids: Vec::new(),
        error: None,
    }
}

fn new_import_token() -> String {
    let mut bytes = [0u8; 24];
    OsRng.fill_bytes(&mut bytes);
    hex_encode(&bytes)
}

fn persist_published(
    cli: &Cli,
    state: &mut LocalServiceState,
    key: &str,
    entry: PublishedImage,
) -> Result<()> {
    with_state_dir_lock(&cli.state_dir, || {
        let mut latest = read_service_state_file(
            &context_paths(&cli.state_dir, &state.service_id).service_state,
        )?
        .unwrap_or_else(|| state.clone());
        let previous_id = latest
            .build
            .published
            .get(key)
            .and_then(|p| p.image_id.clone());
        if entry.retiring
            && entry.image_id.is_none()
            && matches!(latest.phase.as_str(), "built" | "deleted")
            && previous_id.is_some()
            && latest.deploy.published_image_id == previous_id
        {
            latest.deploy.published_image_id = None;
        }
        latest.build.published.insert(key.to_string(), entry);
        write_local_service_state(&cli.state_dir, &latest)?;
        *state = latest;
        Ok(())
    })
}

pub(super) fn mark_exported(cli: &Cli, state: &mut LocalServiceState, key: &str) -> Result<()> {
    let mut entry = state.build.published[key].clone();
    entry.exported = true;
    persist_published(cli, state, key, entry)
}

fn remove_publication(cli: &Cli, state: &mut LocalServiceState, key: &str) -> Result<()> {
    with_state_dir_lock(&cli.state_dir, || {
        let mut latest = read_service_state_file(
            &context_paths(&cli.state_dir, &state.service_id).service_state,
        )?
        .context("publication custody disappeared during retirement")?;
        latest.build.published.remove(key);
        write_local_service_state(&cli.state_dir, &latest)?;
        *state = latest;
        Ok(())
    })
}

fn capture_import_snapshots(
    cli: &Cli,
    ops: &impl PublishCloudOps,
    entry: &mut PublishedImage,
) -> Result<()> {
    if entry.owned {
        if let Some(image_id) = &entry.image_id {
            let mut ids = entry
                .import_snapshot_ids
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>();
            ids.extend(ops.image_snapshot_ids(cli, &entry.region, image_id)?);
            entry.import_snapshot_ids = ids.into_iter().collect();
        }
    }
    Ok(())
}

fn cleanup_upload(cli: &Cli, ops: &impl PublishCloudOps, entry: &mut PublishedImage) {
    if entry.oss_cleaned {
        return;
    }
    if let (Some(bucket), Some(key)) = (&entry.bucket, &entry.object_key) {
        match ops.cleanup_oss_object(cli, bucket, key, &entry.region) {
            Ok(()) => {
                entry.oss_cleaned = true;
                entry.error = None;
            }
            Err(error) => {
                entry.error = Some(format!("OSS cleanup pending: {error:#}"));
                eprintln!(
                    "[ca] warning: {}; retry publish or image prune",
                    entry.error.as_ref().unwrap()
                );
            }
        }
    }
}

fn update_entry_status(entry: &mut PublishedImage, status: &str, error: Option<String>) {
    entry.status = status.to_string();
    entry.error = error;
    entry.updated_at = current_utc_timestamp();
}

struct ImportImageRequest<'a> {
    client_token: &'a str,
    region: &'a str,
    image_name: &'a str,
    bucket: &'a str,
    object_key: &'a str,
    service_id: &'a str,
    build_id: &'a str,
    variant: &'a str,
    source_sha256: &'a str,
}

fn import_image_args(request: ImportImageRequest<'_>) -> Vec<String> {
    vec![
        "ecs".to_string(),
        "ImportImage".to_string(),
        "--ClientToken".to_string(),
        request.client_token.to_string(),
        "--RegionId".to_string(),
        request.region.to_string(),
        "--ImageName".to_string(),
        request.image_name.to_string(),
        "--OSType".to_string(),
        "linux".to_string(),
        "--Platform".to_string(),
        "Aliyun".to_string(),
        "--Architecture".to_string(),
        "x86_64".to_string(),
        "--BootMode".to_string(),
        "UEFI".to_string(),
        "--Features.NvmeSupport".to_string(),
        "supported".to_string(),
        "--DiskDeviceMapping.1.OSSBucket".to_string(),
        request.bucket.to_string(),
        "--DiskDeviceMapping.1.OSSObject".to_string(),
        request.object_key.to_string(),
        "--DiskDeviceMapping.1.Format".to_string(),
        "qcow2".to_string(),
        "--Tag.1.Key".to_string(),
        "ca-managed".to_string(),
        "--Tag.1.Value".to_string(),
        "true".to_string(),
        "--Tag.2.Key".to_string(),
        "ca-service-id".to_string(),
        "--Tag.2.Value".to_string(),
        request.service_id.to_string(),
        "--Tag.3.Key".to_string(),
        "ca-build-id".to_string(),
        "--Tag.3.Value".to_string(),
        request.build_id.to_string(),
        "--Tag.4.Key".to_string(),
        "ca-variant".to_string(),
        "--Tag.4.Value".to_string(),
        request.variant.to_string(),
        "--Tag.5.Key".to_string(),
        "ca-source-sha256".to_string(),
        "--Tag.5.Value".to_string(),
        request.source_sha256.to_string(),
        "--force".to_string(),
    ]
}

#[allow(clippy::too_many_arguments)]
fn publish_variant_image(
    cli: &Cli,
    state: &mut LocalServiceState,
    key: &str,
    image_path: &Path,
    region: &str,
    service_id: &str,
    variant: &str,
    build_id: &str,
    source_sha256: &str,
    source_size: u64,
    wait: bool,
) -> Result<PublishedImage> {
    publish_variant_image_with_ops(
        cli,
        &RealPublishCloudOps,
        state,
        key,
        image_path,
        region,
        service_id,
        variant,
        build_id,
        source_sha256,
        source_size,
        wait,
    )
}

fn wait_for_image_available_with_ops(
    cli: &Cli,
    ops: &impl PublishCloudOps,
    region: &str,
    image_id: &str,
) -> Result<()> {
    let timeout = ops.image_import_timeout();
    let interval = ops.image_import_poll_interval();
    let started = Instant::now();
    let mut transient_failures = 0;
    loop {
        let status = match ops.describe_image_status(cli, region, image_id) {
            Ok(status) => {
                transient_failures = 0;
                status
            }
            Err(error) if transient_failures < 2 && transient_image_status_error(&error) => {
                transient_failures += 1;
                eprintln!("[ca] image status query had a transient failure; retrying ({transient_failures}/2)");
                None
            }
            Err(error) => return Err(error),
        };
        match status.as_deref() {
            Some("Available") => return Ok(()),
            Some("Creating" | "Waiting") | None => {}
            Some("CreateFailed") => bail!("ECS image import failed for {image_id}"),
            Some(other) => eprintln!("[ca] image {image_id} status: {other}"),
        }
        if started.elapsed() >= timeout {
            bail!(
                "timed out after {}s waiting for image {image_id} to become Available",
                timeout.as_secs()
            );
        }
        if !interval.is_zero() {
            thread::sleep(interval);
        }
    }
}

fn transient_image_status_error(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    [
        "context deadline exceeded",
        "client.timeout",
        "connection reset",
        "tls handshake timeout",
        "temporary failure in name resolution",
        "i/o timeout",
        "request timed out",
        "serviceunavailable",
        "throttling",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

#[allow(clippy::too_many_arguments)]
fn publish_variant_image_with_ops(
    cli: &Cli,
    ops: &impl PublishCloudOps,
    state: &mut LocalServiceState,
    key: &str,
    image_path: &Path,
    region: &str,
    service_id: &str,
    variant: &str,
    build_id: &str,
    source_sha256: &str,
    source_size: u64,
    wait: bool,
) -> Result<PublishedImage> {
    // Existing uploads/imports retain their exact object custody across upgrades.
    let previous = state.build.published.get(key);
    let bucket = match previous.and_then(|entry| entry.bucket.clone()) {
        Some(bucket) => bucket,
        None => publish_bucket_name(region, &cli.state_dir)?,
    };
    let object_key = previous
        .and_then(|entry| entry.object_key.clone())
        .unwrap_or_else(|| publish_object_key(service_id, variant, build_id, source_sha256));
    let image_name = publish_image_name(service_id, variant, build_id, source_sha256);
    let mut entry = state.build.published.get(key).cloned().unwrap_or_else(|| {
        published_base_entry(PublishEntrySeed {
            region,
            variant,
            build_id,
            source_sha256,
            source_size,
            bucket: &bucket,
            object_key: &object_key,
            image_name: &image_name,
        })
    });

    anyhow::ensure!(
        !entry.retiring,
        "publication retirement is pending; finish image unpublish before publishing again"
    );
    if entry.import_token.is_empty() {
        entry.import_token = new_import_token();
    }
    ops.validate_credentials(cli, region)?;

    if let Some(image_id) = entry.image_id.clone() {
        match ops
            .describe_image_status(cli, region, &image_id)?
            .as_deref()
        {
            Some("Available") => {
                update_entry_status(&mut entry, STATUS_AVAILABLE, None);
                capture_import_snapshots(cli, ops, &mut entry)?;
                cleanup_upload(cli, ops, &mut entry);
                persist_published(cli, state, key, entry.clone())?;
                return Ok(entry);
            }
            Some("CreateFailed") | None => {
                update_entry_status(&mut entry, STATUS_FAILED, Some("recorded image is missing or import failed; explicitly unpublish before retrying".into()));
                persist_published(cli, state, key, entry.clone())?;
                bail!("recorded image {image_id} is missing or failed; use image unpublish to retire this publication first");
            }
            _ => {
                update_entry_status(&mut entry, STATUS_IMPORTING, None);
                persist_published(cli, state, key, entry.clone())?;
                if !wait {
                    return Ok(entry);
                }
                wait_for_image_available_with_ops(cli, ops, region, &image_id)?;
                update_entry_status(&mut entry, STATUS_AVAILABLE, None);
                capture_import_snapshots(cli, ops, &mut entry)?;
                cleanup_upload(cli, ops, &mut entry);
                persist_published(cli, state, key, entry.clone())?;
                return Ok(entry);
            }
        }
    }

    if let Some(existing_id) = ops.find_existing_published_image(
        cli,
        region,
        service_id,
        variant,
        build_id,
        source_sha256,
    )? {
        entry.image_id = Some(existing_id.clone());
        // Finding matching bits does not establish deletion custody or external retention.
        entry.owned = false;
        if state.build.published.contains_key(key) {
            capture_import_snapshots(cli, ops, &mut entry)?;
            cleanup_upload(cli, ops, &mut entry);
        } else {
            entry.oss_cleaned = true;
        }
        update_entry_status(&mut entry, STATUS_AVAILABLE, None);
        persist_published(cli, state, key, entry.clone())?;
        println!("[ca] adopted existing published image: {existing_id}");
        return Ok(entry);
    }

    if entry.status != STATUS_UPLOADED {
        // A previous unpublish attempt may have cleaned the old object. A new
        // upload acquires it again, including when upload/import later fails.
        entry.oss_cleaned = false;
        update_entry_status(&mut entry, STATUS_UPLOADING, None);
        persist_published(cli, state, key, entry.clone())?;
        println!("[ca] uploading image to OSS ({source_size} bytes)...");
        ops.ensure_publish_bucket(cli, &bucket, region)?;
        ops.upload_image(cli, image_path, &bucket, &object_key, region)?;
        update_entry_status(&mut entry, STATUS_UPLOADED, None);
        persist_published(cli, state, key, entry.clone())?;
    }

    persist_published(cli, state, key, entry.clone())?;
    println!("[ca] importing ECS image '{image_name}'...");
    let imported = ops.import_image(
        cli,
        ImportImageRequest {
            client_token: &entry.import_token,
            region,
            image_name: &image_name,
            bucket: &bucket,
            object_key: &object_key,
            service_id,
            build_id,
            variant,
            source_sha256,
        },
    )?;
    let image_id = imported.image_id;
    entry.image_id = Some(image_id.clone());
    entry.import_task_id = imported.task_id;
    update_entry_status(&mut entry, STATUS_IMPORTING, None);
    persist_published(cli, state, key, entry.clone())?;
    println!("[ca] import started: image_id={image_id}");

    if !wait {
        return Ok(entry);
    }

    println!("[ca] waiting for image import to complete...");
    if let Err(err) = wait_for_image_available_with_ops(cli, ops, region, &image_id) {
        let status = match ops.describe_image_status(cli, region, &image_id) {
            Ok(Some(status)) if status == "CreateFailed" => STATUS_FAILED,
            _ => STATUS_IMPORTING,
        };
        update_entry_status(&mut entry, status, Some(err.to_string()));
        persist_published(cli, state, key, entry.clone())?;
        return Err(err);
    }
    update_entry_status(&mut entry, STATUS_AVAILABLE, None);
    capture_import_snapshots(cli, ops, &mut entry)?;
    cleanup_upload(cli, ops, &mut entry);
    persist_published(cli, state, key, entry.clone())?;
    println!("[ca] image published: image_id={image_id} region={region}");
    Ok(entry)
}

pub(super) fn published_image_for_deploy(
    state: &LocalServiceState,
    spec: &AgentSpec,
    variant: &BuildManifestVariant,
) -> Option<String> {
    state
        .build
        .published
        .values()
        .filter(|entry| {
            entry.provider == PROVIDER_ALIYUN
                && entry.region == spec.deploy.region
                && entry.variant == spec.image_variant()
                && entry.build_id == variant.shelter_build_id
                && entry.status == STATUS_AVAILABLE
                && !entry.retiring
                && entry.image_id.is_some()
        })
        .find_map(|entry| {
            let result =
                read_shelter_build_result(&variant.build_result, &variant.shelter_build_id).ok()?;
            if result.image_path.exists() {
                let source_sha256 = match sha256_file(&result.image_path) {
                    Ok(value) => value,
                    Err(err) => {
                        eprintln!(
                            "[ca] warning: failed to hash local image '{}': {err:#}; skipping published image",
                            result.image_path.display()
                        );
                        return None;
                    }
                };
                if source_sha256 != entry.source_sha256 {
                    eprintln!(
                        "[ca] warning: published image source hash no longer matches local image; skipping published image"
                    );
                    return None;
                }
            }
            entry.image_id.clone()
        })
}

pub(super) fn cmd_image_publish(cli: &Cli, args: &ImagePublishArgs) -> Result<()> {
    let _operation = lock_service_operation(&cli.state_dir, &args.service)?;
    let spec = AgentSpec::from_path(&args.spec)?;
    if spec.service.id != args.service {
        bail!(
            "spec service id '{}' does not match requested service '{}'",
            spec.service.id,
            args.service
        );
    }
    let region = args.region.as_deref().unwrap_or(&spec.deploy.region);
    let variant = args
        .variant
        .as_deref()
        .unwrap_or_else(|| spec.image_variant());
    let paths = context_paths(&cli.state_dir, &args.service);
    let mut state = read_service_state_file(&paths.service_state)?.with_context(|| {
        format!(
            "no local state for service '{}'; run build first",
            args.service
        )
    })?;
    let manifest = read_build_manifest(&paths.manifest).with_context(|| {
        format!(
            "service '{}' has no build manifest; run build first",
            args.service
        )
    })?;
    let build_variant = manifest.variant(variant, Some(&state.build.variant))?;
    let build_result =
        read_shelter_build_result(&build_variant.build_result, &build_variant.shelter_build_id)?;
    if !build_result.image_path.exists() {
        bail!(
            "local image for service '{}' variant '{}' was removed at '{}'; run build first",
            args.service,
            variant,
            build_result.image_path.display()
        );
    }
    let source_sha256 = sha256_file(&build_result.image_path)?;
    let source_size = build_result
        .image_path
        .metadata()
        .with_context(|| format!("failed to stat '{}'", build_result.image_path.display()))?
        .len();
    let key = publish_key(
        region,
        variant,
        &build_variant.shelter_build_id,
        &source_sha256,
    );
    let published = publish_variant_image(
        cli,
        &mut state,
        &key,
        &build_result.image_path,
        region,
        &args.service,
        variant,
        &build_variant.shelter_build_id,
        &source_sha256,
        source_size,
        !args.no_wait,
    )?;
    println!(
        "[ca] published state: key={key} status={} image_id={}",
        published.status,
        published.image_id.as_deref().unwrap_or("-")
    );
    Ok(())
}

pub(super) fn cmd_image_unpublish(cli: &Cli, args: &ImageUnpublishArgs) -> Result<()> {
    cmd_image_unpublish_with_ops(cli, &RealPublishCloudOps, args)
}

fn image_references(state_dir: &Path, image_id: &str) -> Result<Vec<String>> {
    let mut references = BTreeSet::new();
    for state in read_service_states(state_dir)? {
        if matches!(
            state.phase.as_str(),
            "active" | "activating" | "deployed" | "deploying" | "destroying"
        ) && state.deploy.published_image_id.as_deref() == Some(image_id)
        {
            references.insert(format!("service '{}' is {}", state.service_id, state.phase));
        }
        if state
            .build
            .published
            .values()
            .any(|entry| entry.exported && entry.image_id.as_deref() == Some(image_id))
        {
            references.insert(format!(
                "service '{}' exported a signed release",
                state.service_id
            ));
        }
    }
    // Imports can precede state creation (including deploy --render-only).
    // Retain these references until their consumer is explicitly removed.
    let services = absolute_path_for_state(state_dir).join("services");
    if services.try_exists()? {
        for directory in fs::read_dir(services)? {
            let manifest = directory?.path().join("manifest.json");
            if manifest.try_exists()? {
                let manifest = read_build_manifest(&manifest)?;
                if manifest
                    .image
                    .as_ref()
                    .is_some_and(|image| image.image_id == image_id)
                {
                    references.insert(format!(
                        "service '{}' imported this release",
                        manifest.service_id
                    ));
                }
            }
        }
    }
    Ok(references.into_iter().collect())
}

fn cmd_image_unpublish_with_ops(
    cli: &Cli,
    ops: &impl PublishCloudOps,
    args: &ImageUnpublishArgs,
) -> Result<()> {
    let _operation = lock_service_operation(&cli.state_dir, &args.service)?;
    let paths = context_paths(&cli.state_dir, &args.service);
    let mut state = read_service_state_file(&paths.service_state)?
        .with_context(|| format!("no local state for service '{}'", args.service))?;
    let keys = state
        .build
        .published
        .iter()
        .filter(|(_, entry)| {
            args.region
                .as_deref()
                .map(|region| entry.region == region)
                .unwrap_or(true)
                && args
                    .variant
                    .as_deref()
                    .map(|variant| entry.variant == variant)
                    .unwrap_or(true)
                && args
                    .image_id
                    .as_deref()
                    .map(|image_id| entry.image_id.as_deref() == Some(image_id))
                    .unwrap_or(true)
        })
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    if keys.is_empty() {
        println!(
            "[ca] no published images to unpublish for service '{}'",
            args.service
        );
        return Ok(());
    }
    let images: BTreeSet<_> = keys
        .iter()
        .filter_map(|key| {
            let entry = &state.build.published[key];
            entry
                .image_id
                .as_ref()
                .map(|image| (entry.region.clone(), image.clone()))
        })
        .collect();
    let _leases = images
        .iter()
        .map(|(region, image)| lock_image_operation(&cli.state_dir, region, image, false))
        .collect::<Result<Vec<_>>>()?;
    if !args.force {
        for (_, image) in &images {
            let references = image_references(&cli.state_dir, image)?;
            anyhow::ensure!(references.is_empty(),
                "published image {image} is retained: {}; retire its consumers before explicitly unpublishing with --force", references.join("; "));
        }
    }

    let mut removed = 0;
    let mut failures = 0;
    for key in keys {
        if let Err(error) = retire_publication(cli, ops, &mut state, &key, args.force) {
            failures += 1;
            eprintln!("[ca] warning: {error:#}");
        } else {
            removed += 1;
        }
    }
    println!(
        "[ca] unpublished {removed} image(s) for service '{}'",
        args.service
    );
    anyhow::ensure!(failures == 0, "image unpublish partially failed: {failures} publication(s) retained for retry; {removed} removed");
    Ok(())
}

/// Caller holds the service lock and the exclusive cloud-image lease.
fn retire_publication(
    cli: &Cli,
    ops: &impl PublishCloudOps,
    state: &mut LocalServiceState,
    key: &str,
    force: bool,
) -> Result<()> {
    let mut entry = state.build.published[key].clone();
    anyhow::ensure!(entry.owned || force, "image was adopted from another publication; explicit unpublish --force is required to assume deletion custody");
    entry.retiring = true;
    persist_published(cli, state, key, entry.clone())?;
    if let Some(image_id) = entry.image_id.clone() {
        capture_import_snapshots(cli, ops, &mut entry)?;
        persist_published(cli, state, key, entry.clone())?;
        let deletion = (|| {
            if ops
                .describe_image_status(cli, &entry.region, &image_id)?
                .is_some()
            {
                ops.delete_cloud_image(cli, &entry.region, &image_id, force)?;
            }
            Ok::<(), anyhow::Error>(())
        })();
        if let Err(error) = deletion {
            entry.error = Some(format!("cloud image retirement failed: {error:#}"));
            persist_published(cli, state, key, entry)?;
            return Err(error);
        }
        entry.image_id = None;
        entry.import_task_id = None;
        // Save cloud deletion before attempting OSS cleanup, including across process restarts.
        persist_published(cli, state, key, entry.clone())?;
    }
    for snapshot in entry.import_snapshot_ids.clone() {
        if let Err(error) = ops.delete_import_snapshot(cli, &entry.region, &snapshot) {
            entry.error = Some(format!("snapshot cleanup pending: {error:#}"));
            persist_published(cli, state, key, entry)?;
            return Err(error);
        }
        entry.import_snapshot_ids.retain(|id| id != &snapshot);
        persist_published(cli, state, key, entry.clone())?;
    }
    cleanup_upload(cli, ops, &mut entry);
    if !entry.oss_cleaned {
        persist_published(cli, state, key, entry)?;
        bail!("OSS cleanup pending; retry image unpublish");
    }
    remove_publication(cli, state, key)
}

fn published_entry_is_deployed(state: &LocalServiceState, entry: &PublishedImage) -> bool {
    matches!(
        state.phase.as_str(),
        "active" | "activating" | "deployed" | "deploying" | "destroying"
    ) && entry.image_id.is_some()
        && state.deploy.published_image_id.as_deref() == entry.image_id.as_deref()
}

fn should_prune_published_entry(
    state: &LocalServiceState,
    entry: &PublishedImage,
    all: bool,
) -> bool {
    (all || entry.retiring
        || entry.build_id != state.build.build_id
        || entry.status == STATUS_FAILED)
        && !published_entry_is_deployed(state, entry)
        && !entry.exported
        && entry.owned
}

pub(super) fn cmd_image_prune(cli: &Cli, args: &ImagePruneArgs) -> Result<()> {
    cmd_image_prune_with_ops(cli, &RealPublishCloudOps, args)
}

fn cmd_image_prune_with_ops(
    cli: &Cli,
    ops: &impl PublishCloudOps,
    args: &ImagePruneArgs,
) -> Result<()> {
    let mut total = 0usize;
    let mut failures = 0usize;
    for state in read_service_states(&cli.state_dir)? {
        let _operation = lock_service_operation(&cli.state_dir, &state.service_id)?;
        let Some(mut state) = read_service_state_file(
            &context_paths(&cli.state_dir, &state.service_id).service_state,
        )?
        else {
            continue;
        };
        let manifest_path = context_paths(&cli.state_dir, &state.service_id).manifest;
        let current_builds = if manifest_path.exists() {
            read_build_manifest(&manifest_path)?
                .variants
                .values()
                .map(|v| v.shelter_build_id.clone())
                .collect::<BTreeSet<_>>()
        } else {
            BTreeSet::new()
        };
        // Upload GC is independent of whether the cloud image is retained/exported.
        for key in state.build.published.keys().cloned().collect::<Vec<_>>() {
            let mut entry = state.build.published[&key].clone();
            if entry.status == STATUS_AVAILABLE && !entry.oss_cleaned && !entry.retiring {
                if args.dry_run {
                    println!("[ca] would clean OSS upload for {}", key);
                } else {
                    cleanup_upload(cli, ops, &mut entry);
                    if !entry.oss_cleaned {
                        failures += 1;
                    }
                    persist_published(cli, &mut state, &key, entry)?;
                }
            }
        }
        let keys = state
            .build
            .published
            .iter()
            .filter(|(_, entry)| {
                should_prune_published_entry(&state, entry, args.all)
                    && (args.all
                        || entry.retiring
                        || entry.status == STATUS_FAILED
                        || !current_builds.contains(&entry.build_id))
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            let entry = state.build.published[&key].clone();
            let _lease = entry
                .image_id
                .as_deref()
                .map(|image| lock_image_operation(&cli.state_dir, &entry.region, image, false))
                .transpose()?;
            if let Some(image) = &entry.image_id {
                let references = image_references(&cli.state_dir, image)?;
                if !references.is_empty() {
                    println!("[ca] retaining image {image}: {}", references.join("; "));
                    continue;
                }
            }
            if args.dry_run {
                println!(
                    "[ca] would prune service={} key={} image_id={}",
                    state.service_id,
                    key,
                    entry.image_id.as_deref().unwrap_or("-")
                );
            } else {
                if let Err(error) = retire_publication(cli, ops, &mut state, &key, false) {
                    failures += 1;
                    eprintln!("[ca] warning: {error:#}");
                    continue;
                }
            }
            total += 1;
        }
    }
    if args.dry_run {
        println!("[ca] would prune {total} published image(s)");
    } else {
        println!("[ca] pruned {total} published image(s)");
    }
    anyhow::ensure!(failures == 0, "image prune partially failed: {failures} publication(s) retained for retry; {total} removed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    // Run explicitly on a configured host. Both cloud operations are read-only.
    #[test]
    #[ignore = "requires a real host Aliyun profile, native tools and cloud access"]
    fn live_host_profile_authenticates_cloud_cli_and_oss() -> Result<()> {
        for name in [
            "ALICLOUD_ACCESS_KEY",
            "ALICLOUD_SECRET_KEY",
            "ALICLOUD_STS_TOKEN",
            "ALIBABA_CLOUD_ACCESS_KEY_ID",
            "ALIBABA_CLOUD_ACCESS_KEY_SECRET",
            "ALIBABA_CLOUD_SECURITY_TOKEN",
        ] {
            anyhow::ensure!(
                std::env::var_os(name).is_none(),
                "live profile test requires credential environment variables to be unset"
            );
        }
        let state = tempfile::tempdir()?;
        let mut cli = test_cli(state.path());
        cli.tools_dir = std::env::var_os("CA_TOOLS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/libexec/cagent/bin"));
        validate_cloud_credentials(&cli, "cn-beijing")?;
        let config = ossutil_config(&cli, "cn-beijing")?;
        assert_eq!(
            fs::metadata(&config.path)?.permissions().mode() & 0o777,
            0o600
        );
        let path = config.path.clone();
        drop(config);
        assert!(!path.exists());
        run_ossutil_capture(&cli, vec!["ls".into()], "cn-beijing")?;
        Ok(())
    }

    #[test]
    fn publish_key_includes_build_and_hash() {
        let key = publish_key(
            "cn-beijing",
            "release",
            "svc-release-20260602",
            "abcdef0123456789",
        );
        assert_eq!(
            key,
            "aliyun/cn-beijing/release/svc-release-20260602/abcdef012345"
        );
    }

    #[test]
    fn publication_namespace_survives_move_but_not_a_new_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("state");
        let moved = temp.path().join("moved");
        let first = publish_bucket_name("cn-beijing", &original).unwrap();
        assert_eq!(first, publish_bucket_name("cn-beijing", &original).unwrap());
        fs::rename(&original, &moved).unwrap();
        assert_eq!(first, publish_bucket_name("cn-beijing", &moved).unwrap());
        assert_ne!(first, publish_bucket_name("cn-beijing", &original).unwrap());
        assert!(first.starts_with("ca-images-cn-beijing-"));
        assert!(first.len() <= 63);
    }

    #[test]
    fn concurrent_publishers_share_one_namespace() {
        let temp = tempfile::tempdir().unwrap();
        let threads = (0..4)
            .map(|_| {
                let root = temp.path().to_path_buf();
                std::thread::spawn(move || publish_bucket_name("cn-beijing", &root).unwrap())
            })
            .collect::<Vec<_>>();
        let names = threads
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(names.len(), 1);
    }

    #[test]
    fn publish_image_name_filters_and_truncates() {
        let name = publish_image_name(
            "service with spaces!",
            "release",
            &"a".repeat(160),
            "0123456789abcdef",
        );
        assert!(name.len() <= 128);
        assert!(!name.contains(' '));
        assert!(!name.contains('!'));
        assert!(name.ends_with("-0123456789ab"));
    }

    #[test]
    fn import_image_args_pin_qcow2_format() {
        let args = import_image_args(ImportImageRequest {
            client_token: "test-stable-import-token",
            region: "cn-beijing",
            image_name: "ca-pub-openclaw",
            bucket: "ca-images-cn-beijing-test",
            object_key: "images/openclaw.qcow2",
            service_id: "openclaw",
            build_id: "openclaw-agent-release",
            variant: "release",
            source_sha256: "0123456789abcdef",
        });

        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--DiskDeviceMapping.1.Format" && pair[1] == "qcow2"));
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "--Features.NvmeSupport" && pair[1] == "supported"));
        assert!(args.iter().any(|arg| arg == "--force"));
    }

    #[test]
    fn prune_allows_failed_entry_without_image_id_for_active_service() {
        let mut state = test_state("openclaw");
        state.phase = "active".to_string();
        state.deploy.published_image_id = None;
        let mut entry = test_published_entry(&state.build.build_id, STATUS_FAILED);
        entry.image_id = None;

        assert!(should_prune_published_entry(&state, &entry, false));
    }

    #[test]
    fn prune_keeps_deployed_published_image() {
        let mut state = test_state("openclaw");
        state.phase = "active".to_string();
        state.deploy.published_image_id = Some("m-2ze123abc".to_string());
        let mut entry = test_published_entry("old-build", STATUS_FAILED);
        entry.image_id = Some("m-2ze123abc".to_string());

        for phase in ["active", "deployed", "deploying"] {
            state.phase = phase.to_string();
            assert!(!should_prune_published_entry(&state, &entry, true));
        }
    }

    #[test]
    fn cleanup_retains_independent_consumers_and_exported_releases() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let mut publisher = test_state("builder");
        let mut entry = test_published_entry("old-build", STATUS_AVAILABLE);
        entry.oss_cleaned = true;
        publisher
            .build
            .published
            .insert("old".into(), entry.clone());
        write_local_service_state(temp.path(), &publisher).unwrap();
        let ops = FakePublishCloudOps::default();
        let unpublish = ImageUnpublishArgs {
            service: "builder".into(),
            region: None,
            variant: None,
            image_id: None,
            force: false,
        };
        let prune = ImagePruneArgs {
            dry_run: false,
            all: true,
        };
        for phase in ["active", "deployed", "deploying"] {
            let mut consumer = test_state("consumer-a");
            consumer.phase = phase.into();
            consumer.deploy.published_image_id = entry.image_id.clone();
            write_local_service_state(temp.path(), &consumer).unwrap();
            assert!(cmd_image_unpublish_with_ops(&cli, &ops, &unpublish)
                .unwrap_err()
                .to_string()
                .contains("consumer-a"));
            cmd_image_prune_with_ops(&cli, &ops, &prune).unwrap();
            assert!(ops.calls().is_empty());
        }
        let consumer = test_state("consumer-a");
        write_local_service_state(temp.path(), &consumer).unwrap();
        let imported = BuildManifest {
            service_id: "consumer-a".into(),
            image: Some(images::ImageDeployment {
                image_id: entry.image_id.clone().unwrap(),
                source: confidential_agent_core::spec::PublishedImageSpec {
                    release: "/release.json".into(),
                    trusted_key: "/publisher.pem".into(),
                    manifest: Some(format!("sha256:{}", "a".repeat(64))),
                    legacy_account_id: None,
                    image_id: None,
                    skip_signature_verification: false,
                },
            }),
            ..BuildManifest::default()
        };
        let manifest_path = context_paths(temp.path(), "consumer-a").manifest;
        write_json_atomic(&manifest_path, &imported).unwrap();
        assert!(cmd_image_unpublish_with_ops(&cli, &ops, &unpublish)
            .unwrap_err()
            .to_string()
            .contains("imported"));
        cmd_image_prune_with_ops(&cli, &ops, &prune).unwrap();
        assert!(ops.calls().is_empty());
        fs::remove_file(manifest_path).unwrap();
        publisher.build.published.get_mut("old").unwrap().exported = true;
        write_local_service_state(temp.path(), &publisher).unwrap();
        cmd_image_prune_with_ops(&cli, &ops, &prune).unwrap();
        assert!(ops.calls().is_empty());
        assert!(cmd_image_unpublish_with_ops(&cli, &ops, &unpublish)
            .unwrap_err()
            .to_string()
            .contains("exported"));
        cmd_image_unpublish_with_ops(
            &cli,
            &ops,
            &ImageUnpublishArgs {
                force: true,
                ..unpublish
            },
        )
        .unwrap();
        assert_eq!(
            ops.calls(),
            vec!["describe:m-2ze123abc", "delete:m-2ze123abc"]
        );
    }

    #[test]
    fn unpublish_rejects_image_used_by_pending_deploy() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let mut state = test_state("openclaw");
        state.phase = "deploying".to_string();
        let entry = test_published_entry(&state.build.build_id, STATUS_AVAILABLE);
        state.deploy.published_image_id = entry.image_id.clone();
        state.build.published.insert("pending".into(), entry);
        write_local_service_state(temp.path(), &state).unwrap();
        let ops = FakePublishCloudOps::default();
        let result = cmd_image_unpublish_with_ops(
            &cli,
            &ops,
            &ImageUnpublishArgs {
                service: "openclaw".into(),
                region: None,
                variant: None,
                image_id: None,
                force: false,
            },
        );
        assert!(result.unwrap_err().to_string().contains("is deploying"));
        assert!(ops.calls().is_empty());
    }

    #[test]
    fn publish_fresh_no_wait_records_importing_state() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let image_path = temp.path().join("image.qcow2");
        fs::write(&image_path, "image").unwrap();
        let mut state = test_state("openclaw");
        let source_sha256 = sha256_file(&image_path).unwrap();
        let key = publish_key(
            "cn-beijing",
            "release",
            &state.build.build_id,
            &source_sha256,
        );
        let ops = FakePublishCloudOps::default();

        let published = publish_variant_image_with_ops(
            &cli,
            &ops,
            &mut state,
            &key,
            &image_path,
            "cn-beijing",
            "openclaw",
            "release",
            "openclaw-agent-release",
            &source_sha256,
            5,
            false,
        )
        .unwrap();

        assert_eq!(published.status, STATUS_IMPORTING);
        assert_eq!(published.image_id.as_deref(), Some("m-imported"));
        assert_eq!(
            ops.calls(),
            vec![
                "validate",
                "find-existing",
                "ensure-bucket",
                "upload",
                "import"
            ]
        );
        let written =
            read_service_state_file(&context_paths(temp.path(), "openclaw").service_state)
                .unwrap()
                .unwrap();
        let entry = written.build.published.get(&key).unwrap();
        assert_eq!(entry.status, STATUS_IMPORTING);
        assert_eq!(entry.import_task_id.as_deref(), Some("task-imported"));
        assert!(!entry.oss_cleaned);
    }

    #[test]
    fn publish_upload_failure_after_cleanup_preserves_object_custody() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let image_path = temp.path().join("image.qcow2");
        fs::write(&image_path, "image").unwrap();
        let mut state = test_state("openclaw");
        let source_sha256 = sha256_file(&image_path).unwrap();
        let key = publish_key(
            "cn-beijing",
            "release",
            &state.build.build_id,
            &source_sha256,
        );
        let mut prior = test_published_entry(&state.build.build_id, &source_sha256);
        prior.status = STATUS_FAILED.to_string();
        prior.image_id = None;
        prior.import_task_id = None;
        prior.oss_cleaned = true;
        prior.bucket = Some("legacy-publisher-bucket".into());
        prior.object_key = Some("legacy/image.qcow2".into());
        state.build.published.insert(key.clone(), prior);
        // A resumed publication must not allocate a new namespace or object path.
        fs::write(temp.path().join("publication-namespace.json"), "invalid").unwrap();
        let ops = FakePublishCloudOps {
            upload_error: Some("upload failed".to_string()),
            ..FakePublishCloudOps::default()
        };

        let err = publish_variant_image_with_ops(
            &cli,
            &ops,
            &mut state,
            &key,
            &image_path,
            "cn-beijing",
            "openclaw",
            "release",
            "openclaw-agent-release",
            &source_sha256,
            5,
            false,
        )
        .unwrap_err();

        assert!(err.to_string().contains("upload failed"));
        assert_eq!(
            *ops.upload_targets.borrow(),
            vec![(
                "legacy-publisher-bucket".into(),
                "legacy/image.qcow2".into()
            )]
        );
        let written =
            read_service_state_file(&context_paths(temp.path(), "openclaw").service_state)
                .unwrap()
                .unwrap();
        assert_eq!(written.build.published[&key].status, STATUS_UPLOADING);
        assert!(written.build.published[&key].image_id.is_none());
        assert!(!written.build.published[&key].oss_cleaned);
    }

    #[test]
    fn publish_import_failure_keeps_uploaded_state_for_retry() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let image_path = temp.path().join("image.qcow2");
        fs::write(&image_path, "image").unwrap();
        let mut state = test_state("openclaw");
        let source_sha256 = sha256_file(&image_path).unwrap();
        let key = publish_key(
            "cn-beijing",
            "release",
            &state.build.build_id,
            &source_sha256,
        );
        let ops = FakePublishCloudOps {
            import_error: Some("import failed".to_string()),
            ..FakePublishCloudOps::default()
        };

        let err = publish_variant_image_with_ops(
            &cli,
            &ops,
            &mut state,
            &key,
            &image_path,
            "cn-beijing",
            "openclaw",
            "release",
            "openclaw-agent-release",
            &source_sha256,
            5,
            false,
        )
        .unwrap_err();

        assert!(err.to_string().contains("import failed"));
        let written =
            read_service_state_file(&context_paths(temp.path(), "openclaw").service_state)
                .unwrap()
                .unwrap();
        assert_eq!(written.build.published[&key].status, STATUS_UPLOADED);
        assert!(written.build.published[&key].image_id.is_none());
        assert_eq!(
            ops.calls(),
            vec![
                "validate",
                "find-existing",
                "ensure-bucket",
                "upload",
                "import"
            ]
        );
    }

    #[test]
    fn publish_fresh_wait_marks_available_and_cleans_oss() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let image_path = temp.path().join("image.qcow2");
        fs::write(&image_path, "image").unwrap();
        let mut state = test_state("openclaw");
        let source_sha256 = sha256_file(&image_path).unwrap();
        let key = publish_key(
            "cn-beijing",
            "release",
            &state.build.build_id,
            &source_sha256,
        );
        let ops = FakePublishCloudOps {
            statuses: RefCell::new(VecDeque::from([Some("Available".to_string())])),
            ..FakePublishCloudOps::default()
        };

        let published = publish_variant_image_with_ops(
            &cli,
            &ops,
            &mut state,
            &key,
            &image_path,
            "cn-beijing",
            "openclaw",
            "release",
            "openclaw-agent-release",
            &source_sha256,
            5,
            true,
        )
        .unwrap();

        assert_eq!(published.status, STATUS_AVAILABLE);
        assert!(published.oss_cleaned);
        assert_eq!(
            ops.calls(),
            vec![
                "validate",
                "find-existing",
                "ensure-bucket",
                "upload",
                "import",
                "describe:m-imported",
                "cleanup-oss"
            ]
        );
    }

    #[test]
    fn publish_fresh_wait_create_failed_persists_failed_state() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let image_path = temp.path().join("image.qcow2");
        fs::write(&image_path, "image").unwrap();
        let mut state = test_state("openclaw");
        let source_sha256 = sha256_file(&image_path).unwrap();
        let key = publish_key(
            "cn-beijing",
            "release",
            &state.build.build_id,
            &source_sha256,
        );
        let ops = FakePublishCloudOps {
            statuses: RefCell::new(VecDeque::from([
                Some("CreateFailed".to_string()),
                Some("CreateFailed".to_string()),
            ])),
            ..FakePublishCloudOps::default()
        };

        let err = publish_variant_image_with_ops(
            &cli,
            &ops,
            &mut state,
            &key,
            &image_path,
            "cn-beijing",
            "openclaw",
            "release",
            "openclaw-agent-release",
            &source_sha256,
            5,
            true,
        )
        .unwrap_err();

        assert!(err.to_string().contains("ECS image import failed"));
        let written =
            read_service_state_file(&context_paths(temp.path(), "openclaw").service_state)
                .unwrap()
                .unwrap();
        let entry = written.build.published.get(&key).unwrap();
        assert_eq!(entry.status, STATUS_FAILED);
        assert_eq!(entry.image_id.as_deref(), Some("m-imported"));
        assert_eq!(
            ops.calls(),
            vec![
                "validate",
                "find-existing",
                "ensure-bucket",
                "upload",
                "import",
                "describe:m-imported",
                "describe:m-imported"
            ]
        );
    }

    #[test]
    fn wait_for_image_available_times_out_without_sleeping() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let ops = FakePublishCloudOps {
            statuses: RefCell::new(VecDeque::from([Some("Creating".to_string())])),
            timeout: Duration::ZERO,
            ..FakePublishCloudOps::default()
        };

        let err =
            wait_for_image_available_with_ops(&cli, &ops, "cn-beijing", "m-waiting").unwrap_err();

        assert!(err.to_string().contains("timed out after 0s"));
        assert_eq!(ops.calls(), vec!["describe:m-waiting"]);
    }

    #[test]
    fn import_wait_recovers_from_transient_reads_without_reimporting() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let ops = FakePublishCloudOps {
            describe_errors: RefCell::new(VecDeque::from([
                "context deadline exceeded".to_string(),
                "Client.Timeout exceeded while awaiting headers".to_string(),
            ])),
            ..FakePublishCloudOps::default()
        };
        wait_for_image_available_with_ops(&cli, &ops, "cn-beijing", "m-imported").unwrap();
        assert_eq!(ops.calls(), vec!["describe:m-imported"; 3]);
    }

    #[test]
    fn import_wait_bounds_retries_and_rejects_permanent_errors() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        for (error, attempts) in [("context deadline exceeded", 3), ("Forbidden.RAM", 1)] {
            let ops = FakePublishCloudOps {
                describe_errors: RefCell::new(VecDeque::from(vec![error.to_string(); 4])),
                ..FakePublishCloudOps::default()
            };
            let failure = wait_for_image_available_with_ops(&cli, &ops, "cn-beijing", "m-imported")
                .unwrap_err();
            assert!(failure.to_string().contains(error));
            assert_eq!(ops.calls(), vec!["describe:m-imported"; attempts]);
        }
    }

    #[test]
    fn publish_uploaded_retry_skips_upload_and_imports_again() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let image_path = temp.path().join("image.qcow2");
        fs::write(&image_path, "image").unwrap();
        let mut state = test_state("openclaw");
        let source_sha256 = sha256_file(&image_path).unwrap();
        let key = publish_key(
            "cn-beijing",
            "release",
            &state.build.build_id,
            &source_sha256,
        );
        let mut entry = test_published_entry(&state.build.build_id, &source_sha256);
        entry.status = STATUS_UPLOADED.to_string();
        entry.image_id = None;
        entry.import_task_id = None;
        entry.oss_cleaned = false;
        state.build.published.insert(key.clone(), entry);
        let ops = FakePublishCloudOps::default();

        let published = publish_variant_image_with_ops(
            &cli,
            &ops,
            &mut state,
            &key,
            &image_path,
            "cn-beijing",
            "openclaw",
            "release",
            "openclaw-agent-release",
            &source_sha256,
            5,
            false,
        )
        .unwrap();

        assert_eq!(published.status, STATUS_IMPORTING);
        assert_eq!(ops.calls(), vec!["validate", "find-existing", "import"]);
    }

    #[test]
    fn publish_importing_no_wait_does_not_start_duplicate_import() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let image_path = temp.path().join("image.qcow2");
        fs::write(&image_path, "image").unwrap();
        let mut state = test_state("openclaw");
        let source_sha256 = sha256_file(&image_path).unwrap();
        let key = publish_key(
            "cn-beijing",
            "release",
            &state.build.build_id,
            &source_sha256,
        );
        let mut entry = test_published_entry(&state.build.build_id, &source_sha256);
        entry.status = STATUS_IMPORTING.to_string();
        entry.image_id = Some("m-existing-import".to_string());
        entry.oss_cleaned = false;
        state.build.published.insert(key.clone(), entry);
        let ops = FakePublishCloudOps {
            statuses: RefCell::new(VecDeque::from([Some("Creating".to_string())])),
            ..FakePublishCloudOps::default()
        };

        let published = publish_variant_image_with_ops(
            &cli,
            &ops,
            &mut state,
            &key,
            &image_path,
            "cn-beijing",
            "openclaw",
            "release",
            "openclaw-agent-release",
            &source_sha256,
            5,
            false,
        )
        .unwrap();

        assert_eq!(published.status, STATUS_IMPORTING);
        assert_eq!(published.image_id.as_deref(), Some("m-existing-import"));
        assert_eq!(ops.calls(), vec!["validate", "describe:m-existing-import"]);
    }

    #[test]
    fn publish_importing_available_marks_available_and_cleans_oss() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let image_path = temp.path().join("image.qcow2");
        fs::write(&image_path, "image").unwrap();
        let mut state = test_state("openclaw");
        let source_sha256 = sha256_file(&image_path).unwrap();
        let key = publish_key(
            "cn-beijing",
            "release",
            &state.build.build_id,
            &source_sha256,
        );
        let mut entry = test_published_entry(&state.build.build_id, &source_sha256);
        entry.status = STATUS_IMPORTING.to_string();
        entry.image_id = Some("m-existing-import".to_string());
        entry.oss_cleaned = false;
        state.build.published.insert(key.clone(), entry);
        let ops = FakePublishCloudOps {
            statuses: RefCell::new(VecDeque::from([Some("Available".to_string())])),
            ..FakePublishCloudOps::default()
        };

        let published = publish_variant_image_with_ops(
            &cli,
            &ops,
            &mut state,
            &key,
            &image_path,
            "cn-beijing",
            "openclaw",
            "release",
            "openclaw-agent-release",
            &source_sha256,
            5,
            true,
        )
        .unwrap();

        assert_eq!(published.status, STATUS_AVAILABLE);
        assert!(published.oss_cleaned);
        assert_eq!(
            ops.calls(),
            vec!["validate", "describe:m-existing-import", "cleanup-oss"]
        );
    }

    #[test]
    fn publish_adopts_existing_available_cloud_image_without_uploading() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let image_path = temp.path().join("image.qcow2");
        fs::write(&image_path, "image").unwrap();
        let mut state = test_state("openclaw");
        let source_sha256 = sha256_file(&image_path).unwrap();
        let key = publish_key(
            "cn-beijing",
            "release",
            &state.build.build_id,
            &source_sha256,
        );
        let ops = FakePublishCloudOps {
            existing_image: RefCell::new(Some("m-adopted".to_string())),
            ..FakePublishCloudOps::default()
        };

        let published = publish_variant_image_with_ops(
            &cli,
            &ops,
            &mut state,
            &key,
            &image_path,
            "cn-beijing",
            "openclaw",
            "release",
            "openclaw-agent-release",
            &source_sha256,
            5,
            true,
        )
        .unwrap();

        assert_eq!(published.status, STATUS_AVAILABLE);
        assert_eq!(published.image_id.as_deref(), Some("m-adopted"));
        assert!(published.oss_cleaned);
        assert_eq!(ops.calls(), vec!["validate", "find-existing"]);
    }

    #[test]
    fn unpublish_cleanup_failure_keeps_failed_entry_without_deleted_image_id() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let mut state = test_state("openclaw");
        state.phase = "active".to_string();
        state.deploy.published_image_id = Some("m-2ze123abc".to_string());
        let key = "aliyun/cn-beijing/release/openclaw-agent-release/source".to_string();
        let mut entry = test_published_entry(&state.build.build_id, "source");
        entry.oss_cleaned = false;
        state.build.published.insert(key.clone(), entry);
        write_local_service_state(temp.path(), &state).unwrap();
        let ops = FakePublishCloudOps {
            cleanup_error: Some("oss cleanup failed".to_string()),
            ..FakePublishCloudOps::default()
        };

        let error = cmd_image_unpublish_with_ops(
            &cli,
            &ops,
            &ImageUnpublishArgs {
                service: "openclaw".to_string(),
                region: None,
                variant: None,
                image_id: None,
                force: true,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("partially failed"));

        let written =
            read_service_state_file(&context_paths(temp.path(), "openclaw").service_state)
                .unwrap()
                .unwrap();
        let remaining = written.build.published.get(&key).unwrap();
        assert!(remaining.retiring);
        assert_eq!(remaining.status, "source");
        assert!(remaining.image_id.is_none());
        assert!(remaining.import_task_id.is_none());
        assert_eq!(
            written.deploy.published_image_id.as_deref(),
            Some("m-2ze123abc")
        );
        assert_eq!(
            ops.calls(),
            vec!["describe:m-2ze123abc", "delete:m-2ze123abc", "cleanup-oss"]
        );
    }

    #[test]
    fn unpublish_delete_failure_keeps_failed_entry_with_image_id() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let mut state = test_state("openclaw");
        let key = "aliyun/cn-beijing/release/openclaw-agent-release/source".to_string();
        let mut entry = test_published_entry(&state.build.build_id, "source");
        entry.oss_cleaned = false;
        state.build.published.insert(key.clone(), entry);
        write_local_service_state(temp.path(), &state).unwrap();
        let ops = FakePublishCloudOps {
            delete_error: Some("delete failed".to_string()),
            ..FakePublishCloudOps::default()
        };

        let error = cmd_image_unpublish_with_ops(
            &cli,
            &ops,
            &ImageUnpublishArgs {
                service: "openclaw".to_string(),
                region: None,
                variant: None,
                image_id: None,
                force: true,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("partially failed"));

        let written =
            read_service_state_file(&context_paths(temp.path(), "openclaw").service_state)
                .unwrap()
                .unwrap();
        let remaining = written.build.published.get(&key).unwrap();
        assert!(remaining.retiring);
        assert_eq!(remaining.status, "source");
        assert_eq!(remaining.image_id.as_deref(), Some("m-2ze123abc"));
        assert!(!remaining.oss_cleaned);
        assert_eq!(
            ops.calls(),
            vec!["describe:m-2ze123abc", "delete:m-2ze123abc"]
        );
    }

    #[test]
    fn prune_dry_run_does_not_call_cloud_or_mutate_state() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let mut state = test_state("openclaw");
        let key = "aliyun/cn-beijing/release/old/source".to_string();
        state
            .build
            .published
            .insert(key.clone(), test_published_entry("old-build", "source"));
        write_local_service_state(temp.path(), &state).unwrap();
        let ops = FakePublishCloudOps::default();

        cmd_image_prune_with_ops(
            &cli,
            &ops,
            &ImagePruneArgs {
                dry_run: true,
                all: true,
            },
        )
        .unwrap();

        assert!(ops.calls().is_empty());
        let written =
            read_service_state_file(&context_paths(temp.path(), "openclaw").service_state)
                .unwrap()
                .unwrap();
        assert!(written.build.published.contains_key(&key));
    }

    #[test]
    fn prune_cleanup_failure_keeps_failed_entry_without_deleted_image_id() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let mut state = test_state("openclaw");
        let key = "aliyun/cn-beijing/release/old/source".to_string();
        let mut entry = test_published_entry("old-build", "source");
        entry.oss_cleaned = false;
        state.build.published.insert(key.clone(), entry);
        write_local_service_state(temp.path(), &state).unwrap();
        let ops = FakePublishCloudOps {
            cleanup_error: Some("oss cleanup failed".to_string()),
            ..FakePublishCloudOps::default()
        };

        let error = cmd_image_prune_with_ops(
            &cli,
            &ops,
            &ImagePruneArgs {
                dry_run: false,
                all: false,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("partially failed"));

        let written =
            read_service_state_file(&context_paths(temp.path(), "openclaw").service_state)
                .unwrap()
                .unwrap();
        let remaining = written.build.published.get(&key).unwrap();
        assert!(remaining.retiring);
        assert_eq!(remaining.status, "source");
        assert!(remaining.image_id.is_none());
        assert!(remaining.import_task_id.is_none());
        assert_eq!(
            ops.calls(),
            vec!["describe:m-2ze123abc", "delete:m-2ze123abc", "cleanup-oss"]
        );
    }

    #[test]
    fn prune_clears_stale_deploy_published_image_id_after_delete() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let mut state = test_state("openclaw");
        state.phase = "built".to_string();
        state.deploy.published_image_id = Some("m-2ze123abc".to_string());
        let key = "aliyun/cn-beijing/release/old/source".to_string();
        let mut entry = test_published_entry("old-build", "source");
        entry.oss_cleaned = true;
        state.build.published.insert(key.clone(), entry);
        write_local_service_state(temp.path(), &state).unwrap();
        let ops = FakePublishCloudOps::default();

        cmd_image_prune_with_ops(
            &cli,
            &ops,
            &ImagePruneArgs {
                dry_run: false,
                all: false,
            },
        )
        .unwrap();

        let written =
            read_service_state_file(&context_paths(temp.path(), "openclaw").service_state)
                .unwrap()
                .unwrap();
        assert!(!written.build.published.contains_key(&key));
        assert_eq!(written.deploy.published_image_id, None);
        assert_eq!(
            ops.calls(),
            vec!["describe:m-2ze123abc", "delete:m-2ze123abc"]
        );
    }

    fn test_state(service_id: &str) -> LocalServiceState {
        LocalServiceState {
            schema: LOCAL_SERVICE_STATE_SCHEMA_VERSION.to_string(),
            service_id: service_id.to_string(),
            generation: 1,
            phase: "built".to_string(),
            attestation_mode: "challenge".to_string(),
            spec: LocalSpecState {
                path: PathBuf::from("/spec.yaml"),
                sha256: "spec".to_string(),
            },
            build: LocalBuildState {
                build_id: format!("{service_id}-agent-release"),
                image_name: format!("{service_id}-agent"),
                variant: "release".to_string(),
                image_path: PathBuf::from("/state/image.qcow2"),
                images_dir: PathBuf::from("/state/images"),
                cache_dir: PathBuf::from("/state/cache"),
                debug_ssh: None,
                sample_rv: None,
                rekor_meta: None,
                remote: false,
                published: BTreeMap::new(),
            },
            deploy: LocalDeployState {
                provider: PROVIDER_ALIYUN.to_string(),
                run_id: "run".to_string(),
                resource_name: "resource".to_string(),
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
                instance_public_ips: Vec::new(),
                slb_vip: None,
            },
            service: LocalServiceNetwork {
                ports: vec![18789],
                connect: vec![(confidential_agent_core::spec::ConnectPort::Plain(18789))],
                mcp_ports: Vec::new(),
            },
            gateway_identity: Some(LocalGatewayIdentity {
                public_key: "pub".to_string(),
                private_key_path: PathBuf::from(format!(
                    "/state/services/{service_id}/secrets/gateway_identity.seed"
                )),
            }),
            resources: BTreeMap::new(),
            mesh_generation: 0,
            reference_values: "sample".to_string(),
        }
    }

    fn test_published_entry(build_id: &str, status: &str) -> PublishedImage {
        PublishedImage {
            provider: PROVIDER_ALIYUN.to_string(),
            region: "cn-beijing".to_string(),
            variant: "release".to_string(),
            build_id: build_id.to_string(),
            source_sha256: "0123456789abcdef".to_string(),
            source_size: 1024,
            status: status.to_string(),
            image_name: "ca-pub-openclaw".to_string(),
            image_id: Some("m-2ze123abc".to_string()),
            import_task_id: None,
            bucket: Some("ca-images-cn-beijing-test".to_string()),
            object_key: Some("images/openclaw.qcow2".to_string()),
            created_at: "2026-06-02T00:00:00Z".to_string(),
            updated_at: "2026-06-02T00:00:00Z".to_string(),
            oss_cleaned: false,
            exported: false,
            owned: true,
            import_token: new_import_token(),
            retiring: false,
            import_snapshot_ids: Vec::new(),
            error: None,
        }
    }

    fn test_cli(state_dir: &Path) -> Cli {
        Cli {
            command: Commands::Version,
            shelter_bin: PathBuf::from("shelter"),
            state_dir: state_dir.to_path_buf(),
            tools_dir: PathBuf::from("/usr/libexec/cagent/bin"),
        }
    }

    struct FakePublishCloudOps {
        calls: RefCell<Vec<String>>,
        upload_targets: RefCell<Vec<(String, String)>>,
        existing_image: RefCell<Option<String>>,
        statuses: RefCell<VecDeque<Option<String>>>,
        describe_errors: RefCell<VecDeque<String>>,
        timeout: Duration,
        poll_interval: Duration,
        upload_error: Option<String>,
        import_error: Option<String>,
        import_tokens: RefCell<Vec<String>>,
        cleanup_error: Option<String>,
        delete_error: Option<String>,
        snapshot_ids: Vec<String>,
        snapshot_delete_error: Option<String>,
        delete_force: RefCell<Vec<bool>>,
    }

    impl Default for FakePublishCloudOps {
        fn default() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                upload_targets: RefCell::new(Vec::new()),
                existing_image: RefCell::new(None),
                statuses: RefCell::new(VecDeque::new()),
                describe_errors: RefCell::new(VecDeque::new()),
                timeout: Duration::from_secs(60),
                poll_interval: Duration::ZERO,
                upload_error: None,
                import_error: None,
                import_tokens: RefCell::new(Vec::new()),
                cleanup_error: None,
                delete_error: None,
                snapshot_ids: Vec::new(),
                snapshot_delete_error: None,
                delete_force: RefCell::new(Vec::new()),
            }
        }
    }

    impl FakePublishCloudOps {
        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn record(&self, call: impl Into<String>) {
            self.calls.borrow_mut().push(call.into());
        }
    }

    impl PublishCloudOps for FakePublishCloudOps {
        fn image_import_timeout(&self) -> Duration {
            self.timeout
        }

        fn image_import_poll_interval(&self) -> Duration {
            self.poll_interval
        }

        fn validate_credentials(&self, _cli: &Cli, _region: &str) -> Result<()> {
            self.record("validate");
            Ok(())
        }

        fn find_existing_published_image(
            &self,
            _cli: &Cli,
            _region: &str,
            _service_id: &str,
            _variant: &str,
            _build_id: &str,
            _source_sha256: &str,
        ) -> Result<Option<String>> {
            self.record("find-existing");
            Ok(self.existing_image.borrow().clone())
        }

        fn describe_image_status(
            &self,
            _cli: &Cli,
            _region: &str,
            image_id: &str,
        ) -> Result<Option<String>> {
            self.record(format!("describe:{image_id}"));
            if let Some(error) = self.describe_errors.borrow_mut().pop_front() {
                bail!(error);
            }
            Ok(self
                .statuses
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Some("Available".to_string())))
        }

        fn ensure_publish_bucket(&self, _cli: &Cli, _bucket: &str, _region: &str) -> Result<()> {
            self.record("ensure-bucket");
            Ok(())
        }

        fn upload_image(
            &self,
            _cli: &Cli,
            _image_path: &Path,
            _bucket: &str,
            _object_key: &str,
            _region: &str,
        ) -> Result<()> {
            self.record("upload");
            self.upload_targets
                .borrow_mut()
                .push((_bucket.into(), _object_key.into()));
            if let Some(err) = &self.upload_error {
                bail!("{err}");
            }
            Ok(())
        }

        fn import_image(
            &self,
            _cli: &Cli,
            _request: ImportImageRequest<'_>,
        ) -> Result<ImportImageResponse> {
            self.record("import");
            self.import_tokens
                .borrow_mut()
                .push(_request.client_token.to_string());
            if let Some(err) = &self.import_error {
                bail!("{err}");
            }
            Ok(ImportImageResponse {
                image_id: "m-imported".to_string(),
                task_id: Some("task-imported".to_string()),
            })
        }

        fn cleanup_oss_object(
            &self,
            _cli: &Cli,
            _bucket: &str,
            _object_key: &str,
            _region: &str,
        ) -> Result<()> {
            self.record("cleanup-oss");
            if let Some(err) = &self.cleanup_error {
                bail!("{err}");
            }
            Ok(())
        }

        fn image_snapshot_ids(
            &self,
            _cli: &Cli,
            _region: &str,
            _image_id: &str,
        ) -> Result<Vec<String>> {
            Ok(self.snapshot_ids.clone())
        }
        fn delete_import_snapshot(&self, _cli: &Cli, _region: &str, snapshot: &str) -> Result<()> {
            self.record(format!("delete-snapshot:{snapshot}"));
            if let Some(error) = &self.snapshot_delete_error {
                bail!("{error}");
            }
            Ok(())
        }
        fn delete_cloud_image(
            &self,
            _cli: &Cli,
            _region: &str,
            image_id: &str,
            force: bool,
        ) -> Result<()> {
            self.delete_force.borrow_mut().push(force);
            self.record(format!("delete:{image_id}"));
            if let Some(err) = &self.delete_error {
                bail!("{err}");
            }
            Ok(())
        }
    }
    // Lifecycle regression tests.
    #[test]
    fn lifecycle_publish_does_not_retry_failed_retirement() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let image = temp.path().join("image.qcow2");
        fs::write(&image, "image").unwrap();
        let sha = sha256_file(&image).unwrap();
        let mut state = test_state("builder");
        let build = state.build.build_id.clone();
        let key = publish_key("cn-beijing", "release", &build, &sha);
        let mut entry = test_published_entry(&build, STATUS_AVAILABLE);
        entry.exported = true;
        state.build.published.insert(key.clone(), entry);
        write_local_service_state(temp.path(), &state).unwrap();
        let fail_delete = FakePublishCloudOps {
            delete_error: Some("transient delete error".into()),
            ..Default::default()
        };
        cmd_image_unpublish_with_ops(
            &cli,
            &fail_delete,
            &ImageUnpublishArgs {
                service: "builder".into(),
                region: None,
                variant: None,
                image_id: None,
                force: true,
            },
        )
        .unwrap_err();
        state = read_service_state_file(&context_paths(temp.path(), "builder").service_state)
            .unwrap()
            .unwrap();
        assert!(state.build.published[&key].retiring);
        let mut consumer = test_state("consumer");
        consumer.phase = "active".into();
        consumer.deploy.published_image_id = Some("m-2ze123abc".into());
        write_local_service_state(temp.path(), &consumer).unwrap();
        assert!(!image_references(temp.path(), "m-2ze123abc")
            .unwrap()
            .is_empty());
        let retry = FakePublishCloudOps::default();
        publish_variant_image_with_ops(
            &cli,
            &retry,
            &mut state,
            &key,
            &image,
            "cn-beijing",
            "builder",
            "release",
            &build,
            &sha,
            5,
            false,
        )
        .unwrap_err();
        assert!(retry.calls().is_empty());
        assert_eq!(
            state.build.published[&key].image_id.as_deref(),
            Some("m-2ze123abc")
        );
    }

    #[test]
    fn lifecycle_available_publish_retries_failed_oss_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let image = temp.path().join("image.qcow2");
        fs::write(&image, "image").unwrap();
        let sha = sha256_file(&image).unwrap();
        let mut state = test_state("builder");
        let build = state.build.build_id.clone();
        let key = publish_key("cn-beijing", "release", &build, &sha);
        let first = FakePublishCloudOps {
            cleanup_error: Some("temporary OSS timeout".into()),
            ..Default::default()
        };
        publish_variant_image_with_ops(
            &cli,
            &first,
            &mut state,
            &key,
            &image,
            "cn-beijing",
            "builder",
            "release",
            &build,
            &sha,
            5,
            true,
        )
        .unwrap();
        assert_eq!(state.build.published[&key].status, STATUS_AVAILABLE);
        assert!(!state.build.published[&key].oss_cleaned);
        let retry = FakePublishCloudOps::default();
        publish_variant_image_with_ops(
            &cli,
            &retry,
            &mut state,
            &key,
            &image,
            "cn-beijing",
            "builder",
            "release",
            &build,
            &sha,
            5,
            true,
        )
        .unwrap();
        assert!(retry.calls().contains(&"cleanup-oss".into()));
        assert!(state.build.published[&key].oss_cleaned);
        assert!(!should_prune_published_entry(
            &state,
            &state.build.published[&key],
            false
        ));
    }

    #[test]
    fn lifecycle_adopted_image_does_not_gain_deletion_custody() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let image = temp.path().join("image.qcow2");
        fs::write(&image, "image").unwrap();
        let sha = sha256_file(&image).unwrap();
        let mut state = test_state("builder");
        let build = state.build.build_id.clone();
        let key = publish_key("cn-beijing", "release", &build, &sha);
        let adopt = FakePublishCloudOps {
            existing_image: RefCell::new(Some("m-shared-from-workspace-A".into())),
            ..Default::default()
        };
        publish_variant_image_with_ops(
            &cli,
            &adopt,
            &mut state,
            &key,
            &image,
            "cn-beijing",
            "builder",
            "release",
            &build,
            &sha,
            5,
            true,
        )
        .unwrap();
        assert!(!state.build.published[&key].owned);
        let cleanup = FakePublishCloudOps::default();
        cmd_image_prune_with_ops(
            &cli,
            &cleanup,
            &ImagePruneArgs {
                dry_run: false,
                all: true,
            },
        )
        .unwrap();
        assert!(cleanup.calls().is_empty());
    }

    #[test]
    fn lifecycle_prune_keeps_all_current_variants() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let mut state = test_state("builder");
        state.build.build_id = "build-release-20261020".into();
        let mut release = test_published_entry(&state.build.build_id, STATUS_AVAILABLE);
        release.image_id = Some("m-current-release".into());
        release.oss_cleaned = true;
        let mut debug = test_published_entry("build-debug-20261020", STATUS_AVAILABLE);
        debug.image_id = Some("m-current-debug".into());
        debug.variant = "debug".into();
        debug.oss_cleaned = true;
        state.build.published.insert("release".into(), release);
        state.build.published.insert("debug".into(), debug);
        write_local_service_state(temp.path(), &state).unwrap();
        let mut manifest = BuildManifest::default();
        manifest.service_id = "builder".into();
        for (variant, id) in [
            ("release", "build-release-20261020"),
            ("debug", "build-debug-20261020"),
        ] {
            manifest.variants.insert(
                variant.into(),
                BuildManifestVariant {
                    preparation_dir: None,
                    shelter_build_id: id.into(),
                    build_result: "/unused/result.json".into(),
                    extra_files: vec![],
                    debug_ssh: None,
                },
            );
        }
        write_json_atomic(&context_paths(temp.path(), "builder").manifest, &manifest).unwrap();
        let ops = FakePublishCloudOps::default();
        cmd_image_prune_with_ops(
            &cli,
            &ops,
            &ImagePruneArgs {
                dry_run: false,
                all: false,
            },
        )
        .unwrap();
        assert!(ops.calls().is_empty());
    }

    #[test]
    fn lifecycle_publish_preserves_concurrent_mesh_generation() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let mut state = test_state("builder");
        state.phase = "active".into();
        state.mesh_generation = 1;
        write_local_service_state(temp.path(), &state).unwrap();
        let _operation = lock_service_operation(temp.path(), "builder").unwrap();
        // Another service's activation owns the state-dir lock and updates all active peers.
        with_state_dir_lock(temp.path(), || {
            update_mesh_generation(temp.path(), &["builder".into()], 2)
        })
        .unwrap();
        assert_eq!(
            read_service_state_file(&context_paths(temp.path(), "builder").service_state)
                .unwrap()
                .unwrap()
                .mesh_generation,
            2
        );
        let entry = test_published_entry(&state.build.build_id, STATUS_AVAILABLE);
        persist_published(&cli, &mut state, "pub", entry).unwrap();
        assert_eq!(
            read_service_state_file(&context_paths(temp.path(), "builder").service_state)
                .unwrap()
                .unwrap()
                .mesh_generation,
            2
        );
    }

    #[test]
    fn ambiguous_import_response_reuses_saved_token_without_reupload() {
        let root = tempfile::tempdir().unwrap();
        let cli = test_cli(root.path());
        let image = root.path().join("image.qcow2");
        fs::write(&image, "image").unwrap();
        let mut state = test_state("builder");
        let build = state.build.build_id.clone();
        let sha = sha256_file(&image).unwrap();
        let key = publish_key("cn-beijing", "release", &build, &sha);
        let lost_response = FakePublishCloudOps {
            import_error: Some("response lost after cloud acceptance".into()),
            ..Default::default()
        };
        publish_variant_image_with_ops(
            &cli,
            &lost_response,
            &mut state,
            &key,
            &image,
            "cn-beijing",
            "builder",
            "release",
            &build,
            &sha,
            5,
            false,
        )
        .unwrap_err();
        let saved = read_service_state_file(&context_paths(root.path(), "builder").service_state)
            .unwrap()
            .unwrap();
        assert_eq!(saved.build.published[&key].status, STATUS_UPLOADED);
        let token = saved.build.published[&key].import_token.clone();
        assert!(!token.is_empty());
        let retry = FakePublishCloudOps::default();
        let mut state = saved;
        publish_variant_image_with_ops(
            &cli,
            &retry,
            &mut state,
            &key,
            &image,
            "cn-beijing",
            "builder",
            "release",
            &build,
            &sha,
            5,
            false,
        )
        .unwrap();
        assert_eq!(*lost_response.import_tokens.borrow(), vec![token.clone()]);
        assert_eq!(*retry.import_tokens.borrow(), vec![token]);
        assert!(!retry.calls().contains(&"upload".into()));
    }

    #[test]
    fn prune_cleans_upload_for_exported_current_image_without_retiring_it() {
        let root = tempfile::tempdir().unwrap();
        let cli = test_cli(root.path());
        let mut state = test_state("builder");
        let mut entry = test_published_entry(&state.build.build_id, STATUS_AVAILABLE);
        entry.exported = true;
        entry.oss_cleaned = false;
        state.build.published.insert("pub".into(), entry);
        write_local_service_state(root.path(), &state).unwrap();
        let ops = FakePublishCloudOps::default();
        cmd_image_prune_with_ops(
            &cli,
            &ops,
            &ImagePruneArgs {
                dry_run: false,
                all: true,
            },
        )
        .unwrap();
        assert_eq!(ops.calls(), vec!["cleanup-oss"]);
        let state = read_service_state_file(&context_paths(root.path(), "builder").service_state)
            .unwrap()
            .unwrap();
        assert!(state.build.published["pub"].oss_cleaned);
        assert!(state.build.published["pub"].image_id.is_some());
    }

    #[test]
    fn retirement_reconciles_image_already_absent_after_interruption() {
        let root = tempfile::tempdir().unwrap();
        let cli = test_cli(root.path());
        let mut state = test_state("builder");
        let mut entry = test_published_entry(&state.build.build_id, STATUS_AVAILABLE);
        entry.retiring = true;
        entry.oss_cleaned = false;
        state.build.published.insert("pub".into(), entry);
        write_local_service_state(root.path(), &state).unwrap();
        let ops = FakePublishCloudOps {
            statuses: RefCell::new(VecDeque::from([None])),
            ..Default::default()
        };
        cmd_image_unpublish_with_ops(
            &cli,
            &ops,
            &ImageUnpublishArgs {
                service: "builder".into(),
                region: None,
                variant: None,
                image_id: None,
                force: false,
            },
        )
        .unwrap();
        assert_eq!(ops.calls(), vec!["describe:m-2ze123abc", "cleanup-oss"]);
        assert!(
            read_service_state_file(&context_paths(root.path(), "builder").service_state)
                .unwrap()
                .unwrap()
                .build
                .published
                .is_empty()
        );
    }
    #[test]
    fn retirement_persists_snapshots_and_retries_without_deleting_image_twice() {
        let root = tempfile::tempdir().unwrap();
        let cli = test_cli(root.path());
        let mut state = test_state("builder");
        let mut entry = test_published_entry(&state.build.build_id, STATUS_AVAILABLE);
        entry.oss_cleaned = true;
        state.build.published.insert("pub".into(), entry);
        write_local_service_state(root.path(), &state).unwrap();
        let first = FakePublishCloudOps {
            snapshot_ids: vec!["s-import".into()],
            snapshot_delete_error: Some("snapshot temporarily busy".into()),
            ..Default::default()
        };
        let args = ImageUnpublishArgs {
            service: "builder".into(),
            region: None,
            variant: None,
            image_id: None,
            force: false,
        };
        cmd_image_unpublish_with_ops(&cli, &first, &args).unwrap_err();
        assert_eq!(*first.delete_force.borrow(), vec![false]);
        let saved = read_service_state_file(&context_paths(root.path(), "builder").service_state)
            .unwrap()
            .unwrap();
        assert!(saved.build.published["pub"].image_id.is_none());
        assert_eq!(
            saved.build.published["pub"].import_snapshot_ids,
            vec!["s-import"]
        );
        let retry = FakePublishCloudOps::default();
        cmd_image_unpublish_with_ops(&cli, &retry, &args).unwrap();
        assert_eq!(retry.calls(), vec!["delete-snapshot:s-import"]);
        assert!(
            read_service_state_file(&context_paths(root.path(), "builder").service_state)
                .unwrap()
                .unwrap()
                .build
                .published
                .is_empty()
        );
    }
}
