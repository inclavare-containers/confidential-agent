//! Portable Shelter artifacts and their account-local ECS publications.
use super::*;

#[derive(Deserialize)]
struct ReleaseIdentity {
    manifest_digest: String,
    artifact_alias: String,
    disk_digest: String,
    disk_size: u64,
    runtime_metadata: RuntimeIdentity,
}

#[derive(Deserialize)]
struct RuntimeIdentity {
    image_variant: String,
}

#[derive(Deserialize)]
pub(super) struct ArtifactManifest {
    pub(super) manifest_digest: String,
    outputs: Vec<ArtifactOutput>,
}

impl ArtifactManifest {
    pub(super) fn check_disk(&self, sha: &str, size: u64) -> Result<()> {
        let disk = self
            .outputs
            .iter()
            .find(|output| output.role == "disk")
            .context("artifact has no disk")?;
        anyhow::ensure!(
            disk.size == size && disk.digest.strip_prefix("sha256:") == Some(sha),
            "local build disk does not match its Shelter artifact manifest"
        );
        Ok(())
    }
}

#[derive(Deserialize)]
struct ArtifactOutput {
    role: String,
    digest: String,
    size: u64,
    path: PathBuf,
}

pub(super) fn read_artifact_manifest(path: &Path) -> Result<ArtifactManifest> {
    serde_json::from_reader(File::open(path).with_context(|| format!("open {}", path.display()))?)
        .context("read Shelter artifact manifest")
}

fn shelter_json<T: serde::de::DeserializeOwned>(cli: &Cli, args: &[OsString]) -> Result<T> {
    let output = shelter_command(cli, args)?
        .output()
        .context("run Shelter")?;
    anyhow::ensure!(
        output.status.success(),
        "Shelter {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn release_identity(cli: &Cli, path: &Path, release_digest: &str) -> Result<ReleaseIdentity> {
    shelter_json(
        cli,
        &[
            "image".into(),
            "inspect-release".into(),
            path.into(),
            "--metadata-only".into(),
            "--release-digest".into(),
            release_digest.into(),
        ],
    )
}

pub(in crate::app) struct Location {
    pub(super) account: String,
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Publication {
    Builder { service: String, key: String },
    Imported(PublishedImage),
}

fn directory(state: &Path, manifest: &str) -> Result<PathBuf> {
    let digest = manifest
        .strip_prefix("sha256:")
        .context("expected a Shelter manifest digest")?;
    anyhow::ensure!(
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid Shelter manifest digest"
    );
    Ok(absolute_path_for_state(state)
        .join("image-publications")
        .join(digest))
}

pub(super) fn catalog_for_disk(state: &Path, disk: &Path) -> Result<Option<String>> {
    let manifest = disk.with_file_name("artifact-manifest.json");
    if !state.join("image-publications").is_dir() || !manifest.is_file() {
        return Ok(None);
    }
    let digest = read_artifact_manifest(&manifest)?.manifest_digest;
    Ok(directory(state, &digest)?.is_dir().then_some(digest))
}

impl Location {
    pub(super) fn new(cli: &Cli, region: &str, manifest: &str) -> Result<Self> {
        Self::for_account(
            &cli.state_dir,
            caller_account(cli, region)?,
            region,
            manifest,
        )
    }

    pub(super) fn for_account(
        state_dir: &Path,
        account: String,
        region: &str,
        manifest: &str,
    ) -> Result<Self> {
        let dir = directory(state_dir, manifest)?;
        validate_service_id(region)?;
        Ok(Self {
            path: dir.join(&account).join(format!("{region}.json")),
            account,
        })
    }

    pub(super) fn lock(&self) -> Result<StateDirLock> {
        lock_operation_file(&self.path.with_extension("lock"))
    }

    pub(super) fn read(&self) -> Result<Option<Publication>> {
        match File::open(&self.path) {
            Ok(file) => Ok(Some(serde_json::from_reader(file)?)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub(super) fn save_imported(&self, entry: PublishedImage) -> Result<()> {
        write_json_atomic(&self.path, &Publication::Imported(entry))
    }

    pub(super) fn remove_builder(&self, service: &str, key: &str) -> Result<()> {
        let _lock = self.lock()?;
        if matches!(self.read()?, Some(Publication::Builder {service: owner, key: ref_key}) if owner == service && ref_key == key)
        {
            fs::remove_file(&self.path)?;
        }
        Ok(())
    }

    pub(super) fn record_builder(&self, service: &str, key: &str) -> Result<()> {
        write_json_atomic(
            &self.path,
            &Publication::Builder {
                service: service.into(),
                key: key.into(),
            },
        )
    }

    pub(super) fn entry(&self, cli: &Cli) -> Result<Option<PublishedImage>> {
        match self.read()? {
            Some(Publication::Imported(image)) => Ok(Some(image)),
            Some(Publication::Builder { service, key }) => Ok(read_service_state_file(
                &context_paths(&cli.state_dir, &service).service_state,
            )?
            .and_then(|mut state| state.build.published.remove(&key))),
            None => Ok(None),
        }
    }
}

pub(in crate::app) fn published_override(
    cli: &Cli,
    source: &confidential_agent_core::spec::PublishedImageSpec,
    region: &str,
) -> Result<Option<String>> {
    if !cli.state_dir.join("image-publications").is_dir() {
        return Ok(None);
    }
    let release = release_identity(cli, &source.release, &source.release_digest)?;
    Ok(publication_for_release(cli, &release, region)?.and_then(available_image_id))
}

fn publication_for_release(
    cli: &Cli,
    release: &ReleaseIdentity,
    region: &str,
) -> Result<Option<PublishedImage>> {
    if !directory(&cli.state_dir, &release.manifest_digest)?.is_dir() {
        return Ok(None);
    }
    let location = Location::new(cli, region, &release.manifest_digest)?;
    let Some(entry) = location.entry(cli)? else {
        return Ok(None);
    };
    if entry.account_id != location.account {
        return Ok(None);
    }
    let sha = release
        .disk_digest
        .strip_prefix("sha256:")
        .context("expected a SHA-256 disk digest")?;
    check_disk_binding(
        &entry,
        &release.runtime_metadata.image_variant,
        sha,
        release.disk_size,
    )?;
    Ok(Some(entry))
}

fn available_image_id(entry: PublishedImage) -> Option<String> {
    if entry.retiring || entry.status != STATUS_AVAILABLE {
        return None;
    }
    entry.image_id
}

pub(in crate::app) fn resolve_artifact(
    cli: &Cli,
    image_release: &Path,
    release_digest: &str,
    region: &str,
) -> Result<Option<String>> {
    let release = release_identity(cli, image_release, release_digest)?;
    if let Some(entry) = publication_for_release(cli, &release, region)? {
        let Some(id) = available_image_id(entry) else {
            return Ok(None);
        };
        return Ok(
            (describe_image_status(cli, region, &id)?.as_deref() == Some("Available"))
                .then_some(id),
        );
    }
    let sha = release
        .disk_digest
        .strip_prefix("sha256:")
        .context("expected a SHA-256 disk digest")?;
    find_existing_published_image(
        cli,
        region,
        "artifact",
        &release.runtime_metadata.image_variant,
        &release.artifact_alias,
        sha,
    )
}

fn verify_disk(cli: &Cli, base: &Path, release: &ReleaseIdentity) -> Result<PathBuf> {
    let file = if base.is_dir() {
        base.join("artifact-manifest.json")
    } else {
        base.to_owned()
    };
    let manifest = read_artifact_manifest(&file)?;
    #[derive(Deserialize)]
    struct Verification {
        manifest_digest: String,
    }
    let checked: Verification = shelter_json(
        cli,
        &[
            "image".into(),
            "verify".into(),
            file.as_os_str().into(),
            "--format".into(),
            "json".into(),
        ],
    )?;
    anyhow::ensure!(
        checked.manifest_digest == release.manifest_digest,
        "artifact does not match the release manifest"
    );
    let disk = manifest
        .outputs
        .iter()
        .find(|output| output.role == "disk")
        .context("artifact has no disk")?;
    anyhow::ensure!(
        manifest.manifest_digest == release.manifest_digest
            && disk.digest == release.disk_digest
            && disk.size == release.disk_size,
        "artifact disk does not match release"
    );
    let path = file
        .parent()
        .context("manifest path has no parent")?
        .join(&disk.path);
    anyhow::ensure!(
        path.metadata()?.len() == release.disk_size
            && format!("sha256:{}", sha256_file(&path)?) == release.disk_digest,
        "artifact disk does not match release"
    );
    Ok(path)
}

pub(super) fn publish_artifact(
    cli: &Cli,
    args: &ImagePublishArgs,
    input: &Path,
) -> Result<PublishedImage> {
    let release = release_identity(
        cli,
        args.image_release
            .as_deref()
            .context("expected --image-release")?,
        args.release_digest
            .as_deref()
            .context("expected --release-digest")?,
    )?;
    let disk_path = verify_disk(cli, input, &release)?;
    let region = args.region.as_deref().context("expected --region")?;
    let location = Location::new(cli, region, &release.manifest_digest)?;
    let mut lock = location.lock()?;
    let disk = || Ok(disk_path);
    let sha = release
        .disk_digest
        .strip_prefix("sha256:")
        .context("expected a SHA-256 disk digest")?;
    let source = DiskPublication {
        account: &location.account,
        manifest_digest: &release.manifest_digest,
        region,
        service_id: "artifact",
        variant: &release.runtime_metadata.image_variant,
        build_id: &release.artifact_alias,
        source_sha256: sha,
        source_size: release.disk_size,
    };
    if let Some(Publication::Builder { service, key }) = location.read()? {
        // Service operations always acquire the service lock first.
        drop(lock);
        let _service = lock_service_operation(&cli.state_dir, &service)?;
        lock = location.lock()?;
        anyhow::ensure!(
            matches!(location.read()?, Some(Publication::Builder {service: owner, key: owned_key}) if owner == service && owned_key == key),
            "builder publication changed while waiting; retry image publish"
        );
        if let Some((mut state, entry)) =
            read_service_state_file(&context_paths(&cli.state_dir, &service).service_state)?
                .and_then(|state| {
                    state
                        .build
                        .published
                        .get(&key)
                        .cloned()
                        .map(|entry| (state, entry))
                })
        {
            check_disk_binding(
                &entry,
                &release.runtime_metadata.image_variant,
                sha,
                release.disk_size,
            )?;
            return publish_disk(
                cli,
                &RealPublishCloudOps,
                DiskPublication {
                    service_id: &service,
                    ..source
                },
                Some(entry),
                disk,
                !args.no_wait,
                |entry| persist_published(cli, &mut state, &key, entry),
            );
        }
    }
    let _lock = lock;
    let prior = location.entry(cli)?;
    if let Some(entry) = &prior {
        check_disk_binding(
            entry,
            &release.runtime_metadata.image_variant,
            sha,
            release.disk_size,
        )?;
    }
    publish_disk(
        cli,
        &RealPublishCloudOps,
        source,
        prior,
        disk,
        !args.no_wait,
        |entry| location.save_imported(entry),
    )
}

pub(super) fn check_disk_binding(
    entry: &PublishedImage,
    variant: &str,
    sha: &str,
    size: u64,
) -> Result<()> {
    anyhow::ensure!(
        entry.variant == variant && entry.source_sha256 == sha && entry.source_size == size,
        "publication does not match the requested artifact disk and variant"
    );
    Ok(())
}

pub(super) fn unpublish_artifact(
    cli: &Cli,
    ops: &impl PublishCloudOps,
    args: &ImageUnpublishArgs,
) -> Result<()> {
    let region = args
        .region
        .as_deref()
        .context("choose --region for the artifact publication")?;
    let location = Location::new(cli, region, &args.service)?;
    let _lock = location.lock()?;
    let entry = match location.read()? {
        Some(Publication::Imported(entry)) => entry,
        Some(Publication::Builder { service, .. }) => {
            bail!("use image unpublish {service} --region {region} for this builder")
        }
        None => bail!("no publication for this artifact and region in the current account"),
    };
    anyhow::ensure!(
        args.variant
            .as_ref()
            .map_or(true, |variant| variant == &entry.variant)
            && args
                .image_id
                .as_deref()
                .map_or(true, |id| entry.image_id.as_deref() == Some(id)),
        "no publication matches the requested image ID and variant"
    );
    anyhow::ensure!(
        !entry.exported || args.force,
        "published image is retained by an exported release; use --force to retire it"
    );
    let _lease = entry
        .image_id
        .as_deref()
        .map(|id| lock_image_operation(&cli.state_dir, region, id, false))
        .transpose()?;
    if let Some(id) = &entry.image_id {
        if !args.force {
            let refs = image_references(&cli.state_dir, id)?;
            anyhow::ensure!(
                refs.is_empty(),
                "published image {id} is retained: {}",
                refs.join("; ")
            );
        }
    }
    retire_entry(cli, ops, entry, args.force, |entry| {
        location.save_imported(entry)
    })?;
    fs::remove_file(location.path)?;
    Ok(())
}

pub(in crate::app) fn imported_image_entries(state_dir: &Path) -> Result<Vec<ImageListEntry>> {
    let root = state_dir.join("image-publications");
    let mut rows = Vec::new();
    if !root.exists() {
        return Ok(rows);
    }
    for manifest in fs::read_dir(root)? {
        let manifest = manifest?;
        if !manifest.file_type()?.is_dir() {
            continue;
        }
        let digest = format!("sha256:{}", manifest.file_name().to_string_lossy());
        for account in fs::read_dir(manifest.path())? {
            let account = account?;
            if !account.file_type()?.is_dir() {
                continue;
            }
            for file in fs::read_dir(account.path())? {
                let path = file?.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                let Some(Publication::Imported(entry)) = (Location {
                    path: path.clone(),
                    account: account.file_name().to_string_lossy().into_owned(),
                })
                .read()?
                else {
                    continue;
                };
                let shown = format!(
                    "{}/{}:{}({})",
                    entry.account_id,
                    entry.region,
                    entry.image_id.as_deref().unwrap_or("-"),
                    entry.status
                );
                rows.push(ImageListEntry {
                    service_id: digest.clone(),
                    build_id: entry.build_id.clone(),
                    phase: Some(if entry.retiring {
                        "retiring".into()
                    } else {
                        entry.status.clone()
                    }),
                    current: false,
                    image_present: false,
                    image_size: None,
                    image_path: PathBuf::new(),
                    build_result: path,
                    published: Some(shown),
                    publications: vec![entry],
                });
            }
        }
    }
    Ok(rows)
}
