use super::*;

pub(super) fn tool_path(tools_dir: &Path, tool: &str) -> Result<PathBuf> {
    let mut candidates = vec![tools_dir.join(tool)];
    if tool == "rekor-cli" {
        candidates.push(PathBuf::from("/usr/libexec/shelter/bin/rekor-cli"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join(tool)));
    }
    candidates.into_iter().find(|path| {
        path.is_file() && fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }).map(|path| fs::canonicalize(&path).unwrap_or(path))
        .with_context(|| format!("native tool '{tool}' is missing; install the cagent RPM and its dependencies, or set --tools-dir"))
}

pub(super) fn native_tools_path(tools_dir: &Path) -> Result<OsString> {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![
        absolute_path(tools_dir)?,
        PathBuf::from("/usr/libexec/shelter/bin"),
    ];
    paths.extend(std::env::split_paths(&current));
    std::env::join_paths(paths).context("failed to construct native tools PATH")
}

pub(super) fn native_tool_command(tools_dir: &Path, spec: NativeToolSpec) -> Result<Command> {
    let mut command = Command::new(tool_path(tools_dir, spec.tool)?);
    command
        .args(spec.tool_args)
        .env("PATH", native_tools_path(tools_dir)?);
    if let Some(workdir) = spec.workdir {
        command.current_dir(workdir);
    }
    if spec.tool == "cosign" {
        command.env(
            "COSIGN_PASSWORD",
            std::env::var_os("COSIGN_PASSWORD").unwrap_or_default(),
        );
    }
    command.envs(spec.envs);
    Ok(command)
}

pub(super) fn run_native_tool(cli: &Cli, spec: NativeToolSpec, inherit_stdio: bool) -> Result<()> {
    let mut command = native_tool_command(&cli.tools_dir, spec)?;
    if inherit_stdio {
        let status = command.status().context("failed to execute native tool")?;
        anyhow::ensure!(status.success(), "native tool exited with status {status}");
    } else {
        let output = command
            .stdin(Stdio::null())
            .output()
            .context("failed to execute native tool")?;
        anyhow::ensure!(
            output.status.success(),
            "native tool exited with status {}; stderr: {}; stdout: {}",
            output.status,
            summarize_command_bytes(&output.stderr),
            summarize_command_bytes(&output.stdout)
        );
    }
    Ok(())
}

pub(super) fn run_native_tool_output(
    tools_dir: &Path,
    spec: NativeToolSpec,
) -> Result<std::process::Output> {
    native_tool_command(tools_dir, spec)?
        .stdin(Stdio::null())
        .output()
        .context("failed to execute native tool")
}

pub(super) fn summarize_command_bytes(bytes: &[u8]) -> String {
    const MAX: usize = 4096;
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    if text.is_empty() {
        return "<empty>".to_string();
    }
    let mut chars = text.chars();
    let summary = chars.by_ref().take(MAX).collect::<String>();
    if chars.next().is_some() {
        format!("{summary}...<truncated>")
    } else {
        summary
    }
}

#[cfg(any(feature = "full", test))]
pub(super) fn dockerhub_mirror_ref(image: &str) -> Option<String> {
    let image = image.trim();
    if image.is_empty() || image.starts_with("docker.1ms.run/") {
        return None;
    }
    let mut parts = image.splitn(2, '/');
    let first = parts.next()?;
    let rest = parts.next();
    let dockerhub_path = match rest {
        None => format!("library/{}", ensure_tag(first)),
        Some(rest) if is_dockerhub_registry(first) => ensure_tag(&dockerhub_library_path(rest)),
        Some(_) if first.contains('.') || first.contains(':') || first == "localhost" => {
            return None
        }
        Some(_) => ensure_tag(image),
    };
    Some(format!("docker.1ms.run/{dockerhub_path}"))
}

#[cfg(any(feature = "full", test))]
pub(super) fn resolve_build_container_image(image: &str) -> Result<String> {
    let runtime = container_image_runtime().context(
        "build.container.image requires docker or podman on the build host to preflight image fallback",
    )?;
    if container_image_exists(&runtime, image)? {
        return Ok(image.to_string());
    }
    let primary = Command::new(&runtime)
        .arg("pull")
        .arg(image)
        .output()
        .with_context(|| format!("failed to execute '{} pull {image}'", runtime))?;
    if primary.status.success() {
        return Ok(image.to_string());
    }
    let Some(mirror) = dockerhub_mirror_ref(image) else {
        bail!(
            "{} pull '{}' failed with {}; stderr: {}",
            runtime,
            image,
            primary.status,
            summarize_command_bytes(&primary.stderr)
        );
    };
    eprintln!("[ca] {runtime} pull failed for {image}; retrying via {mirror}");
    let fallback = Command::new(&runtime)
        .arg("pull")
        .arg(&mirror)
        .output()
        .with_context(|| format!("failed to execute '{} pull {mirror}'", runtime))?;
    if !fallback.status.success() {
        bail!(
            "{} pull failed for '{}' and fallback '{}'; primary stderr: {}; fallback stderr: {}",
            runtime,
            image,
            mirror,
            summarize_command_bytes(&primary.stderr),
            summarize_command_bytes(&fallback.stderr)
        );
    }
    Ok(mirror)
}

#[cfg(any(feature = "full", test))]
fn container_image_runtime() -> Option<String> {
    for candidate in ["podman", "docker"] {
        if Command::new(candidate)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()
            .is_some_and(|status| status.success())
        {
            return Some(candidate.to_string());
        }
    }
    None
}

#[cfg(any(feature = "full", test))]
fn container_image_exists(runtime: &str, image: &str) -> Result<bool> {
    let status = Command::new(runtime)
        .args(["image", "inspect", image])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to execute '{} image inspect {image}'", runtime))?;
    Ok(status.success())
}

#[cfg(any(feature = "full", test))]
fn is_dockerhub_registry(value: &str) -> bool {
    matches!(
        value,
        "docker.io" | "index.docker.io" | "registry-1.docker.io"
    )
}

#[cfg(any(feature = "full", test))]
fn dockerhub_library_path(value: &str) -> String {
    if value.contains('/') {
        value.to_string()
    } else {
        format!("library/{value}")
    }
}

#[cfg(any(feature = "full", test))]
fn ensure_tag(value: &str) -> String {
    if value.contains('@') {
        return value.to_string();
    }
    let last = value.rsplit('/').next().unwrap_or(value);
    if last.contains(':') {
        value.to_string()
    } else {
        format!("{value}:latest")
    }
}

pub(super) fn run_attestation_client(
    cli: &Cli,
    state_dir: &Path,
    tool_args: Vec<OsString>,
    envs: Vec<(String, String)>,
    inherit_stdio: bool,
) -> Result<()> {
    run_attestation_tool(
        cli,
        state_dir,
        "attestation-challenge-client",
        tool_args,
        envs,
        inherit_stdio,
    )
}

pub(super) fn run_host_tool(
    cli: &Cli,
    tool: &'static str,
    tool_args: Vec<OsString>,
    envs: Vec<(String, String)>,
    inherit_stdio: bool,
) -> Result<()> {
    let workdir = std::env::current_dir().context("failed to resolve current working directory")?;
    let state_dir = absolute_path_for_state(&cli.state_dir);
    fs::create_dir_all(&state_dir)
        .with_context(|| format!("failed to create '{}'", state_dir.display()))?;

    run_native_tool(
        cli,
        NativeToolSpec {
            tool,
            tool_args,

            envs,
            workdir: Some(workdir),
        },
        inherit_stdio,
    )
}

pub(super) fn run_attestation_tool(
    cli: &Cli,
    state_dir: &Path,
    tool: &'static str,
    tool_args: Vec<OsString>,
    mut envs: Vec<(String, String)>,
    inherit_stdio: bool,
) -> Result<()> {
    let workdir = std::env::current_dir().context("failed to resolve current working directory")?;
    let attestation_workdir = ensure_attestation_workdir(state_dir)?;
    envs.push((
        "ATTESTATION_CHALLENGE_CLIENT_WORK_DIR".to_string(),
        attestation_workdir.to_string_lossy().to_string(),
    ));

    run_native_tool(
        cli,
        NativeToolSpec {
            tool,
            tool_args,

            envs,
            workdir: Some(workdir),
        },
        inherit_stdio,
    )
}

pub(super) fn attestation_workdir_path(state_dir: &Path) -> Result<PathBuf> {
    Ok(absolute_path(state_dir)?.join("attestation"))
}

pub(super) fn host_default_policy_path(state_dir: &Path) -> Result<PathBuf> {
    Ok(attestation_workdir_path(state_dir)?.join("token/ear/policies/opa/default.rego"))
}

pub(super) fn ensure_attestation_workdir(state_dir: &Path) -> Result<PathBuf> {
    let root = attestation_workdir_path(state_dir)?;
    let policy = host_default_policy_path(state_dir)?;
    let parent = policy
        .parent()
        .with_context(|| format!("policy path '{}' has no parent", policy.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create '{}'", parent.display()))?;
    fs::write(&policy, DEFAULT_POLICY)
        .with_context(|| format!("failed to write '{}'", policy.display()))?;
    Ok(root)
}

pub(super) fn prepare_sigstore_tools_for_process(cli: &Cli) -> Result<()> {
    std::env::set_var("PATH", native_tools_path(&cli.tools_dir)?);
    if std::env::var_os("COSIGN_PASSWORD").is_none() {
        std::env::set_var("COSIGN_PASSWORD", "");
    }
    Ok(())
}

pub(super) fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .context("failed to resolve current working directory")?
            .join(path))
    }
}

pub(super) fn inherited_proxy_envs(no_proxy_target: Option<&str>) -> Vec<(String, String)> {
    inherited_proxy_envs_from(std::env::vars(), no_proxy_target)
}

pub(super) fn outbound_proxy_configured() -> bool {
    [
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
    ]
    .iter()
    .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
}

pub(super) fn inherited_proxy_envs_from<I, K, V>(
    source: I,
    no_proxy_target: Option<&str>,
) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: Into<String>,
{
    const PROXY_KEYS: &[&str] = &[
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
    ];

    let mut envs = Vec::new();
    let mut current_no_proxy = String::new();
    for (key, value) in source {
        let key = key.as_ref();
        let value = value.into();
        if PROXY_KEYS.contains(&key) {
            envs.push((key.to_string(), value.clone()));
        }
        if (key == "no_proxy" || key == "NO_PROXY") && current_no_proxy.is_empty() {
            current_no_proxy = value.clone();
        }
    }

    let no_proxy = no_proxy_target
        .map(|target| no_proxy_with_target(&current_no_proxy, target))
        .unwrap_or(current_no_proxy);
    if !no_proxy.is_empty() {
        envs.push(("no_proxy".to_string(), no_proxy.clone()));
        envs.push(("NO_PROXY".to_string(), no_proxy));
    }

    envs
}

pub(super) fn challenge_inject_envs<I, K, V>(
    direct: bool,
    target: &str,
    source: I,
) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: Into<String>,
{
    if !direct {
        // The direct attempt already bypasses proxies for the guest.  The
        // fallback must leave the target out of NO_PROXY so environments
        // that require an HTTP proxy for all outbound traffic can reach the
        // attestation API as well.
        return inherited_proxy_envs_from(source, None);
    }

    let mut current_no_proxy = String::new();
    for (key, value) in source {
        let key = key.as_ref();
        let value = value.into();
        if (key == "no_proxy" || key == "NO_PROXY") && current_no_proxy.is_empty() {
            current_no_proxy = value;
        }
    }

    let no_proxy = no_proxy_with_target(&current_no_proxy, target);
    if no_proxy.is_empty() {
        Vec::new()
    } else {
        vec![("NO_PROXY".to_string(), no_proxy)]
    }
}

pub(super) fn challenge_inject_attempt_timeout_secs() -> u64 {
    std::env::var("CA_CHALLENGE_INJECT_ATTEMPT_TIMEOUT_SEC")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(90)
}

pub(super) fn challenge_inject_tool_args(
    tool_args: Vec<OsString>,
    timeout_secs: u64,
) -> Vec<OsString> {
    let mut wrapped = vec![
        OsString::from(format!("{timeout_secs}s")),
        OsString::from("attestation-challenge-client"),
    ];
    wrapped.extend(tool_args);
    wrapped
}

pub(super) fn challenge_inject(
    cli: &Cli,
    state_dir: &Path,
    target_ip: &str,
    resource_path: &str,
    resource_file: &Path,
    tee: &str,
) -> Result<()> {
    let api_url = format!("http://{target_ip}:8006");
    let started = Instant::now();
    let timeout = Duration::from_secs(
        std::env::var("CA_BOOTSTRAP_WAIT_TIMEOUT_SEC")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(600),
    );
    let interval = Duration::from_secs(
        std::env::var("CA_BOOTSTRAP_RETRY_INTERVAL_SEC")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(5),
    );
    loop {
        let direct_result = run_challenge_inject_once(
            cli,
            state_dir,
            &api_url,
            target_ip,
            resource_path,
            resource_file,
            tee,
            true,
        );
        if direct_result.is_ok() {
            return Ok(());
        }

        let proxied_result = run_challenge_inject_once(
            cli,
            state_dir,
            &api_url,
            target_ip,
            resource_path,
            resource_file,
            tee,
            false,
        );
        if proxied_result.is_ok() {
            return Ok(());
        }

        let last_error = format!(
            "direct attempt: {:#}; proxy-aware attempt: {:#}",
            direct_result.unwrap_err(),
            proxied_result.unwrap_err()
        );

        if started.elapsed() >= timeout {
            bail!(
                "failed to inject resource '{}' via {}; last error: {}",
                resource_path,
                api_url,
                last_error
            );
        }
        thread::sleep(interval);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_challenge_inject_once(
    cli: &Cli,
    state_dir: &Path,
    api_url: &str,
    target_ip: &str,
    resource_path: &str,
    resource_file: &Path,
    tee: &str,
    direct: bool,
) -> Result<()> {
    let envs = challenge_inject_envs(direct, target_ip, std::env::vars());

    run_attestation_tool(
        cli,
        state_dir,
        "timeout",
        challenge_inject_tool_args(
            vec![
                OsString::from("inject-resource"),
                OsString::from("--api-url"),
                OsString::from(api_url),
                OsString::from("--resource-path"),
                OsString::from(resource_path),
                OsString::from("--resource-file"),
                resource_file.as_os_str().to_os_string(),
                OsString::from("--tee"),
                OsString::from(tee),
                OsString::from("--policy"),
                OsString::from("default"),
            ],
            challenge_inject_attempt_timeout_secs(),
        ),
        envs,
        false,
    )
}

pub(super) fn no_proxy_with_target(existing: &str, target: &str) -> String {
    let trimmed_target = target.trim();
    let mut entries = existing
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    if !trimmed_target.is_empty() && !entries.iter().any(|entry| entry == trimmed_target) {
        entries.push(trimmed_target.to_string());
    }

    entries.join(",")
}

pub(super) fn allocate_local_port(
    preferred: u16,
    is_occupied: impl Fn(u16) -> bool,
) -> Result<u16> {
    for port in preferred..=u16::MAX {
        if !is_occupied(port) {
            return Ok(port);
        }
    }
    bail!("no available local port at or above {}", preferred)
}

pub(super) fn port_is_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Commands, StatusArgs};

    fn test_cli() -> Cli {
        Cli {
            command: Commands::Status(StatusArgs {
                service: None,
                json: false,
                live: false,
            }),
            shelter_bin: PathBuf::from("shelter"),
            state_dir: PathBuf::from("/work/.confidential-agent"),
            tools_dir: PathBuf::from("/usr/libexec/cagent/bin"),
        }
    }

    #[test]
    fn native_tool_preserves_arguments_cwd_and_environment_without_secret_argv() {
        let root = tempfile::tempdir().unwrap();
        let tool = root.path().join("cosign");
        fs::write(
            &tool,
            "#!/bin/sh\nprintf '%s\\n' \"$1\" \"$CA_TEST_SECRET\" \"$COSIGN_PASSWORD\" \"$PWD\"\n",
        )
        .unwrap();
        set_mode(&tool, 0o755).unwrap();
        let mut command = native_tool_command(
            root.path(),
            NativeToolSpec {
                tool: "cosign",
                tool_args: vec!["argument with spaces".into()],
                envs: vec![
                    ("CA_TEST_SECRET".into(), "test-secret".into()),
                    ("COSIGN_PASSWORD".into(), "explicit-password".into()),
                ],
                workdir: Some(root.path().to_path_buf()),
            },
        )
        .unwrap();
        assert_eq!(command.get_program(), tool.as_os_str());
        assert!(!command
            .get_args()
            .any(|arg| arg.to_string_lossy().contains("test-secret")));
        let output = command.output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "argument with spaces\ntest-secret\nexplicit-password\n{}\n",
                root.path().display()
            )
        );
    }

    #[test]
    fn guest_asset_uses_native_tool_and_reports_missing_dependency() {
        let root = tempfile::tempdir().unwrap();
        let staging = root.path().join("guest");
        fs::create_dir(&staging).unwrap();
        let source = root.path().join("cosign");
        fs::write(&source, "native-asset").unwrap();
        set_mode(&source, 0o755).unwrap();
        let mut cli = test_cli();
        cli.tools_dir = root.path().to_path_buf();
        let staged = stage_native_tool_asset(&cli, &staging, "cosign", "cosign", 0o755).unwrap();
        assert_eq!(fs::read(staged).unwrap(), b"native-asset");
        assert!(tool_path(root.path(), "missing-cagent-test-tool")
            .unwrap_err()
            .to_string()
            .contains("install the cagent RPM"));
    }

    #[test]
    fn dockerhub_mirror_ref_normalizes_common_forms() {
        assert_eq!(
            dockerhub_mirror_ref("busybox").as_deref(),
            Some("docker.1ms.run/library/busybox:latest")
        );
        assert_eq!(
            dockerhub_mirror_ref("library/busybox").as_deref(),
            Some("docker.1ms.run/library/busybox:latest")
        );
        assert_eq!(
            dockerhub_mirror_ref("docker.io/library/busybox:1.36").as_deref(),
            Some("docker.1ms.run/library/busybox:1.36")
        );
        assert_eq!(
            dockerhub_mirror_ref("docker.io/busybox").as_deref(),
            Some("docker.1ms.run/library/busybox:latest")
        );
        assert_eq!(
            dockerhub_mirror_ref("nousresearch/hermes-agent:v2026.6.5").as_deref(),
            Some("docker.1ms.run/nousresearch/hermes-agent:v2026.6.5")
        );
    }

    #[test]
    fn dockerhub_mirror_ref_ignores_non_dockerhub_registries() {
        assert_eq!(dockerhub_mirror_ref("ghcr.io/org/app:tag").as_deref(), None);
        assert_eq!(
            dockerhub_mirror_ref("docker.1ms.run/library/busybox:latest").as_deref(),
            None
        );
    }

    #[test]
    fn summarize_command_bytes_empty() {
        assert_eq!(summarize_command_bytes(b""), "<empty>");
        assert_eq!(summarize_command_bytes(b"  \n  "), "<empty>");
    }

    #[test]
    fn summarize_command_bytes_short() {
        assert_eq!(summarize_command_bytes(b"hello"), "hello");
    }

    #[test]
    fn summarize_command_bytes_truncates_long() {
        let long = "x".repeat(5000);
        let result = summarize_command_bytes(long.as_bytes());
        assert!(result.ends_with("...<truncated>"));
        assert!(result.len() < 5000);
    }

    #[test]
    fn no_proxy_with_target_appends() {
        assert_eq!(no_proxy_with_target("", "10.0.0.1"), "10.0.0.1");
        assert_eq!(
            no_proxy_with_target("localhost,127.0.0.1", "10.0.0.1"),
            "localhost,127.0.0.1,10.0.0.1"
        );
    }

    #[test]
    fn no_proxy_with_target_does_not_duplicate() {
        assert_eq!(
            no_proxy_with_target("localhost,10.0.0.1", "10.0.0.1"),
            "localhost,10.0.0.1"
        );
    }

    #[test]
    fn no_proxy_with_target_empty_target() {
        assert_eq!(no_proxy_with_target("localhost", ""), "localhost");
    }

    #[test]
    fn inherited_proxy_envs_from_forwards_proxy_vars() {
        let source = vec![
            ("http_proxy", "http://proxy:3128"),
            ("HTTPS_PROXY", "http://proxy:3129"),
            ("UNRELATED", "ignored"),
        ];
        let envs = inherited_proxy_envs_from(source, None);
        assert_eq!(envs.len(), 2);
        assert!(envs
            .iter()
            .any(|(k, v)| k == "http_proxy" && v == "http://proxy:3128"));
        assert!(envs
            .iter()
            .any(|(k, v)| k == "HTTPS_PROXY" && v == "http://proxy:3129"));
    }

    #[test]
    fn inherited_proxy_envs_from_adds_no_proxy_target() {
        let source = vec![
            ("http_proxy", "http://proxy:3128"),
            ("no_proxy", "localhost"),
        ];
        let envs = inherited_proxy_envs_from(source, Some("10.0.0.1"));
        let no_proxy = envs.iter().find(|(k, _)| k == "no_proxy").unwrap();
        assert!(no_proxy.1.contains("10.0.0.1"));
        assert!(no_proxy.1.contains("localhost"));
    }

    #[test]
    fn challenge_inject_envs_direct_mode() {
        let source = vec![
            ("http_proxy", "http://proxy:3128"),
            ("no_proxy", "localhost"),
        ];
        let envs = challenge_inject_envs(true, "10.0.0.1", source);
        assert!(envs.iter().all(|(k, _)| k != "http_proxy"));
        let no_proxy = envs.iter().find(|(k, _)| k == "NO_PROXY");
        assert!(no_proxy.is_some());
        assert!(no_proxy.unwrap().1.contains("10.0.0.1"));
    }

    #[test]
    fn challenge_inject_envs_proxy_mode_does_not_bypass_target() {
        let source = vec![
            ("http_proxy", "http://proxy:3128"),
            ("no_proxy", "localhost"),
        ];
        let envs = challenge_inject_envs(false, "10.0.0.1", source);
        assert!(envs
            .iter()
            .any(|(key, value)| key == "http_proxy" && value == "http://proxy:3128"));
        let no_proxy = envs.iter().find(|(key, _)| key == "NO_PROXY").unwrap();
        assert_eq!(no_proxy.1, "localhost");
        assert!(!no_proxy.1.contains("10.0.0.1"));
    }

    #[test]
    fn challenge_inject_tool_args_wraps_with_timeout() {
        let inner = vec![OsString::from("--flag")];
        let wrapped = challenge_inject_tool_args(inner, 120);
        let strs: Vec<String> = wrapped
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(strs[0], "120s");
        assert_eq!(strs[1], "attestation-challenge-client");
        assert_eq!(strs[2], "--flag");
    }

    #[test]
    fn allocate_local_port_returns_preferred_when_available() {
        assert_eq!(allocate_local_port(8080, |_| false).unwrap(), 8080);
    }

    #[test]
    fn allocate_local_port_skips_occupied() {
        let port = allocate_local_port(8080, |p| p < 8083).unwrap();
        assert_eq!(port, 8083);
    }

    #[test]
    fn allocate_local_port_fails_when_all_occupied() {
        assert!(allocate_local_port(u16::MAX, |_| true).is_err());
    }

    #[test]
    fn absolute_path_keeps_absolute() {
        assert_eq!(
            absolute_path(Path::new("/absolute/path")).unwrap(),
            PathBuf::from("/absolute/path")
        );
    }

    #[test]
    fn absolute_path_resolves_relative() {
        let result = absolute_path(Path::new("relative")).unwrap();
        assert!(result.is_absolute());
    }
}
