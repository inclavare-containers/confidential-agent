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

fn publish_bucket_name(region: &str, state_dir: &Path) -> String {
    let abs = absolute_path_for_state(state_dir);
    let mut hasher = Sha256::new();
    hasher.update(abs.to_string_lossy().as_bytes());
    let hash = hex_encode(&hasher.finalize());
    format!("ca-images-{region}-{}", &hash[..10])
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

fn cloud_credential_envs(region: &str) -> Vec<(String, String)> {
    let mut envs = Vec::new();
    let access_key = std::env::var("ALICLOUD_ACCESS_KEY")
        .or_else(|_| std::env::var("ALIBABA_CLOUD_ACCESS_KEY_ID"));
    let secret_key = std::env::var("ALICLOUD_SECRET_KEY")
        .or_else(|_| std::env::var("ALIBABA_CLOUD_ACCESS_KEY_SECRET"));
    let sts_token = std::env::var("ALICLOUD_STS_TOKEN")
        .or_else(|_| std::env::var("ALIBABA_CLOUD_SECURITY_TOKEN"));

    if let Ok(value) = access_key {
        envs.push(("ALICLOUD_ACCESS_KEY".to_string(), value.clone()));
        envs.push(("ALIBABA_CLOUD_ACCESS_KEY_ID".to_string(), value));
    }
    if let Ok(value) = secret_key {
        envs.push(("ALICLOUD_SECRET_KEY".to_string(), value.clone()));
        envs.push(("ALIBABA_CLOUD_ACCESS_KEY_SECRET".to_string(), value));
    }
    if let Ok(value) = sts_token {
        envs.push(("ALICLOUD_STS_TOKEN".to_string(), value.clone()));
        envs.push(("ALIBABA_CLOUD_SECURITY_TOKEN".to_string(), value));
    }
    envs.push(("ALIBABA_CLOUD_REGION".to_string(), region.to_string()));
    envs.extend(inherited_proxy_envs(None));
    envs
}

fn required_cloud_credential(name_a: &str, name_b: &str) -> Result<String> {
    std::env::var(name_a)
        .or_else(|_| std::env::var(name_b))
        .with_context(|| format!("{name_a} or {name_b} is required"))
}

struct TempOssutilConfig {
    path: PathBuf,
}

impl Drop for TempOssutilConfig {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn ossutil_config(cli: &Cli, region: &str) -> Result<TempOssutilConfig> {
    let access_key =
        required_cloud_credential("ALICLOUD_ACCESS_KEY", "ALIBABA_CLOUD_ACCESS_KEY_ID")?;
    let secret_key =
        required_cloud_credential("ALICLOUD_SECRET_KEY", "ALIBABA_CLOUD_ACCESS_KEY_SECRET")?;
    let sts_token = std::env::var("ALICLOUD_STS_TOKEN")
        .or_else(|_| std::env::var("ALIBABA_CLOUD_SECURITY_TOKEN"))
        .ok();

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
    let endpoint = format!("https://oss-{region}.aliyuncs.com");

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
    Ok(TempOssutilConfig { path })
}

fn current_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

fn run_tools_container_capture(cli: &Cli, spec: ToolContainerSpec) -> Result<String> {
    ensure_docker_available()?;
    let envs = spec.envs.clone();
    let args = tools_container_args(&cli.tools_image, spec);
    let mut command = Command::new("docker");
    command.args(args);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command
        .stdin(Stdio::null())
        .output()
        .context("failed to execute 'docker'")?;
    if !output.status.success() {
        bail!(
            "tools container exited with status {}; stderr: {}; stdout: {}",
            output.status,
            summarize_command_bytes(&output.stderr),
            summarize_command_bytes(&output.stdout)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_aliyun_cli(cli: &Cli, args: Vec<String>, region: &str) -> Result<String> {
    let workdir = std::env::current_dir().context("failed to resolve current working directory")?;
    let state_dir = absolute_path_for_state(&cli.state_dir);
    fs::create_dir_all(&state_dir)
        .with_context(|| format!("failed to create '{}'", state_dir.display()))?;
    let mut tool_args = vec![OsString::from("--region"), OsString::from(region)];
    tool_args.extend(args.into_iter().map(OsString::from));
    run_tools_container_capture(
        cli,
        ToolContainerSpec {
            tool: "aliyun",
            tool_args,
            mounts: vec![workdir.clone(), state_dir],
            envs: cloud_credential_envs(region),
            workdir: Some(workdir),
            container_name: None,
        },
    )
}

fn run_ossutil(
    cli: &Cli,
    args: Vec<String>,
    file_mounts: Vec<PathBuf>,
    region: &str,
) -> Result<()> {
    let config = ossutil_config(cli, region)?;
    let mut tool_args = vec![
        OsString::from("-c"),
        config.path.as_os_str().to_os_string(),
        OsString::from("-e"),
        OsString::from(format!("https://oss-{region}.aliyuncs.com")),
    ];
    tool_args.extend(args.into_iter().map(OsString::from));
    let mut mounts = file_mounts;
    mounts.push(config.path.clone());
    run_containerized_host_tool(
        cli,
        "ossutil64",
        tool_args,
        mounts,
        inherited_proxy_envs(None),
        true,
    )
}

fn run_ossutil_capture(
    cli: &Cli,
    args: Vec<String>,
    file_mounts: Vec<PathBuf>,
    region: &str,
) -> Result<String> {
    let config = ossutil_config(cli, region)?;
    let mut tool_args = vec![
        OsString::from("-c"),
        config.path.as_os_str().to_os_string(),
        OsString::from("-e"),
        OsString::from(format!("https://oss-{region}.aliyuncs.com")),
    ];
    tool_args.extend(args.into_iter().map(OsString::from));

    let workdir = std::env::current_dir().context("failed to resolve current working directory")?;
    let state_dir = absolute_path_for_state(&cli.state_dir);
    fs::create_dir_all(&state_dir)
        .with_context(|| format!("failed to create '{}'", state_dir.display()))?;
    let mut mounts = vec![workdir.clone(), state_dir, config.path.clone()];
    for path in file_mounts {
        mounts.extend(mounts_for_file(&path, &workdir));
    }

    run_tools_container_capture(
        cli,
        ToolContainerSpec {
            tool: "ossutil64",
            tool_args,
            mounts,
            envs: inherited_proxy_envs(None),
            workdir: Some(workdir),
            container_name: None,
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
        Vec::new(),
        region,
    );
    if check.is_ok() {
        return Ok(());
    }
    println!("[ca] creating OSS bucket {bucket}...");
    run_ossutil(
        cli,
        vec!["mb".to_string(), format!("oss://{bucket}")],
        Vec::new(),
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
        Vec::new(),
        region,
    )
}

fn delete_cloud_image(cli: &Cli, region: &str, image_id: &str) -> Result<()> {
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
            "true".to_string(),
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
                .unwrap_or(1800),
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

    fn delete_cloud_image(&self, cli: &Cli, region: &str, image_id: &str) -> Result<()>;
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
            vec![image_path.to_path_buf()],
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

    fn delete_cloud_image(&self, cli: &Cli, region: &str, image_id: &str) -> Result<()> {
        delete_cloud_image(cli, region, image_id)
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
        error: None,
    }
}

fn persist_published(
    cli: &Cli,
    state: &mut LocalServiceState,
    key: &str,
    entry: PublishedImage,
) -> Result<()> {
    state.build.published.insert(key.to_string(), entry);
    write_local_service_state(&cli.state_dir, state)
}

fn update_entry_status(entry: &mut PublishedImage, status: &str, error: Option<String>) {
    entry.status = status.to_string();
    entry.error = error;
    entry.updated_at = current_utc_timestamp();
}

struct ImportImageRequest<'a> {
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
    loop {
        match ops.describe_image_status(cli, region, image_id)?.as_deref() {
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
    let bucket = publish_bucket_name(region, &cli.state_dir);
    let object_key = publish_object_key(service_id, variant, build_id, source_sha256);
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

    ops.validate_credentials(cli, region)?;

    if let Some(image_id) = entry.image_id.clone() {
        if entry.status == STATUS_AVAILABLE {
            if ops
                .describe_image_status(cli, region, &image_id)?
                .as_deref()
                == Some("Available")
            {
                println!("[ca] image already published: {image_id}");
                return Ok(entry);
            }
            update_entry_status(
                &mut entry,
                STATUS_FAILED,
                Some("recorded cloud image is missing or unavailable".to_string()),
            );
            persist_published(cli, state, key, entry.clone())?;
        } else if entry.status == STATUS_IMPORTING {
            let import_still_running = match ops
                .describe_image_status(cli, region, &image_id)?
                .as_deref()
            {
                Some("Available") => {
                    update_entry_status(&mut entry, STATUS_AVAILABLE, None);
                    if let (Some(bucket), Some(object_key)) =
                        (entry.bucket.clone(), entry.object_key.clone())
                    {
                        entry.oss_cleaned = ops
                            .cleanup_oss_object(cli, &bucket, &object_key, region)
                            .is_ok();
                    }
                    persist_published(cli, state, key, entry.clone())?;
                    println!("[ca] image import completed: {image_id}");
                    return Ok(entry);
                }
                Some("CreateFailed") => {
                    update_entry_status(
                        &mut entry,
                        STATUS_FAILED,
                        Some("ECS image import failed".to_string()),
                    );
                    persist_published(cli, state, key, entry.clone())?;
                    ops.delete_cloud_image(cli, region, &image_id).with_context(|| {
                        format!(
                            "failed import image {image_id} could not be deleted; retry image unpublish"
                        )
                    })?;
                    entry.image_id = None;
                    entry.import_task_id = None;
                    false
                }
                Some("Creating" | "Waiting") | None => true,
                Some(other) => {
                    eprintln!("[ca] image {image_id} status: {other}");
                    true
                }
            };
            if import_still_running {
                if !wait {
                    println!("[ca] image import already in progress: {image_id}");
                    return Ok(entry);
                }
                println!("[ca] waiting for existing image import to complete: {image_id}");
                wait_for_image_available_with_ops(cli, ops, region, &image_id)?;
                update_entry_status(&mut entry, STATUS_AVAILABLE, None);
                if let (Some(bucket), Some(object_key)) =
                    (entry.bucket.clone(), entry.object_key.clone())
                {
                    entry.oss_cleaned = ops
                        .cleanup_oss_object(cli, &bucket, &object_key, region)
                        .is_ok();
                }
                persist_published(cli, state, key, entry.clone())?;
                return Ok(entry);
            }
        } else if entry.status == STATUS_FAILED {
            ops.delete_cloud_image(cli, region, &image_id)
                .with_context(|| {
                    format!(
                    "failed published image {image_id} could not be deleted; retry image unpublish"
                )
                })?;
            entry.image_id = None;
            entry.import_task_id = None;
            persist_published(cli, state, key, entry.clone())?;
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
        entry.oss_cleaned = true;
        update_entry_status(&mut entry, STATUS_AVAILABLE, None);
        persist_published(cli, state, key, entry.clone())?;
        println!("[ca] adopted existing published image: {existing_id}");
        return Ok(entry);
    }

    if entry.status != STATUS_UPLOADED {
        update_entry_status(&mut entry, STATUS_UPLOADING, None);
        persist_published(cli, state, key, entry.clone())?;
        println!("[ca] uploading image to OSS ({source_size} bytes)...");
        ops.ensure_publish_bucket(cli, &bucket, region)?;
        ops.upload_image(cli, image_path, &bucket, &object_key, region)?;
        update_entry_status(&mut entry, STATUS_UPLOADED, None);
        persist_published(cli, state, key, entry.clone())?;
    }

    println!("[ca] importing ECS image '{image_name}'...");
    let imported = ops.import_image(
        cli,
        ImportImageRequest {
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
    if let (Some(bucket), Some(object_key)) = (entry.bucket.clone(), entry.object_key.clone()) {
        println!("[ca] cleaning up OSS object...");
        entry.oss_cleaned = ops
            .cleanup_oss_object(cli, &bucket, &object_key, region)
            .is_ok();
    }
    update_entry_status(&mut entry, STATUS_AVAILABLE, None);
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

fn cmd_image_unpublish_with_ops(
    cli: &Cli,
    ops: &impl PublishCloudOps,
    args: &ImageUnpublishArgs,
) -> Result<()> {
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
    if !args.force && matches!(state.phase.as_str(), "active" | "deployed") {
        if let Some(deployed_id) = state.deploy.published_image_id.as_deref() {
            if keys
                .iter()
                .any(|key| state.build.published[key].image_id.as_deref() == Some(deployed_id))
            {
                bail!(
                    "service '{}' is {} and uses published image {}; use --force to unpublish it",
                    args.service,
                    state.phase,
                    deployed_id
                );
            }
        }
    }

    let mut removed = 0usize;
    for key in keys {
        let mut entry = state.build.published[&key].clone();
        let mut failed = false;
        let mut deleted_image_id = None;
        if let Some(image_id) = entry.image_id.clone() {
            println!("[ca] deleting ECS image {image_id} ({key})...");
            if let Err(err) = ops.delete_cloud_image(cli, &entry.region, &image_id) {
                eprintln!("[ca] warning: failed to delete image {image_id}: {err:#}");
                failed = true;
            } else {
                deleted_image_id = Some(image_id);
                entry.image_id = None;
                entry.import_task_id = None;
            }
        }
        if !entry.oss_cleaned {
            if let (Some(bucket), Some(object_key)) =
                (entry.bucket.as_deref(), entry.object_key.as_deref())
            {
                if let Err(err) = ops.cleanup_oss_object(cli, bucket, object_key, &entry.region) {
                    eprintln!("[ca] warning: failed to cleanup OSS object: {err:#}");
                    failed = true;
                }
            }
        }
        if failed {
            update_entry_status(
                &mut entry,
                STATUS_FAILED,
                Some("cloud resource deletion failed; retry image unpublish".to_string()),
            );
            state.build.published.insert(key, entry);
        } else {
            state.build.published.remove(&key);
            removed += 1;
        }
        if let Some(deleted) = deleted_image_id.as_deref() {
            if state.deploy.published_image_id.as_deref() == Some(deleted) {
                state.deploy.published_image_id = None;
            }
        }
    }
    write_local_service_state(&cli.state_dir, &state)?;
    println!(
        "[ca] unpublished {} image(s) for service '{}'",
        removed, args.service
    );
    Ok(())
}

fn published_entry_is_deployed(state: &LocalServiceState, entry: &PublishedImage) -> bool {
    matches!(state.phase.as_str(), "active" | "deployed")
        && entry.image_id.is_some()
        && state.deploy.published_image_id.as_deref() == entry.image_id.as_deref()
}

fn should_prune_published_entry(
    state: &LocalServiceState,
    entry: &PublishedImage,
    all: bool,
) -> bool {
    (all || entry.build_id != state.build.build_id || entry.status == STATUS_FAILED)
        && !published_entry_is_deployed(state, entry)
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
    for mut state in read_service_states(&cli.state_dir)? {
        let keys = state
            .build
            .published
            .iter()
            .filter(|(_, entry)| should_prune_published_entry(&state, entry, args.all))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            let mut entry = state.build.published[&key].clone();
            if args.dry_run {
                println!(
                    "[ca] would prune service={} key={} image_id={}",
                    state.service_id,
                    key,
                    entry.image_id.as_deref().unwrap_or("-")
                );
            } else {
                if let Some(image_id) = entry.image_id.clone() {
                    if let Err(err) = ops.delete_cloud_image(cli, &entry.region, &image_id) {
                        eprintln!("[ca] warning: failed to delete image {image_id}: {err:#}");
                        update_entry_status(
                            &mut entry,
                            STATUS_FAILED,
                            Some("cloud image deletion failed; retry image prune".to_string()),
                        );
                        state.build.published.insert(key, entry);
                        write_local_service_state(&cli.state_dir, &state)?;
                        continue;
                    }
                    entry.image_id = None;
                    entry.import_task_id = None;
                    if state.deploy.published_image_id.as_deref() == Some(image_id.as_str()) {
                        state.deploy.published_image_id = None;
                    }
                }
                if !entry.oss_cleaned {
                    if let (Some(bucket), Some(object_key)) =
                        (entry.bucket.as_deref(), entry.object_key.as_deref())
                    {
                        if let Err(err) =
                            ops.cleanup_oss_object(cli, bucket, object_key, &entry.region)
                        {
                            eprintln!("[ca] warning: failed to cleanup OSS object: {err:#}");
                            update_entry_status(
                                &mut entry,
                                STATUS_FAILED,
                                Some("OSS object deletion failed; retry image prune".to_string()),
                            );
                            state.build.published.insert(key, entry);
                            write_local_service_state(&cli.state_dir, &state)?;
                            continue;
                        }
                        entry.oss_cleaned = true;
                    }
                }
                state.build.published.remove(&key);
                write_local_service_state(&cli.state_dir, &state)?;
            }
            total += 1;
        }
    }
    if args.dry_run {
        println!("[ca] would prune {total} published image(s)");
    } else {
        println!("[ca] pruned {total} published image(s)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

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
    fn publish_bucket_name_is_deterministic() {
        let first = publish_bucket_name("cn-beijing", Path::new("/state"));
        let second = publish_bucket_name("cn-beijing", Path::new("/state"));
        assert_eq!(first, second);
        assert!(first.starts_with("ca-images-cn-beijing-"));
        assert!(first.len() <= 63);
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
    fn cloud_envs_do_not_need_credentials_to_construct() {
        let envs = cloud_credential_envs("cn-beijing");
        assert!(envs
            .iter()
            .any(|(key, value)| key == "ALIBABA_CLOUD_REGION" && value == "cn-beijing"));
    }

    #[test]
    fn import_image_args_pin_qcow2_format() {
        let args = import_image_args(ImportImageRequest {
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

        assert!(!should_prune_published_entry(&state, &entry, true));
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
    fn publish_upload_failure_persists_uploading_state_for_retry() {
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
        let written =
            read_service_state_file(&context_paths(temp.path(), "openclaw").service_state)
                .unwrap()
                .unwrap();
        assert_eq!(written.build.published[&key].status, STATUS_UPLOADING);
        assert!(written.build.published[&key].image_id.is_none());
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

        cmd_image_unpublish_with_ops(
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
        .unwrap();

        let written =
            read_service_state_file(&context_paths(temp.path(), "openclaw").service_state)
                .unwrap()
                .unwrap();
        let remaining = written.build.published.get(&key).unwrap();
        assert_eq!(remaining.status, STATUS_FAILED);
        assert!(remaining.image_id.is_none());
        assert!(remaining.import_task_id.is_none());
        assert_eq!(written.deploy.published_image_id, None);
        assert_eq!(ops.calls(), vec!["delete:m-2ze123abc", "cleanup-oss"]);
    }

    #[test]
    fn unpublish_delete_failure_keeps_failed_entry_with_image_id() {
        let temp = tempfile::tempdir().unwrap();
        let cli = test_cli(temp.path());
        let mut state = test_state("openclaw");
        let key = "aliyun/cn-beijing/release/openclaw-agent-release/source".to_string();
        let mut entry = test_published_entry(&state.build.build_id, "source");
        entry.oss_cleaned = true;
        state.build.published.insert(key.clone(), entry);
        write_local_service_state(temp.path(), &state).unwrap();
        let ops = FakePublishCloudOps {
            delete_error: Some("delete failed".to_string()),
            ..FakePublishCloudOps::default()
        };

        cmd_image_unpublish_with_ops(
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
        .unwrap();

        let written =
            read_service_state_file(&context_paths(temp.path(), "openclaw").service_state)
                .unwrap()
                .unwrap();
        let remaining = written.build.published.get(&key).unwrap();
        assert_eq!(remaining.status, STATUS_FAILED);
        assert_eq!(remaining.image_id.as_deref(), Some("m-2ze123abc"));
        assert_eq!(ops.calls(), vec!["delete:m-2ze123abc"]);
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
        let remaining = written.build.published.get(&key).unwrap();
        assert_eq!(remaining.status, STATUS_FAILED);
        assert!(remaining.image_id.is_none());
        assert!(remaining.import_task_id.is_none());
        assert_eq!(ops.calls(), vec!["delete:m-2ze123abc", "cleanup-oss"]);
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
        assert_eq!(ops.calls(), vec!["delete:m-2ze123abc"]);
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
            error: None,
        }
    }

    fn test_cli(state_dir: &Path) -> Cli {
        Cli {
            command: Commands::Version,
            shelter_bin: PathBuf::from("shelter"),
            state_dir: state_dir.to_path_buf(),
            tools_image: "confidential-agent-tools:test".to_string(),
        }
    }

    struct FakePublishCloudOps {
        calls: RefCell<Vec<String>>,
        existing_image: RefCell<Option<String>>,
        statuses: RefCell<VecDeque<Option<String>>>,
        timeout: Duration,
        poll_interval: Duration,
        upload_error: Option<String>,
        import_error: Option<String>,
        cleanup_error: Option<String>,
        delete_error: Option<String>,
    }

    impl Default for FakePublishCloudOps {
        fn default() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                existing_image: RefCell::new(None),
                statuses: RefCell::new(VecDeque::new()),
                timeout: Duration::from_secs(60),
                poll_interval: Duration::ZERO,
                upload_error: None,
                import_error: None,
                cleanup_error: None,
                delete_error: None,
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

        fn delete_cloud_image(&self, _cli: &Cli, _region: &str, image_id: &str) -> Result<()> {
            self.record(format!("delete:{image_id}"));
            if let Some(err) = &self.delete_error {
                bail!("{err}");
            }
            Ok(())
        }
    }
}
