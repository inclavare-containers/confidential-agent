use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_CONFIG_PATH: &str = "/etc/cai/pep/policy.json";
const DEFAULT_SOCKET_PATH: &str = "/run/cai/pep.sock";
const GUEST_POLICY_SOURCE_PATH: &str = "/opt/confidential-agent/policies/trustee-opa-default.rego";
const DEFAULT_ATTESTATION_WORK_DIR: &str = "/var/lib/attestation";
const GUEST_REFERENCE_VALUE_LIST_PATH: &str = "/opt/confidential-agent/reference-value-list.json";

fn main() {
    if let Err(err) = run_main() {
        eprintln!("cai-pep fatal: {err}");
        std::process::exit(1);
    }
}

fn run_main() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("serve") => run_serve(&args[2..]),
        Some("submit") => run_submit(&args[2..]),
        Some("attest") => run_attest(&args[2..]),
        _ => Err(usage()),
    }
}

fn usage() -> String {
    [
        "usage:",
        "  cai-pep serve [--config /etc/cai/pep/policy.json] [--socket /run/cai/pep.sock]",
        "  cai-pep submit --command '<cmd>' [--workdir /workspace] [--socket /run/cai/pep.sock]",
        "  cai-pep attest collect-and-verify [--aa-url http://localhost:8006] [--tee tdx] [--policy default] [--claims]",
    ]
    .join("\n")
}

fn run_serve(args: &[String]) -> Result<(), String> {
    let mut config_path = DEFAULT_CONFIG_PATH.to_string();
    let mut socket_override: Option<String> = None;
    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--config" => {
                idx += 1;
                config_path = args
                    .get(idx)
                    .ok_or_else(|| "--config requires a value".to_string())?
                    .clone();
            }
            "--socket" => {
                idx += 1;
                socket_override = Some(
                    args.get(idx)
                        .ok_or_else(|| "--socket requires a value".to_string())?
                        .clone(),
                );
            }
            other => return Err(format!("unknown serve argument: {other}")),
        }
        idx += 1;
    }

    let mut config = PepConfig::load(Path::new(&config_path))?;
    if let Some(socket) = socket_override {
        config.socket_path = socket;
    }
    serve(config)
}

fn run_submit(args: &[String]) -> Result<(), String> {
    let mut socket_path = DEFAULT_SOCKET_PATH.to_string();
    let mut command: Option<String> = None;
    let mut workdir = "/workspace".to_string();
    let mut run_id = format!("manual-{}", now_ms());
    let mut session_key = "manual:cli".to_string();
    let mut agent_id = "manual".to_string();
    let mut skill_id = "cai-pep-cli".to_string();

    let mut idx = 0;
    while idx < args.len() {
        match args[idx].as_str() {
            "--socket" => {
                idx += 1;
                socket_path = args
                    .get(idx)
                    .ok_or_else(|| "--socket requires a value".to_string())?
                    .clone();
            }
            "--command" => {
                idx += 1;
                command = Some(
                    args.get(idx)
                        .ok_or_else(|| "--command requires a value".to_string())?
                        .clone(),
                );
            }
            "--workdir" => {
                idx += 1;
                workdir = args
                    .get(idx)
                    .ok_or_else(|| "--workdir requires a value".to_string())?
                    .clone();
            }
            "--run-id" => {
                idx += 1;
                run_id = args
                    .get(idx)
                    .ok_or_else(|| "--run-id requires a value".to_string())?
                    .clone();
            }
            "--session-key" => {
                idx += 1;
                session_key = args
                    .get(idx)
                    .ok_or_else(|| "--session-key requires a value".to_string())?
                    .clone();
            }
            "--agent-id" => {
                idx += 1;
                agent_id = args
                    .get(idx)
                    .ok_or_else(|| "--agent-id requires a value".to_string())?
                    .clone();
            }
            "--skill-id" => {
                idx += 1;
                skill_id = args
                    .get(idx)
                    .ok_or_else(|| "--skill-id requires a value".to_string())?
                    .clone();
            }
            other => return Err(format!("unknown submit argument: {other}")),
        }
        idx += 1;
    }

    let request = IntentEnvelope {
        method: "submit_intent".to_string(),
        id: format!("req-{}", now_ms()),
        params: IntentParams {
            version: 1,
            run_id,
            session_key,
            agent_id,
            tool_name: "exec".to_string(),
            skill_id,
            params: ExecParams {
                command: command.ok_or_else(|| "--command is required".to_string())?,
                workdir,
            },
            request_context: Some(json!({
                "provider": "manual-cli",
            })),
            security_profile_ref: None,
            issued_at_ms: now_ms(),
        },
    };

    let raw = send_request(&socket_path, &request)?;
    let parsed: Value =
        serde_json::from_str(&raw).map_err(|err| format!("invalid response JSON: {err}"))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&parsed)
            .map_err(|err| format!("failed to format response: {err}"))?
    );
    Ok(())
}

fn run_attest(args: &[String]) -> Result<(), String> {
    let request = parse_attestation_args(args)?;
    let result = execute_attestation_request(&request, 256 * 1024, 128 * 1024, 60)?;
    if !result.stdout.is_empty() {
        print!("{}", result.stdout);
    }
    if !result.stderr.is_empty() {
        eprint!("{}", result.stderr);
    }
    if result.exit_code == 0 {
        Ok(())
    } else {
        Err(format!(
            "attestation helper failed with exit code {}",
            result.exit_code
        ))
    }
}

fn serve(config: PepConfig) -> Result<(), String> {
    let socket_path = PathBuf::from(&config.socket_path);
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create socket parent {:?}: {err}", parent))?;
    }
    if socket_path.exists() {
        fs::remove_file(&socket_path)
            .map_err(|err| format!("failed to remove stale socket {:?}: {err}", socket_path))?;
    }

    let listener = UnixListener::bind(&socket_path)
        .map_err(|err| format!("failed to bind {:?}: {err}", socket_path))?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o660))
        .map_err(|err| format!("failed to chmod {:?}: {err}", socket_path))?;

    let shared = Arc::new(config);
    eprintln!(
        "{}",
        json!({
            "component": "cai-pep",
            "event": "pep_started",
            "socket_path": shared.socket_path,
            "docker_image": shared.docker_image,
            "workspace_host_path": shared.workspace_host_path,
            "allowed_workspace_prefixes": shared.allowed_workspace_prefixes,
        })
    );

    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let cfg = Arc::clone(&shared);
                thread::spawn(move || {
                    if let Err(err) = handle_stream(stream, cfg) {
                        eprintln!(
                            "{}",
                            json!({
                                "component": "cai-pep",
                                "event": "connection_error",
                                "message": err,
                            })
                        );
                    }
                });
            }
            Err(err) => {
                eprintln!(
                    "{}",
                    json!({
                        "component": "cai-pep",
                        "event": "accept_error",
                        "message": err.to_string(),
                    })
                );
            }
        }
    }
    Ok(())
}

fn handle_stream(mut stream: UnixStream, config: Arc<PepConfig>) -> Result<(), String> {
    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .map_err(|err| format!("failed to read request: {err}"))?;
    let request: IntentEnvelope =
        serde_json::from_str(&raw).map_err(|err| format!("failed to parse request: {err}"))?;
    let response = match handle_intent(&request, &config) {
        Ok(result) => IntentResponse::ok(request.id.clone(), result),
        Err(PepError::PolicyDeny {
            rule_id,
            message,
            detail,
            audit_id,
        }) => IntentResponse::deny(request.id.clone(), rule_id, message, detail, audit_id),
        Err(PepError::BadRequest(message)) => {
            IntentResponse::bad_request(request.id.clone(), message)
        }
        Err(PepError::Execution {
            message,
            audit_id,
            detail,
        }) => IntentResponse::exec_failed(request.id.clone(), message, detail, audit_id),
    };
    if let Some(error) = &response.error {
        eprintln!(
            "{}",
            json!({ "component": "cai-pep", "event": "intent_error",
            "audit_id": error.audit_id, "request_id": request.id,
            "run_id": request.params.run_id, "session_key": request.params.session_key,
            "tool_name": request.params.tool_name, "code": error.code, "rule_id": error.rule_id })
        );
    }
    let payload = serde_json::to_vec(&response)
        .map_err(|err| format!("failed to encode response JSON: {err}"))?;
    stream
        .write_all(&payload)
        .map_err(|err| format!("failed to write response: {err}"))?;
    Ok(())
}

fn handle_intent(
    request: &IntentEnvelope,
    config: &PepConfig,
) -> Result<IntentSuccessResult, PepError> {
    if request.method != "submit_intent" {
        return Err(PepError::BadRequest(format!(
            "unsupported method: {}",
            request.method
        )));
    }
    if request.params.tool_name != "exec" {
        return Err(PepError::BadRequest(format!(
            "unsupported tool_name: {}",
            request.params.tool_name
        )));
    }

    let audit_id = unique_id("audit");
    let started = Instant::now();
    let canonical_workdir =
        canonicalize_existing_dir(&request.params.params.workdir).map_err(|err| {
            PepError::PolicyDeny {
                rule_id: "fs.invalid_workdir".to_string(),
                message: format!("workdir is invalid: {err}"),
                detail: json!({ "workdir": request.params.params.workdir }),
                audit_id: audit_id.clone(),
            }
        })?;

    ensure_allowed_workdir(&canonical_workdir, config, &audit_id)?;
    ensure_command_policy(&request.params.params.command, config, &audit_id)?;

    let (backend, sandbox_profile, exec) = if let Some(attestation) =
        try_parse_attestation_shell_command(&request.params.params.command, &audit_id)?
    {
        (
            "host-attestation".to_string(),
            "attestation-helper".to_string(),
            execute_attestation_request(
                &attestation,
                config.stdout_max_bytes,
                config.stderr_max_bytes,
                config.timeout_secs.max(60),
            )
            .map_err(|message| PepError::Execution {
                message,
                audit_id: audit_id.clone(),
                detail: json!({
                    "workdir": canonical_workdir.display().to_string(),
                    "mode": "host-attestation",
                }),
            })?,
        )
    } else {
        (
            "docker".to_string(),
            "default-docker".to_string(),
            run_exec_in_docker(config, &canonical_workdir, &request.params.params.command)
                .map_err(|message| PepError::Execution {
                    message,
                    audit_id: audit_id.clone(),
                    detail: json!({
                        "workdir": canonical_workdir.display().to_string(),
                        "mode": "docker",
                    }),
                })?,
        )
    };

    let result = IntentSuccessResult {
        status: "ok".to_string(),
        decision: "allow".to_string(),
        backend,
        sandbox_profile,
        stdout: exec.stdout,
        stderr: exec.stderr,
        exit_code: exec.exit_code,
        duration_ms: started.elapsed().as_millis() as u64,
        audit_id: audit_id.clone(),
    };

    eprintln!(
        "{}",
        json!({
            "component": "cai-pep",
            "event": "intent_allow",
            "audit_id": audit_id,
            "request_id": request.id,
            "backend": result.backend,
            "tool_name": request.params.tool_name,
            "run_id": request.params.run_id,
            "session_key": request.params.session_key,
            "agent_id": request.params.agent_id,
            "skill_id": request.params.skill_id,
            "command": summarize_command(&request.params.params.command),
            "workdir": canonical_workdir.display().to_string(),
            "exit_code": result.exit_code,
            "duration_ms": result.duration_ms,
        })
    );

    Ok(result)
}

fn ensure_allowed_workdir(
    workdir: &Path,
    config: &PepConfig,
    audit_id: &str,
) -> Result<(), PepError> {
    for allowed in &config.allowed_workspace_prefixes {
        if let Ok(prefix) = canonicalize_existing_dir(allowed) {
            if workdir.starts_with(&prefix) {
                return Ok(());
            }
        }
    }
    Err(PepError::PolicyDeny {
        rule_id: "fs.workdir_prefix".to_string(),
        message: "workdir is outside allowed workspace prefixes".to_string(),
        detail: json!({
            "workdir": workdir.display().to_string(),
            "allowed_prefixes": config.allowed_workspace_prefixes,
        }),
        audit_id: audit_id.to_string(),
    })
}

fn ensure_command_policy(
    command: &str,
    config: &PepConfig,
    audit_id: &str,
) -> Result<(), PepError> {
    let lowered = command.to_ascii_lowercase();
    for pattern in &config.denied_command_patterns {
        if lowered.contains(&pattern.to_ascii_lowercase()) {
            return Err(PepError::PolicyDeny {
                rule_id: "command.deny_pattern".to_string(),
                message: format!("command contains denied pattern: {pattern}"),
                detail: json!({ "pattern": pattern }),
                audit_id: audit_id.to_string(),
            });
        }
    }
    for prefix in &config.denied_path_prefixes {
        if lowered.contains(&prefix.to_ascii_lowercase()) {
            return Err(PepError::PolicyDeny {
                rule_id: "fs.deny_prefix".to_string(),
                message: format!("requested path is not allowed: {prefix}"),
                detail: json!({ "path_prefix": prefix }),
                audit_id: audit_id.to_string(),
            });
        }
    }
    Ok(())
}

fn try_parse_attestation_shell_command(
    command: &str,
    audit_id: &str,
) -> Result<Option<AttestationRequest>, PepError> {
    if !command.contains("cai-pep attest") && !command.contains("/cai-pep attest") {
        return Ok(None);
    }
    let parts = shlex::split(command).ok_or_else(|| PepError::PolicyDeny {
        rule_id: "command.attest_parse".to_string(),
        message: "failed to parse cai-pep attest command".to_string(),
        detail: json!({ "command": summarize_command(command) }),
        audit_id: audit_id.to_string(),
    })?;
    if parts.len() < 2 {
        return Ok(None);
    }
    let binary = &parts[0];
    if !(binary == "cai-pep" || binary.ends_with("/cai-pep")) || parts[1] != "attest" {
        return Ok(None);
    }

    let args: Vec<String> = parts[2..].to_vec();
    parse_attestation_args(&args)
        .map(Some)
        .map_err(|message| PepError::PolicyDeny {
            rule_id: "command.attest_invalid".to_string(),
            message,
            detail: json!({ "command": summarize_command(command) }),
            audit_id: audit_id.to_string(),
        })
}

fn parse_attestation_args(args: &[String]) -> Result<AttestationRequest, String> {
    let mut aa_url = "http://localhost:8006".to_string();
    let mut tee = "tdx".to_string();
    let mut policy = "default".to_string();
    let mut claims = false;

    let mut idx = 0;
    let subcommand = args
        .first()
        .ok_or_else(|| "attest requires a subcommand".to_string())?;
    if subcommand != "collect-and-verify" {
        return Err(format!("unsupported attest subcommand: {subcommand}"));
    }
    idx += 1;

    while idx < args.len() {
        match args[idx].as_str() {
            "--aa-url" => {
                idx += 1;
                aa_url = args
                    .get(idx)
                    .ok_or_else(|| "--aa-url requires a value".to_string())?
                    .clone();
            }
            "--tee" => {
                idx += 1;
                tee = args
                    .get(idx)
                    .ok_or_else(|| "--tee requires a value".to_string())?
                    .clone();
            }
            "--policy" => {
                idx += 1;
                policy = args
                    .get(idx)
                    .ok_or_else(|| "--policy requires a value".to_string())?
                    .clone();
            }
            "--claims" => {
                claims = true;
            }
            other => return Err(format!("unknown attest argument: {other}")),
        }
        idx += 1;
    }

    Ok(AttestationRequest {
        aa_url,
        tee,
        policy,
        claims,
    })
}

fn execute_attestation_request(
    request: &AttestationRequest,
    stdout_max_bytes: usize,
    stderr_max_bytes: usize,
    timeout_secs: u64,
) -> Result<ExecOutput, String> {
    let mut prepare_warnings = String::new();
    if let Err(err) =
        ensure_attestation_policy(Path::new(GUEST_POLICY_SOURCE_PATH), &attestation_work_dir())
    {
        prepare_warnings.push_str(&format!("cai-pep attest: policy setup skipped: {err}\n"));
    }
    if let Some(mut load) = reference_value_load_command(Path::new(GUEST_REFERENCE_VALUE_LIST_PATH))
    {
        load.stdout(Stdio::piped()).stderr(Stdio::piped());
        match run_command_and_capture(&mut load, stdout_max_bytes, stderr_max_bytes, timeout_secs) {
            Ok(output) if output.exit_code != 0 => {
                prepare_warnings.push_str(&format!(
                    "cai-pep attest: reference value load failed: {}\n",
                    output.stderr.trim_end()
                ));
            }
            Ok(_) => {}
            Err(err) => {
                prepare_warnings.push_str(&format!(
                    "cai-pep attest: reference value load failed: {err}\n"
                ));
            }
        }
    }

    let evidence_path = format!(
        "/tmp/cai-pep-attestation-{}-{}.json",
        now_ms(),
        std::process::id()
    );

    let mut get_evidence = Command::new("/usr/bin/attestation-challenge-client");
    get_evidence
        .arg("get-evidence")
        .arg("--aa-url")
        .arg(&request.aa_url)
        .arg("--output")
        .arg(&evidence_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let get_output = run_command_and_capture(
        &mut get_evidence,
        stdout_max_bytes,
        stderr_max_bytes,
        timeout_secs,
    )?;
    if get_output.exit_code != 0 {
        let _ = fs::remove_file(&evidence_path);
        return Ok(get_output);
    }

    let mut verify = Command::new("/usr/bin/attestation-challenge-client");
    verify
        .arg("verify")
        .arg("--evidence")
        .arg(&evidence_path)
        .arg("--tee")
        .arg(&request.tee)
        .arg("--policy")
        .arg(&request.policy);
    if request.claims {
        verify.arg("--claims");
    }
    verify.stdout(Stdio::piped()).stderr(Stdio::piped());

    let verify_output = run_command_and_capture(
        &mut verify,
        stdout_max_bytes,
        stderr_max_bytes,
        timeout_secs,
    )?;
    let _ = fs::remove_file(&evidence_path);

    let stderr = match (
        get_output.stderr.trim_end(),
        verify_output.stderr.trim_end(),
    ) {
        ("", "") => String::new(),
        ("", right) => format!("{right}\n"),
        (left, "") => format!("{left}\n"),
        (left, right) => format!("{left}\n{right}\n"),
    };
    let stderr = format!("{prepare_warnings}{stderr}");

    Ok(ExecOutput {
        stdout: verify_output.stdout,
        stderr,
        exit_code: verify_output.exit_code,
    })
}

fn attestation_work_dir() -> PathBuf {
    env::var_os("ATTESTATION_CHALLENGE_CLIENT_WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ATTESTATION_WORK_DIR))
}

fn ensure_attestation_policy(source: &Path, work_dir: &Path) -> Result<(), String> {
    let policy = fs::read(source).map_err(|err| format!("read '{}': {err}", source.display()))?;
    let target = work_dir.join("token/ear/policies/opa/default.rego");
    let parent = target
        .parent()
        .ok_or_else(|| format!("policy path '{}' has no parent", target.display()))?;
    fs::create_dir_all(parent).map_err(|err| format!("create '{}': {err}", parent.display()))?;
    fs::write(&target, policy).map_err(|err| format!("write '{}': {err}", target.display()))
}

fn reference_value_load_command(rv_path: &Path) -> Option<Command> {
    let content = fs::read_to_string(rv_path).ok()?;
    let value: Value = serde_json::from_str(&content).ok()?;
    let mut command = Command::new("/usr/bin/attestation-challenge-client");
    if value.get("rv_list").is_some() {
        command
            .arg("set-reference-value-list")
            .arg("--rv-list")
            .arg(rv_path);
    } else {
        command
            .arg("set-reference-value")
            .arg("--provenance-type")
            .arg("sample")
            .arg("--payload")
            .arg(rv_path);
    }
    Some(command)
}

fn run_exec_in_docker(
    config: &PepConfig,
    canonical_workdir: &Path,
    command: &str,
) -> Result<ExecOutput, String> {
    let workspace_root = canonicalize_existing_dir(&config.workspace_host_path)?;
    let container_workdir = host_to_container_path(canonical_workdir, &workspace_root, config)?;
    let metadata = fs::metadata(canonical_workdir)
        .map_err(|err| format!("failed to stat workdir {:?}: {err}", canonical_workdir))?;
    let uid = metadata.uid();
    let gid = metadata.gid();

    let container_name = unique_id("cai-pep");
    let mut child = Command::new("docker");
    child
        .arg("run")
        .arg("--name")
        .arg(&container_name)
        .arg("--label")
        .arg("confidential-agent.cai-pep=true")
        .arg("--rm")
        .arg("--init")
        .arg("--read-only")
        .arg("--network")
        .arg(&config.docker_network_mode)
        .arg("--cap-drop")
        .arg("ALL")
        .arg("--security-opt")
        .arg("no-new-privileges")
        .arg("--pids-limit")
        .arg(config.pids_limit.to_string())
        .arg("--memory")
        .arg(format!("{}m", config.memory_mb))
        .arg("--cpus")
        .arg(config.cpus.to_string())
        .arg("--tmpfs")
        .arg("/tmp:rw,noexec,nosuid,nodev,size=64m")
        .arg("--tmpfs")
        .arg("/run:rw,noexec,nosuid,nodev,size=16m")
        .arg("--user")
        .arg(format!("{uid}:{gid}"))
        .arg("-v")
        .arg(format!(
            "{}:{}:rw",
            workspace_root.display(),
            config.workspace_mount_target
        ))
        .arg("--workdir")
        .arg(container_workdir)
        .arg(&config.docker_image)
        .arg("/bin/sh")
        .arg("-lc")
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    run_command_and_capture_with_cleanup(
        &mut child,
        config.stdout_max_bytes,
        config.stderr_max_bytes,
        config.timeout_secs,
        Some(&container_name),
    )
}

fn run_command_and_capture(
    child: &mut Command,
    stdout_max_bytes: usize,
    stderr_max_bytes: usize,
    timeout_secs: u64,
) -> Result<ExecOutput, String> {
    run_command_and_capture_with_cleanup(
        child,
        stdout_max_bytes,
        stderr_max_bytes,
        timeout_secs,
        None,
    )
}

fn remove_sandbox(name: &str) -> Result<(), String> {
    remove_sandbox_with_runtime("docker", name, Duration::from_secs(3))
}

fn remove_sandbox_with_runtime(runtime: &str, name: &str, timeout: Duration) -> Result<(), String> {
    // A regular file avoids a stuck runtime or its descendants holding a pipe
    // open after the cleanup deadline. Never wait indefinitely for cleanup.
    let error_path = env::temp_dir().join(unique_id("cai-pep-cleanup"));
    let error_file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&error_path)
        .map_err(|err| format!("sandbox cleanup diagnostics: {err}"))?;
    let spawned = Command::new(runtime)
        .args(["rm", "--force", name])
        .stdout(Stdio::null())
        .stderr(Stdio::from(error_file))
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(err) => {
            let _ = fs::remove_file(&error_path);
            return Err(format!("sandbox cleanup failed: {err}"));
        }
    };
    let started = Instant::now();
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut diagnostic = String::new();
                if let Ok(file) = fs::File::open(&error_path) {
                    let _ = file.take(4096).read_to_string(&mut diagnostic);
                }
                let diagnostic = diagnostic.to_ascii_lowercase();
                break if status.success()
                    || diagnostic.contains("no such container")
                    || diagnostic.contains("no container with name")
                {
                    Ok(())
                } else {
                    Err(format!("sandbox cleanup failed for {name}: {diagnostic}"))
                };
            }
            Err(err) => break Err(format!("sandbox cleanup wait failed: {err}")),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!(
                    "sandbox cleanup timed out for {name}; removal is unconfirmed"
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
        }
    };
    let _ = fs::remove_file(&error_path);
    result
}

fn run_command_and_capture_with_cleanup(
    child: &mut Command,
    stdout_max_bytes: usize,
    stderr_max_bytes: usize,
    timeout_secs: u64,
    container_name: Option<&str>,
) -> Result<ExecOutput, String> {
    let mut child = child
        .spawn()
        .map_err(|err| format!("failed to spawn command: {err}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to capture stderr".to_string())?;

    let stdout_reader = spawn_capped_reader(stdout, stdout_max_bytes);
    let stderr_reader = spawn_capped_reader(stderr, stderr_max_bytes);

    let timeout = Duration::from_secs(timeout_secs);
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
            break status;
        }
        if start.elapsed() >= timeout {
            timed_out = true;
            let _ = child.kill();
            // Killing a Docker/Podman client does not stop its container. Stop
            // the sandbox before joining readers, whose pipes it may hold open.
            let status = child.wait().map_err(|err| err.to_string())?;
            if let Some(name) = container_name {
                remove_sandbox(name)?;
            }
            break status;
        }
        thread::sleep(Duration::from_millis(100));
    };

    if !timed_out {
        if let Some(name) = container_name {
            remove_sandbox(name)?;
        }
    }
    let (stdout_buf, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| "stdout reader thread panicked".to_string())?;
    let (stderr_buf, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| "stderr reader thread panicked".to_string())?;

    let mut stdout_text = String::from_utf8_lossy(&stdout_buf).into_owned();
    let mut stderr_text = String::from_utf8_lossy(&stderr_buf).into_owned();
    if stdout_truncated {
        stdout_text.push_str("\n[truncated by cai-pep]");
    }
    if stderr_truncated {
        stderr_text.push_str("\n[truncated by cai-pep]");
    }
    if timed_out {
        if !stderr_text.is_empty() {
            stderr_text.push('\n');
        }
        stderr_text.push_str("[timed out by cai-pep]");
    }

    Ok(ExecOutput {
        stdout: stdout_text,
        stderr: stderr_text,
        exit_code: status.code().unwrap_or(if timed_out { 124 } else { 137 }),
    })
}

fn host_to_container_path(
    workdir: &Path,
    workspace_root: &Path,
    config: &PepConfig,
) -> Result<String, String> {
    let relative = workdir
        .strip_prefix(workspace_root)
        .map_err(|_| "workdir is outside workspace root".to_string())?;
    if relative.as_os_str().is_empty() {
        return Ok(config.workspace_mount_target.clone());
    }
    Ok(format!(
        "{}/{}",
        config.workspace_mount_target.trim_end_matches('/'),
        relative.display()
    ))
}

fn spawn_capped_reader<R>(mut reader: R, max_bytes: usize) -> thread::JoinHandle<(Vec<u8>, bool)>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut tmp = [0_u8; 8192];
        let mut truncated = false;
        loop {
            match reader.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    let remaining = max_bytes.saturating_sub(buf.len());
                    if remaining > 0 {
                        let take = remaining.min(n);
                        buf.extend_from_slice(&tmp[..take]);
                        if take < n {
                            truncated = true;
                        }
                    } else {
                        truncated = true;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        (buf, truncated)
    })
}

fn canonicalize_existing_dir(path: &str) -> Result<PathBuf, String> {
    let candidate = Path::new(path);
    if !candidate.exists() {
        return Err(format!("path does not exist: {path}"));
    }
    let canonical =
        fs::canonicalize(candidate).map_err(|err| format!("canonicalize failed: {err}"))?;
    if !canonical.is_dir() {
        return Err(format!("path is not a directory: {}", canonical.display()));
    }
    Ok(canonical)
}

fn send_request(socket_path: &str, request: &IntentEnvelope) -> Result<String, String> {
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|err| format!("failed to connect to {socket_path}: {err}"))?;
    let body =
        serde_json::to_vec(request).map_err(|err| format!("failed to encode request: {err}"))?;
    stream
        .write_all(&body)
        .map_err(|err| format!("failed to write request: {err}"))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|err| format!("failed to close request stream: {err}"))?;
    let mut raw = String::new();
    stream
        .read_to_string(&mut raw)
        .map_err(|err| format!("failed to read response: {err}"))?;
    Ok(raw)
}

fn summarize_command(command: &str) -> String {
    let trimmed = command.trim();
    if trimmed.len() <= 120 {
        return trimmed.to_string();
    }
    format!("{}...", &trimmed[..120])
}

fn unique_id(prefix: &str) -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    format!(
        "{prefix}-{}-{}-{}",
        now_ms(),
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug, Clone, Deserialize)]
struct PepConfig {
    #[serde(default = "default_socket_path")]
    socket_path: String,
    #[serde(default = "default_docker_image")]
    docker_image: String,
    #[serde(default = "default_workspace_host_path")]
    workspace_host_path: String,
    #[serde(default = "default_workspace_mount_target")]
    workspace_mount_target: String,
    #[serde(default = "default_allowed_workspace_prefixes")]
    allowed_workspace_prefixes: Vec<String>,
    #[serde(default = "default_denied_path_prefixes")]
    denied_path_prefixes: Vec<String>,
    #[serde(default = "default_denied_command_patterns")]
    denied_command_patterns: Vec<String>,
    #[serde(default = "default_timeout_secs")]
    timeout_secs: u64,
    #[serde(default = "default_stdout_max_bytes")]
    stdout_max_bytes: usize,
    #[serde(default = "default_stderr_max_bytes")]
    stderr_max_bytes: usize,
    #[serde(default = "default_memory_mb")]
    memory_mb: u64,
    #[serde(default = "default_cpus")]
    cpus: f64,
    #[serde(default = "default_pids_limit")]
    pids_limit: u64,
    #[serde(default = "default_docker_network_mode")]
    docker_network_mode: String,
}

impl PepConfig {
    fn load(path: &Path) -> Result<Self, String> {
        let content = fs::read_to_string(path)
            .map_err(|err| format!("failed to read config {:?}: {err}", path))?;
        serde_json::from_str(&content)
            .map_err(|err| format!("failed to parse config {:?}: {err}", path))
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct IntentEnvelope {
    method: String,
    id: String,
    params: IntentParams,
}

#[derive(Debug, Deserialize, Serialize)]
struct IntentParams {
    version: u32,
    run_id: String,
    session_key: String,
    agent_id: String,
    tool_name: String,
    skill_id: String,
    params: ExecParams,
    #[serde(default)]
    request_context: Option<Value>,
    #[serde(default)]
    security_profile_ref: Option<String>,
    issued_at_ms: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct ExecParams {
    command: String,
    workdir: String,
}

#[derive(Debug, Serialize)]
struct IntentResponse {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<IntentSuccessResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<IntentErrorResult>,
}

impl IntentResponse {
    fn ok(id: String, result: IntentSuccessResult) -> Self {
        Self {
            id,
            result: Some(result),
            error: None,
        }
    }

    fn deny(id: String, rule_id: String, message: String, detail: Value, audit_id: String) -> Self {
        Self {
            id,
            result: None,
            error: Some(IntentErrorResult {
                code: "POLICY_DENY".to_string(),
                message,
                rule_id: Some(rule_id),
                detail: Some(detail),
                audit_id: Some(audit_id),
            }),
        }
    }

    fn bad_request(id: String, message: String) -> Self {
        Self {
            id,
            result: None,
            error: Some(IntentErrorResult {
                code: "BAD_REQUEST".to_string(),
                message,
                rule_id: None,
                detail: None,
                audit_id: None,
            }),
        }
    }

    fn exec_failed(id: String, message: String, detail: Value, audit_id: String) -> Self {
        Self {
            id,
            result: None,
            error: Some(IntentErrorResult {
                code: "EXECUTION_FAILED".to_string(),
                message,
                rule_id: None,
                detail: Some(detail),
                audit_id: Some(audit_id),
            }),
        }
    }
}

#[derive(Debug, Serialize)]
struct IntentSuccessResult {
    status: String,
    decision: String,
    backend: String,
    sandbox_profile: String,
    stdout: String,
    stderr: String,
    exit_code: i32,
    duration_ms: u64,
    audit_id: String,
}

#[derive(Debug, Serialize)]
struct IntentErrorResult {
    code: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audit_id: Option<String>,
}

#[derive(Debug)]
enum PepError {
    PolicyDeny {
        rule_id: String,
        message: String,
        detail: Value,
        audit_id: String,
    },
    BadRequest(String),
    Execution {
        message: String,
        audit_id: String,
        detail: Value,
    },
}

struct ExecOutput {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

#[derive(Debug, Clone)]
struct AttestationRequest {
    aa_url: String,
    tee: String,
    policy: String,
    claims: bool,
}

fn default_socket_path() -> String {
    DEFAULT_SOCKET_PATH.to_string()
}

fn default_docker_image() -> String {
    "alibaba-cloud-linux-3-registry.cn-hangzhou.cr.aliyuncs.com/alinux3/alinux3:latest".to_string()
}

fn default_workspace_host_path() -> String {
    "/workspace".to_string()
}

fn default_workspace_mount_target() -> String {
    "/workspace".to_string()
}

fn default_allowed_workspace_prefixes() -> Vec<String> {
    vec!["/workspace".to_string()]
}

fn default_denied_path_prefixes() -> Vec<String> {
    vec![
        "/etc".to_string(),
        "/proc".to_string(),
        "/sys".to_string(),
        "/dev".to_string(),
        "/root".to_string(),
        "/run".to_string(),
        "/var/run".to_string(),
        "/home/openclaw/.openclaw".to_string(),
    ]
}

fn default_denied_command_patterns() -> Vec<String> {
    vec![
        "curl ".to_string(),
        "wget ".to_string(),
        "nc ".to_string(),
        "ncat ".to_string(),
        "ssh ".to_string(),
        "scp ".to_string(),
        "sftp ".to_string(),
        "telnet ".to_string(),
        "docker ".to_string(),
        "podman ".to_string(),
    ]
}

fn default_timeout_secs() -> u64 {
    30
}

fn default_stdout_max_bytes() -> usize {
    64 * 1024
}

fn default_stderr_max_bytes() -> usize {
    32 * 1024
}

fn default_memory_mb() -> u64 {
    512
}

fn default_cpus() -> f64 {
    1.0
}

fn default_pids_limit() -> u64 {
    128
}

fn default_docker_network_mode() -> String {
    "none".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_allowed(prefixes: Vec<String>) -> PepConfig {
        let mut config: PepConfig = serde_json::from_str("{}").unwrap();
        config.allowed_workspace_prefixes = prefixes;
        config
    }

    fn test_intent(method: &str, tool_name: &str, command: &str, workdir: &Path) -> IntentEnvelope {
        IntentEnvelope {
            method: method.to_string(),
            id: "req-test".to_string(),
            params: IntentParams {
                version: 1,
                run_id: "run-test".to_string(),
                session_key: "session-test".to_string(),
                agent_id: "agent-test".to_string(),
                tool_name: tool_name.to_string(),
                skill_id: "skill-test".to_string(),
                params: ExecParams {
                    command: command.to_string(),
                    workdir: workdir.display().to_string(),
                },
                request_context: None,
                security_profile_ref: None,
                issued_at_ms: 1,
            },
        }
    }

    fn exchange_with_handle_stream(request: &IntentEnvelope, config: PepConfig) -> Value {
        let (mut client, server) = UnixStream::pair().unwrap();
        let config = Arc::new(config);
        let handle = thread::spawn(move || handle_stream(server, config).unwrap());

        let body = serde_json::to_vec(request).unwrap();
        client.write_all(&body).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let mut raw = String::new();
        client.read_to_string(&mut raw).unwrap();
        handle.join().unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    fn deny_rule(err: PepError) -> String {
        match err {
            PepError::PolicyDeny { rule_id, .. } => rule_id,
            other => panic!("expected PolicyDeny, got {other:?}"),
        }
    }

    fn command_args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn ensure_attestation_policy_writes_and_overwrites_workdir_policy() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("policy.rego");
        fs::write(&source, "package policy\n").unwrap();
        let work_dir = dir.path().join("work");
        let target = work_dir.join("token/ear/policies/opa/default.rego");

        ensure_attestation_policy(&source, &work_dir).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "package policy\n");

        fs::write(&target, "stale upstream policy").unwrap();
        ensure_attestation_policy(&source, &work_dir).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "package policy\n");
    }

    #[test]
    fn ensure_attestation_policy_fails_when_source_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let err =
            ensure_attestation_policy(&dir.path().join("missing.rego"), dir.path()).unwrap_err();
        assert!(err.contains("missing.rego"));
    }

    #[test]
    fn reference_value_load_command_uses_list_subcommand_for_rv_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rv.json");
        fs::write(&path, r#"{"rv_list":[{"id":"x"}]}"#).unwrap();
        let command = reference_value_load_command(&path).unwrap();
        let args = command_args(&command);
        assert_eq!(args[0], "set-reference-value-list");
        assert_eq!(args[1], "--rv-list");
        assert_eq!(args[2], path.display().to_string());
    }

    #[test]
    fn reference_value_load_command_uses_sample_subcommand_for_sample_payload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rv.json");
        fs::write(&path, r#"{"measurement.uki.SHA-384":["abc"]}"#).unwrap();
        let command = reference_value_load_command(&path).unwrap();
        let args = command_args(&command);
        assert_eq!(
            args,
            vec![
                "set-reference-value".to_string(),
                "--provenance-type".to_string(),
                "sample".to_string(),
                "--payload".to_string(),
                path.display().to_string(),
            ]
        );
    }

    #[test]
    fn reference_value_load_command_skips_missing_or_invalid_files() {
        let dir = tempfile::tempdir().unwrap();
        assert!(reference_value_load_command(&dir.path().join("absent.json")).is_none());
        let invalid = dir.path().join("invalid.json");
        fs::write(&invalid, "not json").unwrap();
        assert!(reference_value_load_command(&invalid).is_none());
    }

    #[test]
    fn parse_attestation_args_uses_documented_defaults() {
        let req = parse_attestation_args(&["collect-and-verify".to_string()]).unwrap();

        assert_eq!(req.aa_url, "http://localhost:8006");
        assert_eq!(req.tee, "tdx");
        assert_eq!(req.policy, "default");
        assert!(!req.claims);
    }

    #[test]
    fn parse_attestation_args_consumes_each_flag() {
        let req = parse_attestation_args(&[
            "collect-and-verify".to_string(),
            "--aa-url".to_string(),
            "http://10.0.0.1:8006".to_string(),
            "--tee".to_string(),
            "sgx".to_string(),
            "--policy".to_string(),
            "strict".to_string(),
            "--claims".to_string(),
        ])
        .unwrap();

        assert_eq!(req.aa_url, "http://10.0.0.1:8006");
        assert_eq!(req.tee, "sgx");
        assert_eq!(req.policy, "strict");
        assert!(req.claims);
    }

    #[test]
    fn parse_attestation_args_rejects_unsupported_subcommand() {
        let err = parse_attestation_args(&["status".to_string()]).unwrap_err();
        assert!(err.contains("unsupported attest subcommand: status"));
    }

    #[test]
    fn parse_attestation_args_requires_a_subcommand() {
        let err = parse_attestation_args(&[]).unwrap_err();
        assert!(err.contains("attest requires a subcommand"));
    }

    #[test]
    fn run_attest_surfaces_parse_errors_before_invoking_helper() {
        let err = run_attest(&[]).unwrap_err();
        assert!(err.contains("attest requires a subcommand"));
    }

    #[test]
    fn parse_attestation_args_rejects_missing_value() {
        let err =
            parse_attestation_args(&["collect-and-verify".to_string(), "--aa-url".to_string()])
                .unwrap_err();
        assert!(err.contains("--aa-url requires a value"));
    }

    #[test]
    fn parse_attestation_args_rejects_unknown_flag() {
        let err =
            parse_attestation_args(&["collect-and-verify".to_string(), "--bogus".to_string()])
                .unwrap_err();
        assert!(err.contains("unknown attest argument: --bogus"));
    }

    #[test]
    fn try_parse_attestation_shell_command_matches_canonical_invocation() {
        let req = try_parse_attestation_shell_command(
            "/usr/local/bin/cai-pep attest collect-and-verify --tee tdx",
            "audit-1",
        )
        .unwrap()
        .expect("expected an attestation request to be parsed");

        assert_eq!(req.tee, "tdx");
        assert_eq!(req.aa_url, "http://localhost:8006");
    }

    #[test]
    fn try_parse_attestation_shell_command_returns_none_for_other_binary() {
        // The fast-path filter requires the literal substring; anything not
        // matching it must short-circuit to None without parsing.
        let result = try_parse_attestation_shell_command("/usr/bin/echo hello", "audit-1").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn try_parse_attestation_shell_command_returns_none_when_subcommand_is_not_attest() {
        // The substring filter accepts "cai-pep attest" but we must still
        // reject when the parsed argv shape is not `cai-pep attest …`.
        let result =
            try_parse_attestation_shell_command("/bin/cai-pep attestation foo", "audit-1").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn try_parse_attestation_shell_command_rejects_unbalanced_quotes() {
        let err = try_parse_attestation_shell_command(
            "/bin/cai-pep attest collect-and-verify --tee \"tdx",
            "audit-1",
        )
        .unwrap_err();
        assert_eq!(deny_rule(err), "command.attest_parse");
    }

    #[test]
    fn try_parse_attestation_shell_command_surfaces_attest_arg_errors() {
        let err = try_parse_attestation_shell_command(
            "/bin/cai-pep attest collect-and-verify --bogus",
            "audit-1",
        )
        .unwrap_err();
        assert_eq!(deny_rule(err), "command.attest_invalid");
    }

    #[test]
    fn ensure_allowed_workdir_accepts_canonical_prefix_match() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        let nested = workspace.join("project/sub");
        std::fs::create_dir_all(&nested).unwrap();
        let config = config_with_allowed(vec![workspace.to_string_lossy().to_string()]);

        ensure_allowed_workdir(&nested, &config, "audit-1").unwrap();
    }

    #[test]
    fn ensure_allowed_workdir_rejects_outside_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        let outside = std::env::temp_dir().canonicalize().unwrap();
        let config = config_with_allowed(vec![workspace.to_string_lossy().to_string()]);

        let err = ensure_allowed_workdir(&outside, &config, "audit-1").unwrap_err();
        assert_eq!(deny_rule(err), "fs.workdir_prefix");
    }

    #[test]
    fn ensure_allowed_workdir_skips_missing_prefix_silently() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().canonicalize().unwrap();
        // The only allowed prefix on disk does match — but we add a
        // non-existent prefix first to confirm `canonicalize_existing_dir`
        // failure does not abort the search.
        let config = config_with_allowed(vec![
            "/this/path/does/not/exist".to_string(),
            workspace.to_string_lossy().to_string(),
        ]);

        ensure_allowed_workdir(&workspace, &config, "audit-1").unwrap();
    }

    #[test]
    fn ensure_command_policy_matches_denied_pattern_case_insensitively() {
        let mut config: PepConfig = serde_json::from_str("{}").unwrap();
        config.denied_command_patterns = vec!["RM -RF".to_string()];

        let err = ensure_command_policy("rm -rf /workspace/foo", &config, "audit-1").unwrap_err();
        assert_eq!(deny_rule(err), "command.deny_pattern");
    }

    #[test]
    fn ensure_command_policy_matches_denied_path_prefix() {
        let mut config: PepConfig = serde_json::from_str("{}").unwrap();
        config.denied_command_patterns = Vec::new();
        config.denied_path_prefixes = vec!["/etc".to_string()];

        let err = ensure_command_policy("cat /etc/passwd", &config, "audit-1").unwrap_err();
        assert_eq!(deny_rule(err), "fs.deny_prefix");
    }

    #[test]
    fn ensure_command_policy_allows_safe_command() {
        let mut config: PepConfig = serde_json::from_str("{}").unwrap();
        config.denied_command_patterns = vec!["rm -rf".to_string()];
        config.denied_path_prefixes = vec!["/etc".to_string()];

        ensure_command_policy("ls /workspace", &config, "audit-1").unwrap();
    }

    #[test]
    fn host_to_container_path_maps_workspace_root_to_mount_target() {
        let config: PepConfig = serde_json::from_str("{}").unwrap();
        let root = Path::new("/workspace");

        let mapped = host_to_container_path(root, root, &config).unwrap();
        assert_eq!(mapped, "/workspace");
    }

    #[test]
    fn host_to_container_path_keeps_relative_tail() {
        let config: PepConfig = serde_json::from_str("{}").unwrap();
        let root = Path::new("/workspace");
        let nested = Path::new("/workspace/proj/sub");

        let mapped = host_to_container_path(nested, root, &config).unwrap();
        assert_eq!(mapped, "/workspace/proj/sub");
    }

    #[test]
    fn host_to_container_path_strips_trailing_slash_on_mount_target() {
        let mut config: PepConfig = serde_json::from_str("{}").unwrap();
        config.workspace_mount_target = "/sandbox/".to_string();
        let root = Path::new("/workspace");
        let nested = Path::new("/workspace/proj");

        let mapped = host_to_container_path(nested, root, &config).unwrap();
        assert_eq!(mapped, "/sandbox/proj");
    }

    #[test]
    fn host_to_container_path_rejects_workdir_outside_root() {
        let config: PepConfig = serde_json::from_str("{}").unwrap();
        let err = host_to_container_path(
            Path::new("/elsewhere/proj"),
            Path::new("/workspace"),
            &config,
        )
        .unwrap_err();
        assert!(err.contains("outside workspace root"));
    }

    #[test]
    fn summarize_command_passes_short_strings_through() {
        let short = "echo hi";
        assert_eq!(summarize_command(short), short);
    }

    #[test]
    fn summarize_command_truncates_oversize_with_ellipsis() {
        let long = "x".repeat(200);
        let summary = summarize_command(&long);
        assert!(summary.ends_with("..."));
        assert_eq!(summary.len(), 123); // 120 chars + "..."
    }

    #[test]
    fn summarize_command_trims_whitespace() {
        assert_eq!(summarize_command("  echo hi  "), "echo hi");
    }

    #[test]
    fn pep_config_uses_defaults_when_empty_json() {
        let config: PepConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.socket_path, DEFAULT_SOCKET_PATH);
        assert_eq!(config.timeout_secs, 30);
        assert_eq!(config.memory_mb, 512);
        assert_eq!(config.pids_limit, 128);
        assert_eq!(config.docker_network_mode, "none");
        assert!(!config.denied_command_patterns.is_empty());
        assert!(!config.denied_path_prefixes.is_empty());
    }

    #[test]
    fn pep_config_denied_patterns_include_security_sensitive_commands() {
        let patterns = default_denied_command_patterns();
        assert!(patterns.iter().any(|p| p.contains("curl")));
        assert!(patterns.iter().any(|p| p.contains("ssh")));
        assert!(patterns.iter().any(|p| p.contains("docker")));
    }

    #[test]
    fn pep_config_denied_path_prefixes_include_system_paths() {
        let prefixes = default_denied_path_prefixes();
        assert!(prefixes.contains(&"/etc".to_string()));
        assert!(prefixes.contains(&"/proc".to_string()));
        assert!(prefixes.contains(&"/dev".to_string()));
    }

    #[test]
    fn ensure_command_policy_allows_when_patterns_are_empty() {
        let mut config: PepConfig = serde_json::from_str("{}").unwrap();
        config.denied_command_patterns = Vec::new();
        config.denied_path_prefixes = Vec::new();

        ensure_command_policy("curl http://evil.example", &config, "audit-1").unwrap();
    }

    #[test]
    fn ensure_command_policy_denies_default_patterns() {
        let config: PepConfig = serde_json::from_str("{}").unwrap();

        for cmd in [
            "curl http://x",
            "wget http://x",
            "ssh root@host",
            "docker run x",
        ] {
            let err = ensure_command_policy(cmd, &config, "audit-1").unwrap_err();
            assert_eq!(deny_rule(err), "command.deny_pattern");
        }
    }

    #[test]
    fn ensure_command_policy_denies_access_to_sensitive_paths() {
        let config: PepConfig = serde_json::from_str("{}").unwrap();

        let err = ensure_command_policy("cat /etc/shadow", &config, "audit-1").unwrap_err();
        assert_eq!(deny_rule(err), "fs.deny_prefix");
    }

    #[test]
    fn host_to_container_path_handles_workspace_at_root() {
        let mut config: PepConfig = serde_json::from_str("{}").unwrap();
        config.workspace_mount_target = "/work".to_string();
        let root = Path::new("/data");
        let nested = Path::new("/data/project/src");

        let mapped = host_to_container_path(nested, root, &config).unwrap();
        assert_eq!(mapped, "/work/project/src");
    }

    #[test]
    fn canonicalize_existing_dir_rejects_nonexistent_path() {
        let err = canonicalize_existing_dir("/this/does/not/exist").unwrap_err();
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn canonicalize_existing_dir_rejects_file_path() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let err = canonicalize_existing_dir(temp.path().to_str().unwrap()).unwrap_err();
        assert!(err.contains("not a directory"));
    }

    #[test]
    fn usage_lists_documented_entrypoints() {
        let text = usage();

        assert!(text.contains("cai-pep serve"));
        assert!(text.contains("cai-pep submit"));
        assert!(text.contains("cai-pep attest collect-and-verify"));
    }

    #[test]
    fn run_serve_rejects_invalid_arguments_before_loading_config() {
        let err = run_serve(&["--config".to_string()]).unwrap_err();
        assert!(err.contains("--config requires a value"));

        let err = run_serve(&["--socket".to_string()]).unwrap_err();
        assert!(err.contains("--socket requires a value"));

        let err = run_serve(&["--bogus".to_string()]).unwrap_err();
        assert!(err.contains("unknown serve argument: --bogus"));
    }

    #[test]
    fn run_submit_validates_arguments_before_connecting() {
        let cases = [
            (vec![], "--command is required"),
            (vec!["--socket"], "--socket requires a value"),
            (vec!["--command"], "--command requires a value"),
            (vec!["--workdir"], "--workdir requires a value"),
            (vec!["--run-id"], "--run-id requires a value"),
            (vec!["--session-key"], "--session-key requires a value"),
            (vec!["--agent-id"], "--agent-id requires a value"),
            (vec!["--skill-id"], "--skill-id requires a value"),
            (vec!["--bogus"], "unknown submit argument: --bogus"),
        ];

        for (args, expected) in cases {
            let args = args.into_iter().map(str::to_string).collect::<Vec<_>>();
            let err = run_submit(&args).unwrap_err();
            assert!(
                err.contains(expected),
                "expected '{err}' to contain '{expected}'"
            );
        }
    }

    #[test]
    fn run_submit_sends_intent_to_socket_and_accepts_json_response() {
        let temp = tempfile::tempdir().unwrap();
        let socket_path = temp.path().join("pep.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut raw = String::new();
            stream.read_to_string(&mut raw).unwrap();
            let request: IntentEnvelope = serde_json::from_str(&raw).unwrap();

            assert_eq!(request.method, "submit_intent");
            assert_eq!(request.params.run_id, "run-1");
            assert_eq!(request.params.session_key, "session-1");
            assert_eq!(request.params.agent_id, "agent-1");
            assert_eq!(request.params.skill_id, "skill-1");
            assert_eq!(request.params.tool_name, "exec");
            assert_eq!(request.params.params.command, "echo hi");
            assert_eq!(request.params.params.workdir, "/tmp");
            assert_eq!(
                request.params.request_context.unwrap()["provider"],
                "manual-cli"
            );

            stream
                .write_all(br#"{"id":"req-test","result":{"status":"ok"}}"#)
                .unwrap();
        });

        run_submit(&[
            "--socket".to_string(),
            socket_path.display().to_string(),
            "--command".to_string(),
            "echo hi".to_string(),
            "--workdir".to_string(),
            "/tmp".to_string(),
            "--run-id".to_string(),
            "run-1".to_string(),
            "--session-key".to_string(),
            "session-1".to_string(),
            "--agent-id".to_string(),
            "agent-1".to_string(),
            "--skill-id".to_string(),
            "skill-1".to_string(),
        ])
        .unwrap();
        server.join().unwrap();
    }

    #[test]
    fn send_request_reports_connection_failures() {
        let temp = tempfile::tempdir().unwrap();
        let request = test_intent("submit_intent", "exec", "echo hi", temp.path());
        let missing = temp.path().join("missing.sock");

        let err = send_request(missing.to_str().unwrap(), &request).unwrap_err();

        assert!(err.contains("failed to connect"));
    }

    #[test]
    fn handle_stream_maps_bad_method_to_json_error() {
        let temp = tempfile::tempdir().unwrap();
        let config = config_with_allowed(vec![temp.path().display().to_string()]);
        let request = test_intent("unknown_method", "exec", "echo hi", temp.path());

        let response = exchange_with_handle_stream(&request, config);

        assert_eq!(response["error"]["code"], "BAD_REQUEST");
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unsupported method"));
    }

    #[test]
    fn handle_stream_maps_policy_denial_to_json_error() {
        let temp = tempfile::tempdir().unwrap();
        let config = config_with_allowed(vec![temp.path().display().to_string()]);
        let missing = temp.path().join("missing");
        let request = test_intent("submit_intent", "exec", "echo hi", &missing);

        let response = exchange_with_handle_stream(&request, config);

        assert_eq!(response["error"]["code"], "POLICY_DENY");
        assert_eq!(response["error"]["rule_id"], "fs.invalid_workdir");
        assert!(response["error"]["audit_id"]
            .as_str()
            .unwrap()
            .starts_with("audit-"));
    }

    #[test]
    fn handle_stream_maps_execution_setup_failures_to_json_error() {
        let temp = tempfile::tempdir().unwrap();
        let workdir = temp.path().join("workspace");
        std::fs::create_dir_all(&workdir).unwrap();
        let mut config = config_with_allowed(vec![workdir.display().to_string()]);
        config.denied_command_patterns = Vec::new();
        config.denied_path_prefixes = Vec::new();
        config.workspace_host_path = temp.path().join("missing-root").display().to_string();
        let request = test_intent("submit_intent", "exec", "echo hi", &workdir);

        let response = exchange_with_handle_stream(&request, config);

        assert_eq!(response["error"]["code"], "EXECUTION_FAILED");
        assert_eq!(response["error"]["detail"]["mode"], "docker");
        assert!(response["error"]["audit_id"]
            .as_str()
            .unwrap()
            .starts_with("audit-"));
    }

    #[test]
    fn handle_intent_rejects_unsupported_tool_name() {
        let temp = tempfile::tempdir().unwrap();
        let config = config_with_allowed(vec![temp.path().display().to_string()]);
        let request = test_intent("submit_intent", "read_file", "echo hi", temp.path());

        let err = handle_intent(&request, &config).unwrap_err();

        match err {
            PepError::BadRequest(message) => {
                assert!(message.contains("unsupported tool_name: read_file"));
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn sandbox_cleanup_deadline_survives_a_stalled_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("docker");
        fs::write(&runtime, "#!/bin/sh\nsleep 5\n").unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let started = Instant::now();
        let error = remove_sandbox_with_runtime(
            runtime.to_str().unwrap(),
            "test-sandbox",
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.contains("cleanup timed out"));
        assert!(error.contains("unconfirmed"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn sandbox_cleanup_distinguishes_absent_container_from_runtime_failure() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("docker");
        fs::write(
            &runtime,
            "#!/bin/sh\necho 'No such container: test' >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        remove_sandbox_with_runtime(runtime.to_str().unwrap(), "test", Duration::from_secs(1))
            .unwrap();
        fs::write(
            &runtime,
            "#!/bin/sh\necho 'Cannot connect to daemon' >&2\nexit 1\n",
        )
        .unwrap();
        let error =
            remove_sandbox_with_runtime(runtime.to_str().unwrap(), "test", Duration::from_secs(1))
                .unwrap_err();
        assert!(error.contains("cannot connect to daemon"));
    }

    #[test]
    fn spawn_capped_reader_reports_truncation() {
        let reader = std::io::Cursor::new(b"abcdef".to_vec());

        let (buf, truncated) = spawn_capped_reader(reader, 3).join().unwrap();

        assert_eq!(buf, b"abc");
        assert!(truncated);
    }

    #[test]
    fn run_command_and_capture_collects_stdout_stderr_and_exit_code() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("printf out; printf err >&2; exit 7")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = run_command_and_capture(&mut command, 1024, 1024, 5).unwrap();

        assert_eq!(output.stdout, "out");
        assert_eq!(output.stderr, "err");
        assert_eq!(output.exit_code, 7);
    }

    #[test]
    fn run_command_and_capture_marks_truncated_streams() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("printf 0123456789; printf abcdef >&2")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = run_command_and_capture(&mut command, 4, 3, 5).unwrap();

        assert_eq!(output.stdout, "0123\n[truncated by cai-pep]");
        assert_eq!(output.stderr, "abc\n[truncated by cai-pep]");
        assert_eq!(output.exit_code, 0);
    }

    #[test]
    fn run_command_and_capture_marks_timeouts() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("sleep 1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = run_command_and_capture(&mut command, 1024, 1024, 0).unwrap();

        assert!(output.stderr.contains("[timed out by cai-pep]"));
        assert_ne!(output.exit_code, 0);
    }

    #[test]
    fn intent_response_ok_has_no_error() {
        let resp = IntentResponse::ok(
            "req-1".to_string(),
            IntentSuccessResult {
                status: "ok".to_string(),
                decision: "allow".to_string(),
                backend: "docker".to_string(),
                sandbox_profile: "default".to_string(),
                stdout: "output".to_string(),
                stderr: String::new(),
                exit_code: 0,
                duration_ms: 100,
                audit_id: "audit-1".to_string(),
            },
        );
        assert!(resp.error.is_none());
        assert!(resp.result.is_some());
    }

    #[test]
    fn intent_response_deny_has_policy_code() {
        let resp = IntentResponse::deny(
            "req-1".to_string(),
            "fs.workdir_prefix".to_string(),
            "denied".to_string(),
            json!({}),
            "audit-1".to_string(),
        );
        assert!(resp.result.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, "POLICY_DENY");
        assert_eq!(err.rule_id.unwrap(), "fs.workdir_prefix");
    }

    #[test]
    fn intent_response_bad_request_has_no_rule_id() {
        let resp = IntentResponse::bad_request("req-1".to_string(), "bad".to_string());
        let err = resp.error.unwrap();
        assert_eq!(err.code, "BAD_REQUEST");
        assert!(err.rule_id.is_none());
        assert!(err.audit_id.is_none());
    }

    #[test]
    fn intent_response_exec_failed_has_audit_id() {
        let resp = IntentResponse::exec_failed(
            "req-1".to_string(),
            "boom".to_string(),
            json!({"mode": "docker"}),
            "audit-123".to_string(),
        );
        let err = resp.error.unwrap();
        assert_eq!(err.code, "EXECUTION_FAILED");
        assert_eq!(err.audit_id.unwrap(), "audit-123");
    }

    #[test]
    fn try_parse_attestation_bare_cai_pep() {
        let req = try_parse_attestation_shell_command(
            "cai-pep attest collect-and-verify --claims",
            "audit-1",
        )
        .unwrap()
        .unwrap();
        assert!(req.claims);
        assert_eq!(req.tee, "tdx");
    }

    #[test]
    fn try_parse_attestation_returns_none_for_non_matching_binary_name() {
        let result =
            try_parse_attestation_shell_command("not-cai-pep attest collect-and-verify", "a")
                .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn parse_attestation_args_rejects_unknown_arg_after_subcommand() {
        let err = parse_attestation_args(&[
            "collect-and-verify".to_string(),
            "--unknown-flag".to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("unknown attest argument"));
    }

    #[test]
    fn pep_config_load_rejects_missing_file() {
        let err = PepConfig::load(Path::new("/nonexistent/config.json")).unwrap_err();
        assert!(err.contains("failed to read config"));
    }

    #[test]
    fn pep_config_load_rejects_invalid_json() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), "not json").unwrap();
        let err = PepConfig::load(temp.path()).unwrap_err();
        assert!(err.contains("failed to parse config"));
    }

    #[test]
    fn pep_config_load_accepts_partial_config_with_defaults() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            temp.path(),
            r#"{
                "socket_path": "/tmp/pep.sock",
                "timeout_secs": 9,
                "allowed_workspace_prefixes": ["/tmp"]
            }"#,
        )
        .unwrap();

        let config = PepConfig::load(temp.path()).unwrap();

        assert_eq!(config.socket_path, "/tmp/pep.sock");
        assert_eq!(config.timeout_secs, 9);
        assert_eq!(config.allowed_workspace_prefixes, vec!["/tmp"]);
        assert_eq!(config.docker_network_mode, "none");
        assert_eq!(config.workspace_mount_target, "/workspace");
    }
}
