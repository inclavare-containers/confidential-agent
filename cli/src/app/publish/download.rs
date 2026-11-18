//! Download the release-bound disk into an OCI layout and let ORAS copy the rest of the graph.
use super::*;
use std::os::unix::process::CommandExt;
use std::process::ExitStatus;

const OCI_MANIFEST: &str = "Accept: application/vnd.oci.image.manifest.v1+json";
const ATTEMPTS: u32 = 8;

pub(super) struct Layout {
    pub path: PathBuf,
    pub oras: PathBuf,
    _lock: StateDirLock,
}

// The caller has already validated the immutable OCI reference.
pub(super) fn prepare(cli: &Cli, input: &str, disk: &str, size: u64) -> Result<Layout> {
    let (name, oci_digest) = input.rsplit_once('@').context("missing OCI digest")?;
    let root = absolute_path_for_state(&cli.state_dir)
        .join("prebuilt-images")
        .join(oci_digest.trim_start_matches("sha256:"));
    let lock = lock_operation_file(&root.join("download.lock"))?;
    let layout = root.join("layout");
    let oras = tool_path(&cli.tools_dir, "oras")?;
    let public = match name.split_once('/') {
        Some((host @ ("ghcr.io" | "ghcr.1ms.run"), tagged_repository)) => {
            let repository = tagged_repository.split(':').next().unwrap();
            seed_disk(host, repository, oci_digest, disk, size, &root, &layout)?
        }
        _ => false,
    };
    let mut cp = Command::new(&oras);
    cp.args(["cp", "--recursive", "--to-oci-layout"]);
    // Keep private pulls on the caller's ordinary ORAS credentials.
    let anonymous = if public {
        Some(tempfile::NamedTempFile::new_in(&root)?)
    } else {
        None
    };
    if let Some(config) = &anonymous {
        fs::write(config.path(), br#"{"auths":{}}"#)?;
        cp.arg("--from-registry-config").arg(config.path());
    }
    let mut destination = layout.as_os_str().to_owned();
    destination.push(":release");
    cp.arg(input).arg(destination);
    stop_with_parent(&mut cp);
    let status = cp
        .status()
        .context("copy Shelter artifact to the local OCI layout")?;
    anyhow::ensure!(
        status.success(),
        "ORAS copy to {} failed ({status})",
        layout.display()
    );
    Ok(Layout {
        path: layout,
        oras,
        _lock: lock,
    })
}

fn seed_disk(
    host: &str,
    repository: &str,
    oci_digest: &str,
    disk: &str,
    size: u64,
    root: &Path,
    layout: &Path,
) -> Result<bool> {
    let url = format!("https://{host}/v2/{repository}/manifests/{oci_digest}");
    let args = [
        "--header",
        OCI_MANIFEST,
        "--location",
        "--proto-redir",
        "=https",
    ];
    let mut auth = None;
    let mut manifest = metadata(&url, &args, None)?;
    if manifest.code == 401 {
        let mut bearer = Bearer::from_headers(&manifest.headers, repository)?;
        if !bearer.refresh()? {
            return Ok(false);
        }
        manifest = metadata(&url, &args, Some(&bearer.token))?;
        auth = Some(bearer);
    }
    if matches!(manifest.code, 401 | 403 | 404) {
        return Ok(false);
    }
    ensure_http(&manifest, 200, "fetch anonymous OCI manifest")?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest.body)?;
    anyhow::ensure!(
        manifest["artifactType"] == "application/vnd.shelter.artifact.v1"
            && manifest["layers"].as_array().is_some_and(|layers| layers
                .iter()
                .any(|layer| layer["digest"] == disk && layer["size"].as_u64() == Some(size))),
        "OCI artifact does not contain the disk described by the release"
    );
    let sha = disk
        .strip_prefix("sha256:")
        .filter(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .context("invalid release disk SHA-256")?;
    let blobs = layout.join("blobs/sha256");
    fs::create_dir_all(&blobs)?;
    download(
        &format!("https://{host}/v2/{repository}/blobs/{disk}"),
        auth,
        &root.join("disk.part"),
        &blobs.join(sha),
        sha,
        size,
        repository,
    )?;
    Ok(true)
}

struct Bearer {
    realm: String,
    service: String,
    scope: String,
    token: String,
}

impl Bearer {
    fn from_headers(headers: &str, repository: &str) -> Result<Self> {
        let params = headers
            .lines()
            .filter_map(|l| l.split_once(':'))
            .filter(|(name, _)| name.eq_ignore_ascii_case("www-authenticate"))
            .filter_map(|(_, v)| v.trim().split_once(' '))
            .find(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .context("registry did not return a Bearer challenge")?
            .1;
        let attr = |key| {
            params
                .split(',')
                .filter_map(|s| s.trim().split_once('='))
                .find(|(name, _)| name.eq_ignore_ascii_case(key))
                .map(|(_, v)| v.trim().trim_matches('"').to_owned())
                .with_context(|| format!("missing {key} in registry challenge"))
        };
        let realm = attr("realm")?;
        anyhow::ensure!(
            realm.starts_with("https://"),
            "registry token realm must use HTTPS"
        );
        Ok(Self {
            realm,
            service: attr("service")?,
            scope: format!("repository:{repository}:pull"),
            token: String::new(),
        })
    }

    fn refresh(&mut self) -> Result<bool> {
        let reply = metadata(
            &self.realm,
            &[
                "--get",
                "--data-urlencode",
                &format!("service={}", self.service),
                "--data-urlencode",
                &format!("scope={}", self.scope),
            ],
            None,
        )?;
        if matches!(reply.code, 401 | 403) {
            return Ok(false);
        }
        ensure_http(&reply, 200, "get anonymous registry token")?;
        #[derive(Deserialize)]
        struct Token {
            token: String,
        }
        self.token = serde_json::from_slice::<Token>(&reply.body)?.token;
        anyhow::ensure!(
            !self.token.is_empty() && !self.token.contains(['\r', '\n']),
            "invalid registry token"
        );
        Ok(true)
    }
}

// A killed cagent must not leave a writer running after the layout lock is released.
fn stop_with_parent(cmd: &mut Command) {
    let parent = std::process::id() as libc::pid_t;
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::from_raw_os_error(libc::EINTR));
            }
            Ok(())
        });
    }
}

struct Reply {
    status: ExitStatus,
    code: u16,
    headers: String,
    body: Vec<u8>,
}

fn curl(url: &str) -> Command {
    let mut cmd = Command::new("curl");
    cmd.args([
        "-q",
        "--silent",
        "--show-error",
        "--connect-timeout",
        "20",
        "--dump-header",
        "/dev/stderr",
        "--write-out",
        "\\n%{http_code}",
        url,
    ]);
    cmd
}

fn run(mut cmd: Command, token: Option<&str>) -> Result<Reply> {
    if token.is_some() {
        cmd.args(["--header", "@-"]).stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    stop_with_parent(&mut cmd);
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start registry transfer")?;
    if let Some(token) = token {
        writeln!(child.stdin.take().unwrap(), "Authorization: Bearer {token}")?;
    }
    let out = child.wait_with_output()?;
    let idx = out
        .stdout
        .iter()
        .rposition(|b| *b == b'\n')
        .context("missing registry HTTP status")?;
    Ok(Reply {
        status: out.status,
        code: std::str::from_utf8(&out.stdout[idx + 1..])?
            .trim()
            .parse()?,
        headers: String::from_utf8_lossy(&out.stderr).into_owned(),
        body: out.stdout[..idx].to_vec(),
    })
}

fn ensure_http(reply: &Reply, code: u16, action: &str) -> Result<()> {
    anyhow::ensure!(
        reply.status.success() && reply.code == code,
        "{action}: HTTP {}, curl {}",
        reply.code,
        reply.status
    );
    Ok(())
}

fn interrupted(reply: &Reply) -> bool {
    matches!(reply.status.code(), Some(18 | 28 | 52 | 55 | 56 | 92))
}

fn retry(reply: &Reply, failures: u32) -> Result<()> {
    let transient = interrupted(reply)
        || matches!(reply.status.code(), Some(6 | 7))
        || matches!(reply.code, 401 | 408 | 429 | 500 | 502 | 503 | 504);
    anyhow::ensure!(
        failures < ATTEMPTS && transient,
        "registry transfer failed (HTTP {}, curl {})",
        reply.code,
        reply.status
    );
    let delay = reply
        .headers
        .lines()
        .rev()
        .take_while(|l| !l.starts_with("HTTP/"))
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("retry-after"))
        .and_then(|(_, v)| {
            let v = v.trim();
            v.parse().ok().map(Duration::from_secs).or_else(|| {
                httpdate::parse_http_date(v)
                    .ok()
                    .map(|at| at.duration_since(SystemTime::now()).unwrap_or_default())
            })
        })
        .unwrap_or_else(|| Duration::from_secs(1 << (failures - 1).min(5)));
    thread::sleep(delay);
    Ok(())
}

fn metadata(url: &str, args: &[&str], token: Option<&str>) -> Result<Reply> {
    for failures in 1.. {
        let mut cmd = curl(url);
        cmd.args(["--max-time", "60"]).args(args);
        let reply = run(cmd, token)?;
        if reply.status.success() && !matches!(reply.code, 408 | 429 | 500 | 502 | 503 | 504) {
            return Ok(reply);
        }
        retry(&reply, failures)?;
    }
    unreachable!()
}

fn length(path: &Path) -> Result<Option<u64>> {
    match fs::metadata(path) {
        Ok(m) => Ok(Some(m.len())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn download(
    url: &str,
    mut auth: Option<Bearer>,
    part: &Path,
    blob: &Path,
    sha: &str,
    size: u64,
    repository: &str,
) -> Result<()> {
    if let Some(bytes) = length(blob)? {
        if bytes == size && sha256_file(blob)? == sha {
            return Ok(());
        }
        fs::remove_file(blob)?;
    }
    let (speed, interval) = if url.starts_with("https://ghcr.io/") {
        ("65536", "30")
    } else {
        ("1", "90")
    };
    let mut failures = 0;
    loop {
        let before = length(part)?;
        if before.is_some_and(|n| n >= size) {
            if before == Some(size) && sha256_file(part)? == sha {
                fs::rename(part, blob)?;
                return Ok(());
            }
            fs::remove_file(part)?;
            bail!("downloaded disk does not match the release SHA-256 and size");
        }
        let mut cmd = curl(url);
        cmd.args([
            "--fail",
            "--location",
            "--proto-redir",
            "=https",
            "--continue-at",
            "-",
            "--speed-limit",
            speed,
            "--speed-time",
            interval,
            "--output",
        ])
        .arg(part);
        let reply = run(cmd, auth.as_ref().map(|a| a.token.as_str()))?;
        if reply.status.success() || (interrupted(&reply) && matches!(reply.code, 200 | 206)) {
            if length(part)?.unwrap_or(0) > before.unwrap_or(0)
                || (size == 0 && reply.status.success())
            {
                failures = 0;
                continue;
            }
            anyhow::ensure!(
                !reply.status.success(),
                "registry returned no additional disk bytes"
            );
        }
        failures += 1;
        if reply.code == 401 {
            let mut bearer = match auth.take() {
                Some(bearer) => bearer,
                None => Bearer::from_headers(&reply.headers, repository)?,
            };
            anyhow::ensure!(bearer.refresh()?, "registry denied anonymous disk read");
            auth = Some(bearer);
        }
        retry(&reply, failures)?;
    }
}
