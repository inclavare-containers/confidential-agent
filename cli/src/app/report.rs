use super::*;

const TRUSTIFLUX_PORT: u16 = 8006;
const REPORT_SCHEMA: &str = "confidential-agent/attestation-report/v1";
const REPORT_HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const ATTESTATION_COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Serialize)]
struct AttestationReport {
    schema: String,
    generated_at: String,
    cli_version: String,
    services: Vec<ServiceReport>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    a2a_peers: Vec<A2aPeerReport>,
}

#[derive(Debug, Serialize)]
struct ServiceReport {
    service_id: String,
    phase: String,
    collect_status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    collect_errors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    build: Option<BuildReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tee_info: Option<TeeInfoReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attestation: Option<AttestationEarReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rekor: Option<RekorReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon: Option<DaemonReport>,
}

#[derive(Debug, Serialize)]
struct BuildReport {
    build_id: String,
    image_name: String,
    variant: String,
    spec_sha256: String,
    tee: String,
    reference_values_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reference_values: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rekor_meta: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct TeeInfoReport {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tee: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

#[derive(Debug, Serialize)]
struct AttestationEarReport {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ear_jwt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ear_claims: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct RekorReport {
    status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    entries: Vec<RekorEntryReport>,
}

#[derive(Debug, Serialize)]
struct RekorEntryReport {
    uuid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    log_index: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    integrated_time: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entry_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inclusion_proof: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct DaemonReport {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bootstrap_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mesh_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mesh_ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_ready: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    debug_ssh_ready: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    applied_resources: BTreeMap<String, AppliedResourceState>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    a2a_peers: BTreeMap<String, DaemonA2aPeerStatus>,
}

#[derive(Debug, Serialize)]
struct A2aPeerReport {
    alias: Option<String>,
    url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    scoped_services: Vec<String>,
    fetch_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    card: Option<A2aPeerCardReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signer_pin: Option<A2aSignerPin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    live_status: Option<DaemonA2aPeerStatus>,
}

#[derive(Debug, Serialize)]
struct A2aPeerCardReport {
    id: String,
    tee: String,
    public_ip: String,
    ports: Vec<AgentCardPort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reference_values: Option<serde_json::Value>,
    rekor: AgentCardRekor,
    signed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature_verified: Option<bool>,
}

pub(super) fn cmd_report(cli: &Cli, args: &ReportArgs) -> Result<()> {
    if args.include_a2a
        && A2aStateFile::from_path(&a2a_state_path(&cli.state_dir))?
            .peers
            .iter()
            .any(|peer| peer.signer.is_some())
    {
        prepare_sigstore_tools_for_process(cli)?;
    }
    let mut states = read_service_states(&cli.state_dir)?;
    if let Some(service) = args.service.as_deref() {
        states.retain(|s| s.service_id == service);
        if states.is_empty() {
            bail!("no local state for service '{}'", service);
        }
    }

    let verifier = AttestationVerifier::new(cli)?;
    let services: Vec<ServiceReport> = states
        .iter()
        .map(|state| collect_service_report(&cli.state_dir, state, &verifier))
        .collect();

    let a2a_peers = if args.include_a2a {
        collect_a2a_reports(&cli.state_dir, &states)?
    } else {
        Vec::new()
    };

    let report = AttestationReport {
        schema: REPORT_SCHEMA.to_string(),
        generated_at: current_utc_timestamp(),
        cli_version: env!("CARGO_PKG_VERSION").to_string(),
        services,
        a2a_peers,
    };

    if let Some(out) = args.out.as_ref() {
        let content = serde_json::to_string_pretty(&report)?;
        write_report_file(out, &content)?;
        println!("[ca] report written to {}", out.display());
        return Ok(());
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report_human(&report);
    }
    Ok(())
}

fn collect_service_report(
    state_dir: &Path,
    state: &LocalServiceState,
    verifier: &AttestationVerifier,
) -> ServiceReport {
    if state.phase != "active" {
        return ServiceReport {
            service_id: state.service_id.clone(),
            phase: state.phase.clone(),
            collect_status: "skipped".to_string(),
            collect_errors: vec![format!("service is {}", state.phase)],
            build: None,
            tee_info: None,
            attestation: None,
            rekor: None,
            daemon: None,
        };
    }

    let mut errors = Vec::new();

    let build = collect_build_report(state_dir, state, &mut errors);

    let public_ip = state
        .deploy
        .public_ip
        .as_deref()
        .filter(|ip| !ip.trim().is_empty());

    let tee_info = Some(tee_info_from_state(state));

    let attestation = match public_ip {
        Some(ip) => match fetch_ear_token(verifier, ip, &state.deploy.tee) {
            Ok(ear) => Some(ear),
            Err(err) => {
                errors.push(format!("attestation: {err:#}"));
                Some(AttestationEarReport {
                    status: "error".to_string(),
                    ear_jwt: None,
                    ear_claims: None,
                })
            }
        },
        None => None,
    };

    let daemon = match public_ip {
        Some(ip) => {
            match fetch_daemon_status_from(ip, DAEMON_STATUS_PORT, Duration::from_secs(5)) {
                Ok(status) => Some(daemon_report_from_status(&status)),
                Err(err) => {
                    errors.push(format!("daemon: {err:#}"));
                    Some(DaemonReport {
                        status: "error".to_string(),
                        phase: None,
                        bootstrap_generation: None,
                        mesh_generation: None,
                        mesh_ready: None,
                        app_ready: None,
                        debug_ssh_ready: None,
                        applied_resources: BTreeMap::new(),
                        a2a_peers: BTreeMap::new(),
                    })
                }
            }
        }
        None => None,
    };

    let rekor = if state.reference_values == "rekor" {
        Some(collect_rekor_report(state_dir, state, &mut errors))
    } else {
        Some(RekorReport {
            status: "not_applicable".to_string(),
            entries: Vec::new(),
        })
    };

    let collect_status = if errors.is_empty() {
        "ok".to_string()
    } else {
        "partial".to_string()
    };

    ServiceReport {
        service_id: state.service_id.clone(),
        phase: state.phase.clone(),
        collect_status,
        collect_errors: errors,
        build: Some(build),
        tee_info,
        attestation,
        rekor,
        daemon,
    }
}

fn collect_build_report(
    state_dir: &Path,
    state: &LocalServiceState,
    errors: &mut Vec<String>,
) -> BuildReport {
    let paths = context_paths(state_dir, &state.service_id);

    let reference_values = match &state.build.sample_rv {
        Some(p) => match fs::read_to_string(p) {
            Ok(c) => match serde_json::from_str(&c) {
                Ok(v) => Some(v),
                Err(err) => {
                    errors.push(format!("build: failed to parse '{}': {err}", p.display()));
                    None
                }
            },
            Err(err) => {
                errors.push(format!("build: failed to read '{}': {err}", p.display()));
                None
            }
        },
        None => None,
    };

    let rekor_meta: Option<serde_json::Value> = if state.reference_values == "rekor" {
        let meta_path = paths.service_dir.join("shelter-rekor-meta.json");
        match fs::read_to_string(&meta_path) {
            Ok(c) => match serde_json::from_str(&c) {
                Ok(v) => Some(v),
                Err(err) => {
                    errors.push(format!(
                        "build: failed to parse '{}': {err}",
                        meta_path.display()
                    ));
                    None
                }
            },
            Err(err) => {
                errors.push(format!(
                    "build: failed to read '{}': {err}",
                    meta_path.display()
                ));
                None
            }
        }
    } else {
        None
    };

    BuildReport {
        build_id: state.build.build_id.clone(),
        image_name: state.build.image_name.clone(),
        variant: state.build.variant.clone(),
        spec_sha256: state.spec.sha256.clone(),
        tee: state.deploy.tee.clone(),
        reference_values_mode: state.reference_values.clone(),
        reference_values,
        rekor_meta,
    }
}

fn report_http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(REPORT_HTTP_TIMEOUT)
        .timeout_read(REPORT_HTTP_TIMEOUT)
        .redirects(0)
        .try_proxy_from_env(false)
        .build()
}

fn tee_info_from_state(state: &LocalServiceState) -> TeeInfoReport {
    TeeInfoReport {
        status: "ok".to_string(),
        tee: Some(state.deploy.tee.to_ascii_lowercase()),
        version: None,
    }
}

fn fetch_ear_token(
    verifier: &AttestationVerifier,
    host: &str,
    tee: &str,
) -> Result<AttestationEarReport> {
    let evidence = tempfile::Builder::new()
        .prefix("report-evidence-")
        .suffix(".json")
        .tempfile_in(&verifier.workdir)
        .context("failed to create evidence file in attestation workdir")?;
    let aa_url = format!("http://{}:{}", host, TRUSTIFLUX_PORT);

    fetch_evidence_with_proxy_fallback(verifier, &aa_url, evidence.path())
        .with_context(|| format!("failed to fetch attestation evidence from {aa_url}"))?;

    let output = verifier.run_client(
        verify_tool_args(evidence.path(), tee),
        Vec::new(),
        "verify",
        ATTESTATION_COMMAND_TIMEOUT,
    )?;

    let jwt = String::from_utf8(output.stdout)
        .context("attestation verifier output was not UTF-8")?
        .trim()
        .to_string();
    if jwt.is_empty() {
        bail!("attestation verifier returned an empty EAR token");
    }

    let claims = decode_jwt_claims(&jwt);
    Ok(AttestationEarReport {
        status: "ok".to_string(),
        ear_jwt: Some(jwt),
        ear_claims: claims,
    })
}

/// Runs `attestation-challenge-client` inside the tools container so the
/// host only needs docker, matching the other host-side attestation flows.
#[derive(Debug, Clone)]
pub(super) struct AttestationVerifier {
    tools_image: String,
    workdir: PathBuf,
}

impl AttestationVerifier {
    pub(super) fn new(cli: &Cli) -> Result<Self> {
        Ok(Self {
            tools_image: cli.tools_image.clone(),
            workdir: ensure_attestation_workdir(&cli.state_dir)?,
        })
    }

    #[cfg(test)]
    pub(super) fn for_tests(tools_image: &str, workdir: PathBuf) -> Self {
        Self {
            tools_image: tools_image.to_string(),
            workdir,
        }
    }

    pub(super) fn verify_remote_attestation_claims(
        &self,
        host: &str,
        tee: &str,
    ) -> Result<Option<serde_json::Value>> {
        Ok(fetch_ear_token(self, host, tee)?.ear_claims)
    }

    fn run_client(
        &self,
        tool_args: Vec<OsString>,
        mut envs: Vec<(String, String)>,
        action: &str,
        timeout: Duration,
    ) -> Result<std::process::Output> {
        envs.push((
            "ATTESTATION_CHALLENGE_CLIENT_WORK_DIR".to_string(),
            self.workdir.to_string_lossy().to_string(),
        ));
        let output = run_tools_container_output(
            &self.tools_image,
            ToolContainerSpec {
                tool: "timeout",
                tool_args: challenge_inject_tool_args(tool_args, timeout.as_secs()),
                mounts: vec![self.workdir.clone()],
                envs,
                workdir: None,
                container_name: None,
            },
        )
        .with_context(|| format!("failed to run attestation-challenge-client {action}"))?;
        interpret_attestation_output(output, action, timeout)
    }
}

fn get_evidence_tool_args(aa_url: &str, output: &Path) -> Vec<OsString> {
    vec![
        OsString::from("get-evidence"),
        OsString::from("--aa-url"),
        OsString::from(aa_url),
        OsString::from("--output"),
        output.as_os_str().to_os_string(),
    ]
}

fn verify_tool_args(evidence: &Path, tee: &str) -> Vec<OsString> {
    vec![
        OsString::from("verify"),
        OsString::from("--evidence"),
        evidence.as_os_str().to_os_string(),
        OsString::from("--tee"),
        OsString::from(tee.to_ascii_lowercase()),
        OsString::from("--policy"),
        OsString::from("default"),
    ]
}

fn fetch_evidence_with_proxy_fallback(
    verifier: &AttestationVerifier,
    aa_url: &str,
    output: &Path,
) -> Result<()> {
    let proxy_configured = outbound_proxy_configured();
    // The container starts with a clean environment, so the first attempt is
    // always direct.  Keep it short when a proxy is configured because the
    // guest may only be reachable through it.
    let direct_timeout = if proxy_configured {
        Duration::from_secs(3)
    } else {
        ATTESTATION_COMMAND_TIMEOUT
    };
    let direct_result = verifier.run_client(
        get_evidence_tool_args(aa_url, output),
        Vec::new(),
        "get-evidence",
        direct_timeout,
    );
    if direct_result.is_ok() {
        return Ok(());
    }
    if !proxy_configured {
        return direct_result.map(|_| ());
    }

    verifier
        .run_client(
            get_evidence_tool_args(aa_url, output),
            inherited_proxy_envs(None),
            "get-evidence through proxy",
            ATTESTATION_COMMAND_TIMEOUT,
        )
        .map(|_| ())
        .with_context(|| {
            format!(
                "direct get-evidence attempt failed first: {:#}",
                direct_result.unwrap_err()
            )
        })
}

const TIMEOUT_COMMAND_EXIT_CODE: i32 = 124;

fn interpret_attestation_output(
    output: std::process::Output,
    action: &str,
    timeout: Duration,
) -> Result<std::process::Output> {
    if output.status.code() == Some(TIMEOUT_COMMAND_EXIT_CODE) {
        bail!(
            "attestation-challenge-client {action} timed out after {}s",
            timeout.as_secs()
        );
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "attestation-challenge-client {action} failed with status {}: stderr={} stdout={}",
            output.status,
            stderr.trim(),
            stdout.trim()
        );
    }
    Ok(output)
}

fn decode_jwt_claims(jwt: &str) -> Option<serde_json::Value> {
    let payload = jwt.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .or_else(|_| BASE64_STANDARD.decode(payload))
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

fn daemon_report_from_status(status: &DaemonStatus) -> DaemonReport {
    DaemonReport {
        status: "ok".to_string(),
        phase: Some(status.phase.clone()),
        bootstrap_generation: Some(status.bootstrap_generation),
        mesh_generation: Some(status.mesh_generation),
        mesh_ready: Some(status.mesh_ready),
        app_ready: Some(status.app_ready),
        debug_ssh_ready: Some(status.debug_ssh_ready),
        applied_resources: status.applied_resources.clone(),
        a2a_peers: status.a2a_peers.clone(),
    }
}

fn collect_rekor_report(
    state_dir: &Path,
    state: &LocalServiceState,
    errors: &mut Vec<String>,
) -> RekorReport {
    let paths = context_paths(state_dir, &state.service_id);
    let Some(meta_path) = state.build.rekor_meta.as_ref() else {
        errors.push("rekor: missing Rekor metadata path in local state".to_string());
        return RekorReport {
            status: "error".to_string(),
            entries: Vec::new(),
        };
    };
    let meta_content = match fs::read_to_string(meta_path) {
        Ok(c) => c,
        Err(err) => {
            errors.push(format!(
                "rekor: failed to read '{}': {err}",
                meta_path.display()
            ));
            return RekorReport {
                status: "error".to_string(),
                entries: Vec::new(),
            };
        }
    };
    let meta: serde_json::Value = match serde_json::from_str(&meta_content) {
        Ok(v) => v,
        Err(err) => {
            errors.push(format!(
                "rekor: failed to parse '{}': {err}",
                meta_path.display()
            ));
            return RekorReport {
                status: "error".to_string(),
                entries: Vec::new(),
            };
        }
    };

    let rekor_url = meta
        .get("rekor_url")
        .and_then(|v| v.as_str())
        .unwrap_or("https://rekor.sigstore.dev");

    let mut entries = Vec::new();
    let roots = rekor_artifact_roots(&paths, state);
    let local_uuids = local_rekor_entry_uuids(&roots);
    if !local_uuids.is_empty() {
        entries = fetch_rekor_entries_by_uuids(rekor_url, &local_uuids, errors);
    }
    if entries.is_empty() {
        let hashes = local_rekor_search_hashes(&roots);
        if hashes.is_empty() && local_uuids.is_empty() {
            errors.push(format!(
                "rekor: no local Rekor upload record or searchable SLSA hash found under {}",
                roots
                    .iter()
                    .map(|root| format!("'{}'", root.display()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        } else {
            entries = fetch_rekor_entries_by_hashes(rekor_url, &hashes, errors);
        }
    }

    let status = if entries.is_empty() {
        "not_found"
    } else {
        "found"
    };

    RekorReport {
        status: status.to_string(),
        entries,
    }
}

fn fetch_rekor_entries_by_hashes(
    rekor_url: &str,
    hashes: &[String],
    errors: &mut Vec<String>,
) -> Vec<RekorEntryReport> {
    let mut uuids = BTreeSet::new();
    for hash in hashes.iter().take(10) {
        for uuid in search_rekor_uuids_by_hash(rekor_url, hash, errors) {
            uuids.insert(uuid);
        }
    }
    fetch_rekor_entries_by_uuids(rekor_url, &uuids.into_iter().collect::<Vec<_>>(), errors)
}

fn search_rekor_uuids_by_hash(
    rekor_url: &str,
    hash: &str,
    errors: &mut Vec<String>,
) -> Vec<String> {
    let search_url = format!("{}/api/v1/index/retrieve", rekor_url.trim_end_matches('/'));
    let agent = report_http_agent();

    let search_body = serde_json::json!({ "hash": hash });
    match agent
        .post(&search_url)
        .set("Content-Type", "application/json")
        .send_string(&search_body.to_string())
    {
        Ok(response) => match response.into_string() {
            Ok(s) => match serde_json::from_str(&s) {
                Ok(v) => v,
                Err(err) => {
                    errors.push(format!("rekor: failed to parse search response: {err}"));
                    Vec::new()
                }
            },
            Err(err) => {
                errors.push(format!("rekor: failed to read search response: {err}"));
                Vec::new()
            }
        },
        Err(err) => {
            errors.push(format!(
                "rekor: hash search request failed for {hash}: {err}"
            ));
            Vec::new()
        }
    }
}

fn fetch_rekor_entries_by_uuids(
    rekor_url: &str,
    uuids: &[String],
    errors: &mut Vec<String>,
) -> Vec<RekorEntryReport> {
    let mut entries = Vec::new();
    for uuid in uuids.iter().take(10) {
        match fetch_single_rekor_entry(rekor_url, uuid) {
            Ok(entry) => entries.push(entry),
            Err(err) => errors.push(format!("rekor: failed to fetch entry {uuid}: {err:#}")),
        }
    }
    entries
}

fn rekor_artifact_roots(paths: &ContextPaths, state: &LocalServiceState) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(parent) = state.build.image_path.parent() {
        roots.push(parent.to_path_buf());
    }
    roots.push(paths.artifacts_dir.join(&state.build.build_id));
    roots.push(
        paths
            .shelter_work_dir
            .join("images")
            .join(&state.build.build_id),
    );
    roots.sort();
    roots.dedup();
    roots
}

fn local_rekor_entry_uuids(roots: &[PathBuf]) -> Vec<String> {
    let mut uuids = BTreeSet::new();
    for root in roots {
        for path in find_files_named(root, "rekor-v1-upload.txt", 6) {
            if let Ok(content) = fs::read_to_string(&path) {
                uuids.extend(extract_rekor_entry_uuids(&content));
            }
        }
    }
    uuids.into_iter().collect()
}

pub(super) fn local_rekor_entry_uuid_for_state(
    state_dir: &Path,
    state: &LocalServiceState,
) -> Option<String> {
    let paths = context_paths(state_dir, &state.service_id);
    local_rekor_entry_uuids(&rekor_artifact_roots(&paths, state))
        .into_iter()
        .next()
}

fn extract_rekor_entry_uuids(content: &str) -> Vec<String> {
    let marker = "/api/v1/log/entries/";
    let mut rest = content;
    let mut uuids = BTreeSet::new();
    while let Some(idx) = rest.find(marker) {
        let tail = &rest[idx + marker.len()..];
        let uuid = tail
            .chars()
            .take_while(|ch| ch.is_ascii_hexdigit())
            .collect::<String>();
        if !uuid.is_empty() {
            uuids.insert(uuid.clone());
        }
        rest = &tail[uuid.len()..];
    }
    uuids.into_iter().collect()
}

fn local_rekor_search_hashes(roots: &[PathBuf]) -> Vec<String> {
    let mut hashes = BTreeSet::new();
    for name in [
        "statement.json",
        "statement.attestation.json",
        "statement.dsse.json",
    ] {
        for root in roots {
            for path in find_files_named(root, name, 6) {
                let Ok(content) = fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
                    continue;
                };
                collect_rekor_hashes_from_json(&value, &mut hashes);
            }
        }
    }
    hashes.into_iter().collect()
}

fn collect_rekor_hashes_from_json(value: &serde_json::Value, hashes: &mut BTreeSet<String>) {
    collect_statement_subject_hashes(value, hashes);
    if let Some(payload) = value.get("payload").and_then(|payload| payload.as_str()) {
        if let Ok(decoded) = BASE64_STANDARD
            .decode(payload)
            .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload))
        {
            if let Ok(statement) = serde_json::from_slice::<serde_json::Value>(&decoded) {
                collect_statement_subject_hashes(&statement, hashes);
            }
        }
    }
}

fn collect_statement_subject_hashes(value: &serde_json::Value, hashes: &mut BTreeSet<String>) {
    let Some(subjects) = value.get("subject").and_then(|subject| subject.as_array()) else {
        return;
    };
    for subject in subjects {
        let Some(digests) = subject.get("digest").and_then(|digest| digest.as_object()) else {
            continue;
        };
        for (algorithm, digest) in digests {
            let algorithm = algorithm.to_ascii_lowercase();
            let Some(digest) = digest.as_str() else {
                continue;
            };
            if rekor_search_hash_supported(&algorithm, digest) {
                hashes.insert(format!("{algorithm}:{digest}"));
            }
        }
    }
}

fn rekor_search_hash_supported(algorithm: &str, digest: &str) -> bool {
    let expected_len = match algorithm {
        "sha1" => 40,
        "sha256" => 64,
        "sha512" => 128,
        _ => return false,
    };
    digest.len() == expected_len && digest.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn find_files_named(root: &Path, file_name: &str, max_depth: usize) -> Vec<PathBuf> {
    fn visit(dir: &Path, file_name: &str, depth: usize, out: &mut Vec<PathBuf>) {
        if depth == 0 {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_file() {
                if path.file_name().and_then(|name| name.to_str()) == Some(file_name) {
                    out.push(path);
                }
            } else if file_type.is_dir() {
                visit(&path, file_name, depth - 1, out);
            }
        }
    }

    let mut files = Vec::new();
    visit(root, file_name, max_depth, &mut files);
    files.sort();
    files
}

fn fetch_single_rekor_entry(rekor_url: &str, uuid: &str) -> Result<RekorEntryReport> {
    let url = format!(
        "{}/api/v1/log/entries/{}",
        rekor_url.trim_end_matches('/'),
        uuid
    );
    let agent = report_http_agent();
    let response = agent
        .get(&url)
        .call()
        .with_context(|| format!("failed to fetch Rekor entry {uuid}"))?;
    let entry_map: serde_json::Value = serde_json::from_str(
        &response
            .into_string()
            .context("failed to read Rekor entry response")?,
    )
    .context("failed to parse Rekor entry")?;

    let entry = entry_map
        .as_object()
        .and_then(|m| m.values().next())
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let log_index = entry.get("logIndex").and_then(|v| v.as_u64());
    let integrated_time = entry.get("integratedTime").and_then(|v| v.as_u64());

    let body = entry
        .get("body")
        .and_then(|v| v.as_str())
        .and_then(|b64| BASE64_STANDARD.decode(b64).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());

    let entry_kind = body
        .as_ref()
        .and_then(|b| b.get("kind"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            body.as_ref()
                .and_then(|b| b.get("apiVersion"))
                .and_then(|v| v.as_str())
        })
        .map(String::from);

    let inclusion_proof = entry
        .get("verification")
        .and_then(|v| v.get("inclusionProof"))
        .cloned();

    Ok(RekorEntryReport {
        uuid: uuid.to_string(),
        log_index,
        integrated_time,
        entry_kind,
        body,
        inclusion_proof,
    })
}

fn collect_a2a_reports(
    state_dir: &Path,
    services: &[LocalServiceState],
) -> Result<Vec<A2aPeerReport>> {
    let a2a_state = A2aStateFile::from_path(&a2a_state_path(state_dir))?;
    if a2a_state.peers.is_empty() {
        return Ok(Vec::new());
    }

    let daemon_a2a_peers = collect_daemon_a2a_peers(services);

    let mut reports = Vec::new();
    for peer in &a2a_state.peers {
        let report = collect_single_a2a_report(peer, &daemon_a2a_peers);
        reports.push(report);
    }
    Ok(reports)
}

fn collect_daemon_a2a_peers(
    services: &[LocalServiceState],
) -> BTreeMap<String, DaemonA2aPeerStatus> {
    let mut result = BTreeMap::new();
    for state in services.iter().filter(|s| s.phase == "active") {
        let ip = match state.deploy.public_ip.as_deref() {
            Some(ip) if !ip.trim().is_empty() => ip,
            _ => continue,
        };
        if let Ok(status) = fetch_daemon_status_from(ip, DAEMON_STATUS_PORT, Duration::from_secs(5))
        {
            for (key, peer_status) in status.a2a_peers {
                result.entry(key).or_insert(peer_status);
            }
        }
    }
    result
}

fn collect_single_a2a_report(
    peer: &A2aStatePeer,
    daemon_peers: &BTreeMap<String, DaemonA2aPeerStatus>,
) -> A2aPeerReport {
    let key = peer.alias.clone().unwrap_or_else(|| peer.url.clone());

    let signer = peer
        .signer
        .as_ref()
        .map(confidential_agent_core::agent_card_signing::AgentCardSignerPin::from);

    let (fetch_status, card) = match fetch_agent_card_with_signer(&peer.url, signer.as_ref()) {
        Ok(agent_card) => {
            let signed = !agent_card.signatures.is_empty();
            match confidential_extension(&agent_card) {
                Ok(ext) => (
                    "ok".to_string(),
                    Some(A2aPeerCardReport {
                        id: ext.id.clone(),
                        tee: ext.tee.clone(),
                        public_ip: ext.public_ip.clone(),
                        ports: ext.ports.clone(),
                        reference_values: ext.reference_values.clone(),
                        rekor: ext.rekor.clone(),
                        signed,
                        signature_verified: if signed && peer.signer.is_some() {
                            Some(true)
                        } else {
                            None
                        },
                    }),
                ),
                Err(err) => (format!("card_error: {err:#}"), None),
            }
        }
        Err(err) => (format!("fetch_error: {err}"), None),
    };

    let live_status = daemon_peers.get(&key).cloned();

    A2aPeerReport {
        alias: peer.alias.clone(),
        url: peer.url.clone(),
        scoped_services: peer.scoped_services.clone(),
        fetch_status,
        card,
        signer_pin: peer.signer.clone(),
        live_status,
    }
}

fn write_report_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create '{}'", parent.display()))?;
    }
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("failed to open '{}'", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to set permissions on '{}'", path.display()))?;
    file.write_all(content.as_bytes())
        .with_context(|| format!("failed to write '{}'", path.display()))?;
    Ok(())
}

fn print_report_human(report: &AttestationReport) {
    println!("Confidential Agent Attestation Report");
    println!("Generated: {}", report.generated_at);
    println!("CLI Version: {}", report.cli_version);
    println!();

    if report.services.is_empty() {
        println!("no services");
        return;
    }

    println!(
        "{:<18} {:<9} {:<6} {:<10} {:<10} {:<10} {:<10} {:<12}",
        "SERVICE", "PHASE", "TEE", "HARDWARE", "EXECUTBL", "CONFIG", "FILESYS", "REKOR"
    );
    for svc in &report.services {
        let tee = svc
            .tee_info
            .as_ref()
            .and_then(|t| t.tee.as_deref())
            .unwrap_or("-");
        let tv = svc
            .attestation
            .as_ref()
            .and_then(|a| a.ear_claims.as_ref())
            .and_then(|c| c.get("submods"))
            .and_then(|s| s.get("cpu0"))
            .and_then(|c| c.get("ear.trustworthiness-vector"));
        let hw = tv
            .and_then(|v| v.get("hardware"))
            .and_then(|v| v.as_i64())
            .map(|v| format!("{v}"))
            .unwrap_or_else(|| "-".to_string());
        let ex = tv
            .and_then(|v| v.get("executables"))
            .and_then(|v| v.as_i64())
            .map(|v| format!("{v}"))
            .unwrap_or_else(|| "-".to_string());
        let cf = tv
            .and_then(|v| v.get("configuration"))
            .and_then(|v| v.as_i64())
            .map(|v| format!("{v}"))
            .unwrap_or_else(|| "-".to_string());
        let fs_val = tv
            .and_then(|v| v.get("file-system"))
            .and_then(|v| v.as_i64())
            .map(|v| format!("{v}"))
            .unwrap_or_else(|| "-".to_string());
        let rekor = svc.rekor.as_ref().map(|r| r.status.as_str()).unwrap_or("-");
        println!(
            "{:<18} {:<9} {:<6} {:<10} {:<10} {:<10} {:<10} {:<12}",
            svc.service_id, svc.phase, tee, hw, ex, cf, fs_val, rekor
        );
    }

    let error_services: Vec<_> = report
        .services
        .iter()
        .filter(|s| !s.collect_errors.is_empty())
        .collect();
    if !error_services.is_empty() {
        println!();
        println!("Collection Errors");
        for svc in error_services {
            for err in &svc.collect_errors {
                println!("  {}: {}", svc.service_id, err);
            }
        }
    }

    if !report.a2a_peers.is_empty() {
        println!();
        println!("A2A Peers");
        println!(
            "{:<20} {:<6} {:<16} {:<9} {:<12}",
            "ALIAS", "TEE", "IP", "SIGNED", "STATUS"
        );
        for peer in &report.a2a_peers {
            let alias = peer.alias.as_deref().unwrap_or("-");
            let tee = peer.card.as_ref().map(|c| c.tee.as_str()).unwrap_or("-");
            let ip = peer
                .card
                .as_ref()
                .map(|c| c.public_ip.as_str())
                .unwrap_or("-");
            let signed = peer
                .card
                .as_ref()
                .map(|c| if c.signed { "yes" } else { "no" })
                .unwrap_or("-");
            let status = peer
                .live_status
                .as_ref()
                .map(|s| s.state.as_str())
                .unwrap_or("-");
            println!(
                "{:<20} {:<6} {:<16} {:<9} {:<12}",
                alias, tee, ip, signed, status
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_jwt_claims_valid() {
        let payload = r#"{"sub":"test","iss":"trustee"}"#;
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
        let jwt = format!("eyJhbGciOiJSUzM4NCJ9.{encoded}.signature");
        let claims = decode_jwt_claims(&jwt).unwrap();
        assert_eq!(claims["sub"], "test");
        assert_eq!(claims["iss"], "trustee");
    }

    #[test]
    fn decode_jwt_claims_invalid() {
        assert!(decode_jwt_claims("not-a-jwt").is_none());
        assert!(decode_jwt_claims("").is_none());
    }

    #[test]
    fn decode_jwt_claims_padded() {
        let payload = r#"{"x":1}"#;
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
        let jwt = format!("header.{encoded}.sig");
        assert!(decode_jwt_claims(&jwt).is_some());
    }

    #[test]
    fn interpret_attestation_output_maps_timeout_exit_code() {
        use std::os::unix::process::ExitStatusExt;
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(TIMEOUT_COMMAND_EXIT_CODE << 8),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        let error = interpret_attestation_output(output, "get-evidence", Duration::from_secs(20))
            .unwrap_err();
        assert!(error.to_string().contains("timed out after 20s"));
    }

    #[test]
    fn interpret_attestation_output_reports_failure_streams() {
        use std::os::unix::process::ExitStatusExt;
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(7 << 8),
            stdout: b"partial".to_vec(),
            stderr: b"boom".to_vec(),
        };
        let error = interpret_attestation_output(output, "verify", Duration::from_secs(20))
            .unwrap_err()
            .to_string();
        assert!(error.contains("verify failed"));
        assert!(error.contains("boom"));
        assert!(error.contains("partial"));
    }

    #[test]
    fn interpret_attestation_output_passes_success_through() {
        use std::os::unix::process::ExitStatusExt;
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: b"jwt".to_vec(),
            stderr: Vec::new(),
        };
        let output =
            interpret_attestation_output(output, "verify", Duration::from_secs(20)).unwrap();
        assert_eq!(output.stdout, b"jwt");
    }

    #[test]
    fn verify_tool_args_lowercase_tee_and_default_policy() {
        let args = verify_tool_args(Path::new("/work/evidence.json"), "TDX");
        let strs: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            strs,
            vec![
                "verify",
                "--evidence",
                "/work/evidence.json",
                "--tee",
                "tdx",
                "--policy",
                "default"
            ]
        );
    }

    #[test]
    fn get_evidence_tool_args_target_aa_url_and_output() {
        let args = get_evidence_tool_args("http://10.0.0.1:8006", Path::new("/work/e.json"));
        let strs: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            strs,
            vec![
                "get-evidence",
                "--aa-url",
                "http://10.0.0.1:8006",
                "--output",
                "/work/e.json"
            ]
        );
    }

    #[test]
    fn service_report_skips_non_active() {
        let state = LocalServiceState {
            schema: LOCAL_SERVICE_STATE_SCHEMA_VERSION.to_string(),
            service_id: "test".to_string(),
            generation: 1,
            phase: "deleted".to_string(),
            attestation_mode: "challenge".to_string(),
            spec: LocalSpecState {
                path: PathBuf::from("/spec.yaml"),
                sha256: "abc".to_string(),
            },
            build: LocalBuildState {
                build_id: "b1".to_string(),
                image_name: "test".to_string(),
                variant: "release".to_string(),
                image_path: PathBuf::from("/img"),
                images_dir: PathBuf::from("/imgs"),
                cache_dir: PathBuf::from("/cache"),
                debug_ssh: None,
                sample_rv: None,
                rekor_meta: None,
                remote: false,
                published: BTreeMap::new(),
            },
            deploy: LocalDeployState {
                provider: "aliyun".to_string(),
                run_id: String::new(),
                resource_name: String::new(),
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
                ports: vec![],
                connect: vec![],
                mcp_ports: Vec::new(),
            },
            gateway_identity: None,
            resources: BTreeMap::new(),
            mesh_generation: 0,
            reference_values: "sample".to_string(),
        };
        let report = collect_service_report(
            Path::new("/state"),
            &state,
            &AttestationVerifier::for_tests("confidential-agent-tools:test", PathBuf::from("/tmp")),
        );
        assert_eq!(report.collect_status, "skipped");
        assert!(report.build.is_none());
        assert!(report.attestation.is_none());
    }

    #[test]
    fn daemon_report_from_status_maps_fields() {
        let status = DaemonStatus {
            schema: "confidential-agent/daemon-status/v1".to_string(),
            service_id: "test".to_string(),
            phase: "running".to_string(),
            bootstrap_generation: 3,
            mesh_generation: 2,
            applied_resources: BTreeMap::new(),
            mesh_fingerprint: Some("abc".to_string()),
            app_ready: true,
            mesh_ready: true,
            debug_ssh_ready: false,
            a2a_peers: BTreeMap::new(),
            last_error: None,
        };
        let report = daemon_report_from_status(&status);
        assert_eq!(report.status, "ok");
        assert_eq!(report.phase.as_deref(), Some("running"));
        assert_eq!(report.mesh_generation, Some(2));
    }

    #[test]
    fn extracts_rekor_entry_uuids_from_upload_log() {
        let log = "Created entry at index 1, available at: https://rekor.sigstore.dev/api/v1/log/entries/abc123\n\
                   duplicate https://rekor.sigstore.dev/api/v1/log/entries/abc123\n\
                   second https://rekor.sigstore.dev/api/v1/log/entries/deadbeef.";

        let uuids = extract_rekor_entry_uuids(log);

        assert_eq!(uuids, vec!["abc123".to_string(), "deadbeef".to_string()]);
    }

    #[test]
    fn collect_rekor_hashes_accepts_statement_and_dsse_payload() {
        let statement = serde_json::json!({
            "subject": [
                {
                    "name": "artifact-index-hash",
                    "digest": {
                        "sha256": "ac6777717772600ceb3b69ea0c8932d050a8e711785bd5f497c2fc0755179ad3",
                        "sha384": "35c46f28cf58877b58e515f4d121ca82cdc4297cd288df9c1057bc9885a261d10aca40de29e8bf84303c0d3b6011724c"
                    }
                }
            ]
        });
        let payload = BASE64_STANDARD.encode(serde_json::to_vec(&statement).unwrap());
        let dsse = serde_json::json!({ "payload": payload });
        let mut hashes = BTreeSet::new();

        collect_rekor_hashes_from_json(&statement, &mut hashes);
        collect_rekor_hashes_from_json(&dsse, &mut hashes);

        assert_eq!(
            hashes.into_iter().collect::<Vec<_>>(),
            vec!["sha256:ac6777717772600ceb3b69ea0c8932d050a8e711785bd5f497c2fc0755179ad3"]
        );
    }
}
