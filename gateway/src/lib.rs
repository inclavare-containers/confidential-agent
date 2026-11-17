use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex, MutexGuard,
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const GATEWAY_CONFIG_SCHEMA: &str = "confidential-agent/gateway-config/v1";
pub const AUDIT_SCHEMA: &str = "confidential-agent/gateway-audit/v1";
const FRAME_MAGIC: &[u8; 4] = b"CAIG";
const FRAME_VERSION: u8 = 1;
const MAX_TOKEN_BYTES: usize = 8192;
const DEFAULT_TOKEN_TTL_SEC: u64 = 60;
const DEFAULT_SO_MARK: u32 = 565;
const TOKEN_CLOCK_SKEW_SEC: u64 = 30;
const DOWNSTREAM_READ_TIMEOUT: Duration = Duration::from_secs(30);
const UPSTREAM_READ_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HTTP_HEADERS_BYTES: usize = 1024 * 1024;
const MAX_HTTP_BODY_BYTES: usize = 16 * 1024 * 1024;
const TRUSTIFLUX_AA_URL: &str = "http://127.0.0.1:8006";
static JTI_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub schema: String,
    pub service_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<GatewayIdentity>,
    #[serde(default)]
    pub mesh_generation: u64,
    #[serde(default)]
    pub token_ttl_sec: Option<u64>,
    #[serde(default)]
    pub so_mark: Option<u32>,
    #[serde(default)]
    pub trusted_services: BTreeMap<String, GatewayTrustedService>,
    #[serde(default)]
    pub client_routes: Vec<ClientRoute>,
    #[serde(default)]
    pub server_routes: Vec<ServerRoute>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayIdentity {
    pub public_key: String,
    pub private_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayTrustedService {
    pub public_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientRoute {
    pub listen_host: String,
    pub listen_port: u16,
    pub tng_host: String,
    pub tng_port: u16,
    pub target_service: String,
    pub target_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerRoute {
    pub listen_host: String,
    pub listen_port: u16,
    pub upstream_host: String,
    pub upstream_port: u16,
    #[serde(default = "raw_protocol")]
    pub protocol: String,
    #[serde(default)]
    pub audit_path: Option<PathBuf>,
}

fn raw_protocol() -> String {
    "raw".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenHeader {
    alg: String,
    typ: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServiceClaims {
    iss: String,
    sub: String,
    aud: String,
    iat: u64,
    exp: u64,
    jti: String,
    mesh_generation: u64,
}

#[derive(Debug, Clone)]
struct VerifiedCaller {
    service_id: String,
    jti: String,
}

#[derive(Debug, Default)]
struct ReplayCache {
    entries: Mutex<BTreeMap<String, u64>>,
}

impl ReplayCache {
    fn insert(&self, jti: &str, exp: u64, now: u64) -> Result<()> {
        let mut entries = match self.entries.lock() {
            Ok(entries) => entries,
            Err(_) => bail!("replay cache poisoned"),
        };
        entries.retain(|_, entry_exp| *entry_exp + TOKEN_CLOCK_SKEW_SEC >= now);
        if entries.contains_key(jti) {
            bail!("replayed gateway token jti '{jti}'");
        }
        entries.insert(jti.to_string(), exp);
        Ok(())
    }
}

pub fn run_gateway(config: GatewayConfig, shutdown: Arc<AtomicBool>) -> Result<()> {
    config.validate()?;
    let netfilter = NetfilterGuard::install(&config)?;
    let config = Arc::new(config);
    let replay_cache = Arc::new(ReplayCache::default());
    let mut handles = Vec::new();

    for route in config.client_routes.clone() {
        let cfg = config.clone();
        let shutdown = shutdown.clone();
        handles.push(thread::spawn(move || {
            if let Err(err) = run_client_route(cfg, route, shutdown) {
                eprintln!("client gateway route failed: {err:#}");
            }
        }));
    }
    for route in config.server_routes.clone() {
        let cfg = config.clone();
        let shutdown = shutdown.clone();
        let replay_cache = replay_cache.clone();
        handles.push(thread::spawn(move || {
            if let Err(err) = run_server_route(cfg, route, replay_cache, shutdown) {
                eprintln!("server gateway route failed: {err:#}");
            }
        }));
    }

    while !shutdown.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(200));
    }
    drop(netfilter);
    for handle in handles {
        let _ = handle.join();
    }
    Ok(())
}

impl GatewayConfig {
    fn validate(&self) -> Result<()> {
        if self.schema != GATEWAY_CONFIG_SCHEMA {
            bail!(
                "unsupported gateway config schema '{}'; expected '{}'",
                self.schema,
                GATEWAY_CONFIG_SCHEMA
            );
        }
        if self.service_id.trim().is_empty() {
            bail!("gateway service_id must not be empty");
        }
        if let Some(identity) = &self.identity {
            decode_key32(&identity.private_key, "identity.private_key")?;
            decode_key32(&identity.public_key, "identity.public_key")?;
        } else if !self.client_routes.is_empty() {
            bail!("gateway identity is required when client_routes are configured");
        }
        let mut listen_ports = BTreeSet::new();
        for route in &self.client_routes {
            if !listen_ports.insert(("client", route.listen_port)) {
                bail!("duplicate client gateway listen port {}", route.listen_port);
            }
            if route.listen_port == route.tng_port {
                bail!(
                    "client gateway listen port {} collides with hidden TNG port",
                    route.listen_port
                );
            }
            if route.target_service.trim().is_empty() {
                bail!("client gateway target_service must not be empty");
            }
        }
        for route in &self.server_routes {
            if !listen_ports.insert(("server", route.listen_port)) {
                bail!("duplicate server gateway listen port {}", route.listen_port);
            }
            if route.listen_port == route.upstream_port {
                bail!(
                    "server gateway listen port {} collides with upstream port",
                    route.listen_port
                );
            }
            match route.protocol.as_str() {
                "raw" | "mcp" => {}
                other => bail!("unsupported server gateway protocol '{other}'"),
            }
        }
        for (id, trusted) in &self.trusted_services {
            if id.trim().is_empty() {
                bail!("trusted service id must not be empty");
            }
            decode_key32(&trusted.public_key, "trusted_services.public_key")?;
        }
        Ok(())
    }
}

fn run_client_route(
    config: Arc<GatewayConfig>,
    route: ClientRoute,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let listener = TcpListener::bind((route.listen_host.as_str(), route.listen_port))
        .with_context(|| {
            format!(
                "failed to bind client gateway {}:{}",
                route.listen_host, route.listen_port
            )
        })?;
    listener.set_nonblocking(true)?;
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((downstream, _)) => {
                let cfg = config.clone();
                let route = route.clone();
                thread::spawn(move || {
                    if let Err(err) = handle_client_connection(&cfg, &route, downstream) {
                        eprintln!("client gateway connection failed: {err:#}");
                    }
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(err) => return Err(err).context("client gateway accept failed"),
        }
    }
    Ok(())
}

fn handle_client_connection(
    config: &GatewayConfig,
    route: &ClientRoute,
    downstream: TcpStream,
) -> Result<()> {
    let mut upstream =
        TcpStream::connect((route.tng_host.as_str(), route.tng_port)).with_context(|| {
            format!(
                "failed to connect hidden TNG {}:{}",
                route.tng_host, route.tng_port
            )
        })?;
    let token = sign_service_token(config, &route.target_service, route.target_port, now_unix())?;
    write_frame(&mut upstream, &token)?;
    proxy_bidirectional(downstream, upstream)
}

fn run_server_route(
    config: Arc<GatewayConfig>,
    route: ServerRoute,
    replay_cache: Arc<ReplayCache>,
    shutdown: Arc<AtomicBool>,
) -> Result<()> {
    let audit = if route.protocol == "mcp" {
        Some(Arc::new(Mutex::new(AuditStore::open(
            route.audit_path.clone().unwrap_or_else(|| {
                PathBuf::from(format!(
                    "/var/lib/cai-gateway/audit-{}.jsonl",
                    route.upstream_port
                ))
            }),
            &config.service_id,
        )?)))
    } else {
        None
    };
    let listener = TcpListener::bind((route.listen_host.as_str(), route.listen_port))
        .with_context(|| {
            format!(
                "failed to bind server gateway {}:{}",
                route.listen_host, route.listen_port
            )
        })?;
    listener.set_nonblocking(true)?;
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let cfg = config.clone();
                let route = route.clone();
                let replay_cache = replay_cache.clone();
                let audit = audit.clone();
                thread::spawn(move || {
                    if let Err(err) =
                        handle_server_connection(&cfg, &route, replay_cache, audit, stream)
                    {
                        eprintln!("server gateway connection failed: {err:#}");
                    }
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(err) => return Err(err).context("server gateway accept failed"),
        }
    }
    Ok(())
}

fn handle_server_connection(
    config: &GatewayConfig,
    route: &ServerRoute,
    replay_cache: Arc<ReplayCache>,
    audit: Option<Arc<Mutex<AuditStore>>>,
    mut downstream: TcpStream,
) -> Result<()> {
    downstream
        .set_read_timeout(Some(DOWNSTREAM_READ_TIMEOUT))
        .context("failed to set gateway downstream read timeout")?;
    let token = read_frame(&mut downstream)?;
    let caller = verify_service_token(
        &token,
        &config.trusted_services,
        &audience(&config.service_id, route.upstream_port),
        now_unix(),
        &replay_cache,
    )?;
    match route.protocol.as_str() {
        "raw" => {
            downstream
                .set_read_timeout(None)
                .context("failed to clear raw downstream read timeout")?;
            let upstream = TcpStream::connect((route.upstream_host.as_str(), route.upstream_port))
                .with_context(|| {
                    format!(
                        "failed to connect upstream {}:{}",
                        route.upstream_host, route.upstream_port
                    )
                })?;
            proxy_bidirectional(downstream, upstream)
        }
        "mcp" => handle_mcp_connection(
            route,
            audit.context("mcp route missing audit store")?,
            caller,
            downstream,
        ),
        other => bail!("unsupported server gateway protocol '{other}'"),
    }
}

fn proxy_bidirectional(left: TcpStream, right: TcpStream) -> Result<()> {
    let mut left_read = left.try_clone().context("failed to clone left stream")?;
    let mut left_write = left;
    let mut right_read = right.try_clone().context("failed to clone right stream")?;
    let mut right_write = right;
    let a = thread::spawn(move || {
        let _ = std::io::copy(&mut left_read, &mut right_write);
        let _ = right_write.shutdown(std::net::Shutdown::Write);
    });
    let b = thread::spawn(move || {
        let _ = std::io::copy(&mut right_read, &mut left_write);
        let _ = left_write.shutdown(std::net::Shutdown::Write);
    });
    let _ = a.join();
    let _ = b.join();
    Ok(())
}

fn sign_service_token(
    config: &GatewayConfig,
    target_service: &str,
    target_port: u16,
    now: u64,
) -> Result<String> {
    let identity = config
        .identity
        .as_ref()
        .context("gateway identity is required to sign service tokens")?;
    let private_key = decode_key32(&identity.private_key, "identity.private_key")?;
    let signing_key = SigningKey::from_bytes(&private_key);
    let ttl = config.token_ttl_sec.unwrap_or(DEFAULT_TOKEN_TTL_SEC);
    let header = TokenHeader {
        alg: "EdDSA".to_string(),
        typ: "JWT".to_string(),
    };
    let claims = ServiceClaims {
        iss: config.service_id.clone(),
        sub: config.service_id.clone(),
        aud: audience(target_service, target_port),
        iat: now,
        exp: now + ttl,
        jti: new_jti()?,
        mesh_generation: config.mesh_generation,
    };
    sign_claims(&signing_key, &header, &claims)
}

fn sign_claims(
    signing_key: &SigningKey,
    header: &TokenHeader,
    claims: &ServiceClaims,
) -> Result<String> {
    let header = b64_json(header)?;
    let payload = b64_json(claims)?;
    let message = format!("{header}.{payload}");
    let signature = signing_key.sign(message.as_bytes());
    Ok(format!(
        "{message}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    ))
}

fn verify_service_token(
    token: &str,
    trusted: &BTreeMap<String, GatewayTrustedService>,
    expected_aud: &str,
    now: u64,
    replay_cache: &ReplayCache,
) -> Result<VerifiedCaller> {
    let parts = token.split('.').collect::<Vec<_>>();
    if parts.len() != 3 {
        bail!("gateway token must have three JWT segments");
    }
    let header: TokenHeader = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0])?)
        .context("invalid gateway token header")?;
    if header.alg != "EdDSA" || header.typ != "JWT" {
        bail!("unsupported gateway token header");
    }
    let claims: ServiceClaims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1])?)
        .context("invalid gateway token claims")?;
    if claims.aud != expected_aud {
        bail!(
            "gateway token audience '{}' does not match '{}'",
            claims.aud,
            expected_aud
        );
    }
    if claims.exp < now {
        bail!("gateway token expired at {}", claims.exp);
    }
    if claims.iat > now + TOKEN_CLOCK_SKEW_SEC {
        bail!("gateway token iat {} is in the future", claims.iat);
    }
    let trusted = trusted
        .get(&claims.iss)
        .with_context(|| format!("untrusted gateway token issuer '{}'", claims.iss))?;
    let public_key = decode_key32(&trusted.public_key, "trusted public_key")?;
    let verifying_key =
        VerifyingKey::from_bytes(&public_key).context("invalid trusted gateway public key")?;
    let sig_bytes = URL_SAFE_NO_PAD.decode(parts[2])?;
    let signature = Signature::from_slice(&sig_bytes).context("invalid gateway token signature")?;
    let message = format!("{}.{}", parts[0], parts[1]);
    verifying_key
        .verify(message.as_bytes(), &signature)
        .context("gateway token signature verification failed")?;
    replay_cache.insert(&claims.jti, claims.exp, now)?;
    Ok(VerifiedCaller {
        service_id: claims.iss,
        jti: claims.jti,
    })
}

fn audience(service_id: &str, port: u16) -> String {
    format!("cai-gateway:{service_id}:{port}")
}

fn write_frame(stream: &mut TcpStream, token: &str) -> Result<()> {
    let token = token.as_bytes();
    if token.len() > MAX_TOKEN_BYTES {
        bail!("gateway token is too large");
    }
    let mut header = Vec::with_capacity(7);
    header.extend_from_slice(FRAME_MAGIC);
    header.push(FRAME_VERSION);
    header.extend_from_slice(&(token.len() as u16).to_be_bytes());
    stream.write_all(&header)?;
    stream.write_all(token)?;
    Ok(())
}

fn read_frame(stream: &mut TcpStream) -> Result<String> {
    let mut header = [0u8; 7];
    stream.read_exact(&mut header)?;
    if &header[..4] != FRAME_MAGIC {
        bail!("invalid gateway frame magic");
    }
    if header[4] != FRAME_VERSION {
        bail!("unsupported gateway frame version {}", header[4]);
    }
    let len = u16::from_be_bytes([header[5], header[6]]) as usize;
    if len == 0 || len > MAX_TOKEN_BYTES {
        bail!("invalid gateway frame token length {len}");
    }
    let mut token = vec![0u8; len];
    stream.read_exact(&mut token)?;
    String::from_utf8(token).context("gateway token is not UTF-8")
}

fn b64_json<T: Serialize>(value: &T) -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(value)?))
}

fn decode_key32(value: &str, field: &str) -> Result<[u8; 32]> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .with_context(|| format!("{field} must be base64url without padding"))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("{field} must decode to 32 bytes"))?;
    Ok(bytes)
}

fn new_jti() -> Result<String> {
    let mut random = [0u8; 32];
    File::open("/dev/urandom")
        .context("failed to open /dev/urandom")?
        .read_exact(&mut random)
        .context("failed to read /dev/urandom")?;
    let counter = JTI_COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut hasher = Sha256::new();
    hasher.update(random);
    hasher.update(counter.to_be_bytes());
    hasher.update(now_unix().to_be_bytes());
    Ok(URL_SAFE_NO_PAD.encode(hasher.finalize()))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

struct NetfilterGuard {
    cleanup: Vec<Vec<String>>,
}

impl NetfilterGuard {
    fn install(config: &GatewayConfig) -> Result<Self> {
        if std::env::var_os("CA_GATEWAY_SKIP_IPTABLES").is_some() {
            return Ok(Self {
                cleanup: Vec::new(),
            });
        }
        let chain = format!("CAI_GATEWAY_{}", sanitize_chain_suffix(&config.service_id));
        let mut guard = Self {
            cleanup: Vec::new(),
        };
        run_iptables(
            &["-t", "nat", "-D", "OUTPUT", "-p", "tcp", "-j", &chain],
            true,
        )?;
        run_iptables(&["-t", "nat", "-F", &chain], true)?;
        run_iptables(&["-t", "nat", "-X", &chain], true)?;
        run_iptables(&["-t", "nat", "-N", &chain], false)?;
        guard.cleanup.push(vec![
            "-t".into(),
            "nat".into(),
            "-D".into(),
            "OUTPUT".into(),
            "-p".into(),
            "tcp".into(),
            "-j".into(),
            chain.clone(),
        ]);
        guard
            .cleanup
            .push(vec!["-t".into(), "nat".into(), "-F".into(), chain.clone()]);
        guard
            .cleanup
            .push(vec!["-t".into(), "nat".into(), "-X".into(), chain.clone()]);
        let mark = config.so_mark.unwrap_or(DEFAULT_SO_MARK).to_string();
        for route in &config.server_routes {
            run_iptables(
                &[
                    "-t",
                    "nat",
                    "-A",
                    &chain,
                    "-p",
                    "tcp",
                    "-m",
                    "mark",
                    "--mark",
                    &mark,
                    "--dport",
                    &route.upstream_port.to_string(),
                    "-j",
                    "REDIRECT",
                    "--to-ports",
                    &route.listen_port.to_string(),
                ],
                false,
            )?;
        }
        run_iptables(
            &["-t", "nat", "-I", "OUTPUT", "1", "-p", "tcp", "-j", &chain],
            false,
        )?;
        Ok(guard)
    }
}

impl Drop for NetfilterGuard {
    fn drop(&mut self) {
        for args in &self.cleanup {
            let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
            let _ = run_iptables(&refs, true);
        }
    }
}

fn run_iptables(args: &[&str], ignore_failure: bool) -> Result<()> {
    let output = Command::new("iptables")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("failed to run iptables")?;
    if !output.status.success() && !ignore_failure {
        bail!(
            "iptables {} failed with {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn sanitize_chain_suffix(service_id: &str) -> String {
    let mut out = service_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    out.truncate(16);
    if out.is_empty() {
        "SVC".to_string()
    } else {
        out
    }
}

fn handle_mcp_connection(
    route: &ServerRoute,
    audit: Arc<Mutex<AuditStore>>,
    caller: VerifiedCaller,
    mut downstream: TcpStream,
) -> Result<()> {
    downstream
        .set_read_timeout(None)
        .context("failed to clear mcp downstream read timeout")?;
    loop {
        let request = match read_http_request(&mut downstream)? {
            Some(request) => request,
            None => return Ok(()),
        };
        let rpc = parse_json_rpc(&request.body);
        if rpc.method.is_none() {
            let response = json_rpc_error_response(
                rpc.id.clone().unwrap_or(Value::Null),
                -32600,
                "invalid MCP JSON-RPC request: missing method".to_string(),
            )?;
            downstream.write_all(&response.to_bytes())?;
            if request.connection_close {
                return Ok(());
            }
            continue;
        }
        let started = now_unix();
        let response = if rpc.method.as_deref() == Some("tools/call")
            && rpc.tool_name.as_deref().is_some_and(is_virtual_tool)
        {
            match virtual_tool_response(&rpc, &audit, route) {
                Ok(response) => response,
                Err(err) => json_rpc_error_response(
                    rpc.id.clone().unwrap_or(Value::Null),
                    -32000,
                    format!("{err:#}"),
                )?,
            }
        } else {
            match forward_http_request(route, &request) {
                Ok(upstream_response) => {
                    if rpc.method.as_deref() == Some("tools/list") {
                        inject_virtual_tools(upstream_response)?
                    } else {
                        upstream_response
                    }
                }
                Err(err) => json_rpc_error_response(
                    rpc.id.clone().unwrap_or(Value::Null),
                    -32502,
                    format!("upstream MCP server unavailable: {err:#}"),
                )?,
            }
        };
        if let Some(method) = rpc.method.clone() {
            if let Err(err) = (|| -> Result<()> {
                let mut store = lock_audit(&audit)?;
                store.append(AuditInput {
                    timestamp: started,
                    caller_service_id: caller.service_id.clone(),
                    caller_jti: caller.jti.clone(),
                    method,
                    tool_name: rpc.tool_name.clone(),
                    params_hash: rpc.params.as_ref().map(hash_json_value),
                    result_hash: response_json_result_hash(&response.body),
                    http_status: response.status,
                })
            })() {
                let error_response = json_rpc_error_response(
                    rpc.id.clone().unwrap_or(Value::Null),
                    -32001,
                    format!("audit append failed; upstream response withheld: {err:#}"),
                )?;
                downstream.write_all(&error_response.to_bytes())?;
                return Err(err).context("audit append failed after upstream MCP handling");
            }
        }
        downstream.write_all(&response.to_bytes())?;
        if request.connection_close {
            return Ok(());
        }
    }
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    connection_close: bool,
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn json(status: u16, body: Value) -> Result<Self> {
        let body = serde_json::to_vec(&body)?;
        Ok(Self {
            status,
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("content-length".to_string(), body.len().to_string()),
                ("connection".to_string(), "close".to_string()),
            ],
            body,
        })
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {} {}\r\n",
            self.status,
            reason_phrase(self.status)
        )
        .into_bytes();
        let mut has_len = false;
        for (name, value) in &self.headers {
            if name.eq_ignore_ascii_case("content-length") {
                has_len = true;
            }
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(value.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        if !has_len {
            out.extend_from_slice(format!("content-length: {}\r\n", self.body.len()).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "",
    }
}

fn read_http_request(stream: &mut TcpStream) -> Result<Option<HttpRequest>> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte)? {
            0 if head.is_empty() => return Ok(None),
            0 => bail!("HTTP request ended before headers completed"),
            _ => head.push(byte[0]),
        }
        if head.len() > MAX_HTTP_HEADERS_BYTES {
            bail!("HTTP request headers too large");
        }
    }
    let head_str = String::from_utf8(head).context("HTTP request headers are not UTF-8")?;
    let mut lines = head_str.split("\r\n");
    let request_line = lines.next().context("missing HTTP request line")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().context("missing HTTP method")?.to_string();
    let path = parts.next().context("missing HTTP path")?.to_string();
    let mut headers = Vec::new();
    let mut content_length = 0usize;
    let mut connection_close = false;
    for line in lines.filter(|line| !line.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            bail!("invalid HTTP request header '{line}'");
        };
        let name = name.trim().to_string();
        let value = value.trim().to_string();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().context("invalid content-length")?;
            if content_length > MAX_HTTP_BODY_BYTES {
                bail!("HTTP request body too large");
            }
        }
        if name.eq_ignore_ascii_case("connection") && value.eq_ignore_ascii_case("close") {
            connection_close = true;
        }
        headers.push((name, value));
    }
    let mut body = vec![0u8; content_length];
    stream.read_exact(&mut body)?;
    Ok(Some(HttpRequest {
        method,
        path,
        headers,
        body,
        connection_close,
    }))
}

fn forward_http_request(route: &ServerRoute, request: &HttpRequest) -> Result<HttpResponse> {
    forward_http_request_with_timeout(route, request, UPSTREAM_READ_TIMEOUT)
}

fn forward_http_request_with_timeout(
    route: &ServerRoute,
    request: &HttpRequest,
    read_timeout: Duration,
) -> Result<HttpResponse> {
    let upstream_addr = (route.upstream_host.as_str(), route.upstream_port)
        .to_socket_addrs()
        .context("failed to resolve upstream MCP address")?
        .next()
        .context("upstream MCP address resolved to no socket addresses")?;
    let mut upstream = TcpStream::connect_timeout(&upstream_addr, read_timeout)
        .context("failed to connect to upstream MCP server")?;
    upstream
        .set_read_timeout(Some(read_timeout))
        .context("failed to set upstream read timeout")?;
    let mut bytes = format!("{} {} HTTP/1.1\r\n", request.method, request.path).into_bytes();
    let mut has_host = false;
    for (name, value) in &request.headers {
        if is_hop_header(name) {
            continue;
        }
        if name.eq_ignore_ascii_case("host") {
            has_host = true;
        }
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(b": ");
        bytes.extend_from_slice(value.as_bytes());
        bytes.extend_from_slice(b"\r\n");
    }
    if !has_host {
        bytes.extend_from_slice(
            format!("Host: {}:{}\r\n", route.upstream_host, route.upstream_port).as_bytes(),
        );
    }
    bytes.extend_from_slice(b"Connection: close\r\n");
    bytes.extend_from_slice(format!("Content-Length: {}\r\n\r\n", request.body.len()).as_bytes());
    bytes.extend_from_slice(&request.body);
    upstream.write_all(&bytes)?;
    read_http_response(&mut upstream)
}

fn is_hop_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
    )
}

fn read_http_response(stream: &mut TcpStream) -> Result<HttpResponse> {
    let mut all = Vec::new();
    stream
        .take(MAX_HTTP_HEADERS_BYTES as u64 + MAX_HTTP_BODY_BYTES as u64 + 1)
        .read_to_end(&mut all)?;
    if all.len() > MAX_HTTP_HEADERS_BYTES + MAX_HTTP_BODY_BYTES {
        bail!("HTTP response too large");
    }
    let split = all
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("HTTP response missing header terminator")?;
    let head =
        String::from_utf8(all[..split].to_vec()).context("HTTP response headers are not UTF-8")?;
    let raw_body = all[split + 4..].to_vec();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().context("missing HTTP status line")?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .context("missing HTTP status code")?
        .parse::<u16>()
        .context("invalid HTTP status code")?;
    let mut headers = Vec::new();
    let mut transfer_encodings = Vec::new();
    let mut content_length = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            bail!("invalid HTTP response header '{line}'");
        };
        if name.eq_ignore_ascii_case("transfer-encoding") {
            transfer_encodings.push(value.trim().to_string());
        }
        if name.eq_ignore_ascii_case("content-length") {
            let parsed = value.trim().parse::<u64>().with_context(|| {
                format!("invalid HTTP response Content-Length '{}'", value.trim())
            })?;
            if let Some(existing) = content_length {
                if existing != parsed {
                    bail!("conflicting duplicate HTTP response Content-Length headers");
                }
            } else {
                content_length = Some(parsed);
            }
        }
        if !is_hop_header(name) {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let chunked = transfer_encoding_is_chunked(&transfer_encodings)?;
    if chunked && content_length.is_some() {
        bail!("HTTP response has both Transfer-Encoding: chunked and Content-Length");
    }
    let body = if chunked {
        decode_chunked_body(&raw_body)?
    } else {
        raw_body
    };
    headers.push(("connection".to_string(), "close".to_string()));
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

fn transfer_encoding_is_chunked(values: &[String]) -> Result<bool> {
    let mut has_chunked = false;
    for coding in values
        .iter()
        .flat_map(|value| value.split(','))
        .map(|coding| coding.trim().to_ascii_lowercase())
        .filter(|coding| !coding.is_empty())
    {
        if coding == "chunked" {
            has_chunked = true;
        } else {
            bail!("unsupported HTTP transfer encoding '{coding}'");
        }
    }
    Ok(has_chunked)
}

fn decode_chunked_body(body: &[u8]) -> Result<Vec<u8>> {
    let mut decoded = Vec::new();
    let mut pos = 0usize;
    loop {
        let line_end = body[pos..]
            .windows(2)
            .position(|window| window == b"\r\n")
            .map(|offset| pos + offset)
            .context("chunked HTTP response missing chunk-size terminator")?;
        let size_line = std::str::from_utf8(&body[pos..line_end])
            .context("chunked HTTP response chunk-size is not UTF-8")?;
        let size_text = size_line
            .split_once(';')
            .map(|(size, _)| size)
            .unwrap_or(size_line)
            .trim();
        let size = usize::from_str_radix(size_text, 16)
            .with_context(|| format!("invalid chunked HTTP response chunk size '{size_text}'"))?;
        pos = line_end + 2;

        if size == 0 {
            loop {
                let trailer_end = body[pos..]
                    .windows(2)
                    .position(|window| window == b"\r\n")
                    .map(|offset| pos + offset)
                    .context("chunked HTTP response missing final trailer terminator")?;
                let trailer = &body[pos..trailer_end];
                std::str::from_utf8(trailer)
                    .context("chunked HTTP response trailer is not UTF-8")?;
                pos = trailer_end + 2;
                if trailer.is_empty() {
                    if pos != body.len() {
                        bail!("chunked HTTP response has trailing data after final trailer");
                    }
                    return Ok(decoded);
                }
            }
        }

        let chunk_end = pos
            .checked_add(size)
            .context("chunked HTTP response chunk size overflow")?;
        if chunk_end > body.len().saturating_sub(2) {
            bail!("chunked HTTP response ended before chunk data completed");
        }
        if &body[chunk_end..chunk_end + 2] != b"\r\n" {
            bail!("chunked HTTP response chunk missing trailing CRLF");
        }
        if decoded
            .len()
            .checked_add(size)
            .context("chunked HTTP response decoded size overflow")?
            > MAX_HTTP_BODY_BYTES
        {
            bail!("HTTP response body too large");
        }
        decoded.extend_from_slice(&body[pos..chunk_end]);
        pos = chunk_end + 2;
    }
}

#[derive(Default)]
struct JsonRpcRequest {
    id: Option<Value>,
    method: Option<String>,
    params: Option<Value>,
    tool_name: Option<String>,
}

fn parse_json_rpc(body: &[u8]) -> JsonRpcRequest {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return JsonRpcRequest::default();
    };
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let params = value.get("params").cloned();
    let tool_name = params
        .as_ref()
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    JsonRpcRequest {
        id: value.get("id").cloned(),
        method,
        params,
        tool_name,
    }
}

fn is_virtual_tool(name: &str) -> bool {
    matches!(name, "tee_attest" | "audit_status" | "audit_verify")
}

fn virtual_tool_response(
    rpc: &JsonRpcRequest,
    audit: &Arc<Mutex<AuditStore>>,
    route: &ServerRoute,
) -> Result<HttpResponse> {
    let id = rpc.id.clone().unwrap_or(Value::Null);
    let tool_name = rpc
        .tool_name
        .as_deref()
        .context("missing virtual tool name")?;
    let result = match tool_name {
        "audit_status" => {
            let store = lock_audit(audit)?;
            serde_json::to_value(store.status())?
        }
        "audit_verify" => {
            let store = lock_audit(audit)?;
            serde_json::to_value(AuditStore::verify_path(&store.path)?)?
        }
        "tee_attest" => {
            let nonce = rpc
                .params
                .as_ref()
                .and_then(|params| params.get("arguments"))
                .and_then(|arguments| arguments.get("nonce"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let status = {
                let store = lock_audit(audit)?;
                store.status()
            };
            let binding = json!({
                "schema": "confidential-agent/gateway-tee-attest-binding/v1",
                "nonce": nonce,
                "audit_chain_digest": status.latest_hash,
                "audit_total_records": status.total_records,
                "upstream_port": route.upstream_port,
                "timestamp": now_unix(),
            });
            let runtime_data_sha256 = hex_sha256(&canonical_json(&binding));
            tee_attest_result(route, &runtime_data_sha256, binding)?
        }
        other => bail!("unsupported virtual tool '{other}'"),
    };
    HttpResponse::json(
        200,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&result)?,
                }],
                "structuredContent": result,
            }
        }),
    )
}

fn lock_audit(audit: &Arc<Mutex<AuditStore>>) -> Result<MutexGuard<'_, AuditStore>> {
    match audit.lock() {
        Ok(store) => Ok(store),
        Err(_) => bail!("audit store lock poisoned; refusing to extend audit chain"),
    }
}

fn json_rpc_error_response(id: Value, code: i64, message: String) -> Result<HttpResponse> {
    HttpResponse::json(
        200,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": code,
                "message": message,
            }
        }),
    )
}

fn inject_virtual_tools(mut response: HttpResponse) -> Result<HttpResponse> {
    if let Ok(mut value) = serde_json::from_slice::<Value>(&response.body) {
        if inject_virtual_tools_value(&mut value) {
            set_response_body(&mut response, serde_json::to_vec(&value)?);
        }
        return Ok(response);
    }

    let Ok(text) = String::from_utf8(response.body.clone()) else {
        return Ok(response);
    };
    if let Some(updated) = inject_virtual_tools_sse(&text)? {
        set_response_body(&mut response, updated.into_bytes());
    }
    Ok(response)
}

fn inject_virtual_tools_value(value: &mut Value) -> bool {
    let Some(tools) = value
        .get_mut("result")
        .and_then(|result| result.get_mut("tools"))
        .and_then(Value::as_array_mut)
    else {
        return false;
    };

    let existing = tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();
    for tool in virtual_tool_definitions() {
        if tool
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| existing.contains(name))
        {
            continue;
        }
        tools.push(tool);
    }
    true
}

fn virtual_tool_definitions() -> [Value; 3] {
    [
        json!({"name":"tee_attest","description":"Get TEE evidence bound to the current gateway audit chain","inputSchema":{"type":"object","properties":{"nonce":{"type":"string"}}}}),
        json!({"name":"audit_status","description":"Get MCP audit chain status","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"audit_verify","description":"Verify MCP audit chain integrity","inputSchema":{"type":"object","properties":{}}}),
    ]
}

fn inject_virtual_tools_sse(text: &str) -> Result<Option<String>> {
    let normalized = text.replace("\r\n", "\n");
    let mut out = String::with_capacity(normalized.len() + 512);
    let mut changed = false;

    for event in normalized.split("\n\n") {
        if event.is_empty() {
            continue;
        }
        if !changed {
            let data = event
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n");
            if !data.is_empty() {
                if let Ok(mut value) = serde_json::from_str::<Value>(&data) {
                    if inject_virtual_tools_value(&mut value) {
                        for line in event.lines().filter(|line| !line.starts_with("data:")) {
                            out.push_str(line);
                            out.push('\n');
                        }
                        out.push_str("data: ");
                        out.push_str(&serde_json::to_string(&value)?);
                        out.push_str("\n\n");
                        changed = true;
                        continue;
                    }
                }
            }
        }
        out.push_str(event);
        out.push_str("\n\n");
    }

    Ok(changed.then_some(out))
}

fn set_response_body(response: &mut HttpResponse, body: Vec<u8>) {
    response.body = body;
    response
        .headers
        .retain(|(name, _)| !name.eq_ignore_ascii_case("content-length"));
    response.headers.push((
        "content-length".to_string(),
        response.body.len().to_string(),
    ));
}

fn tee_attest_result(
    route: &ServerRoute,
    runtime_data_sha256: &str,
    runtime_binding: Value,
) -> Result<Value> {
    let suffix = new_jti().context("failed to allocate tee_attest temp suffix")?;
    let evidence = format!(
        "/tmp/cai-gateway-evidence-{}-{}-{}.json",
        route.upstream_port,
        std::process::id(),
        suffix
    );
    let output = Command::new("attestation-challenge-client")
        .arg("get-evidence")
        .arg("--aa-url")
        .arg(TRUSTIFLUX_AA_URL)
        .arg("--runtime-data")
        .arg(runtime_data_sha256)
        .arg("--output")
        .arg(&evidence)
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .env("NO_PROXY", "*")
        .output()
        .context("failed to run attestation-challenge-client get-evidence")?;
    if !output.status.success() {
        let _ = fs::remove_file(&evidence);
        bail!(
            "attestation-challenge-client get-evidence failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let bytes = fs::read(&evidence).with_context(|| format!("failed to read '{evidence}'"))?;
    let _ = fs::remove_file(&evidence);
    let evidence_json: Value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({"raw": URL_SAFE_NO_PAD.encode(&bytes)}));
    Ok(json!({
        "timestamp": now_unix(),
        "runtime_data_sha256": runtime_data_sha256,
        "runtime_binding": runtime_binding,
        "evidence_sha256": hex_sha256(&bytes),
        "evidence": evidence_json,
    }))
}

fn response_json_result_hash(body: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<Value>(body).ok()?;
    value.get("result").map(hash_json_value)
}

#[derive(Debug)]
struct AuditInput {
    timestamp: u64,
    caller_service_id: String,
    caller_jti: String,
    method: String,
    tool_name: Option<String>,
    params_hash: Option<String>,
    result_hash: Option<String>,
    http_status: u16,
}

#[derive(Debug, Serialize, Deserialize)]
struct AuditLine {
    schema: String,
    seq: u64,
    prev_hash: String,
    chain_hash: String,
    record: Value,
}

#[derive(Debug, Serialize)]
pub struct AuditVerifyReport {
    pub valid: bool,
    pub total_records: u64,
    pub latest_hash: Option<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct AuditStatus {
    pub path: PathBuf,
    pub total_records: u64,
    pub latest_hash: Option<String>,
    pub methods_summary: BTreeMap<String, u64>,
}

pub struct AuditStore {
    path: PathBuf,
    next_seq: u64,
    latest_hash: String,
    methods_summary: BTreeMap<String, u64>,
}

impl AuditStore {
    pub fn open(path: PathBuf, service_id: &str) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create audit dir '{}'", parent.display()))?;
        }
        if !path.exists() {
            let mut store = Self {
                path,
                next_seq: 0,
                latest_hash: "0".repeat(64),
                methods_summary: BTreeMap::new(),
            };
            store.append_genesis(service_id)?;
            return Ok(store);
        }
        let report = Self::verify_path(&path)?;
        if !report.valid {
            bail!("existing audit chain '{}' is invalid", path.display());
        }
        let mut latest_hash = "0".repeat(64);
        let mut next_seq = 0;
        let mut methods_summary = BTreeMap::new();
        for line in fs::read_to_string(&path)?
            .lines()
            .filter(|line| !line.trim().is_empty())
        {
            let line: AuditLine = serde_json::from_str(line)?;
            latest_hash = line.chain_hash;
            next_seq = line.seq + 1;
            if let Some(method) = line.record.get("method").and_then(Value::as_str) {
                *methods_summary.entry(method.to_string()).or_insert(0) += 1;
            }
        }
        Ok(Self {
            path,
            next_seq,
            latest_hash,
            methods_summary,
        })
    }

    fn append_genesis(&mut self, service_id: &str) -> Result<()> {
        let record = json!({
            "type": "genesis",
            "service_id": service_id,
            "timestamp": now_unix(),
        });
        self.append_record(record)
    }

    fn append(&mut self, input: AuditInput) -> Result<()> {
        let method = input.method.clone();
        let record = json!({
            "type": "mcp_call",
            "timestamp": input.timestamp,
            "caller_service_id": input.caller_service_id,
            "caller_jti": input.caller_jti,
            "method": input.method,
            "tool_name": input.tool_name,
            "params_hash": input.params_hash,
            "result_hash": input.result_hash,
            "http_status": input.http_status,
        });
        self.append_record(record)?;
        *self.methods_summary.entry(method).or_insert(0) += 1;
        Ok(())
    }

    fn append_record(&mut self, record: Value) -> Result<()> {
        let prev_hash = self.latest_hash.clone();
        let chain_hash = chain_hash(&prev_hash, &record);
        let line = AuditLine {
            schema: AUDIT_SCHEMA.to_string(),
            seq: self.next_seq,
            prev_hash,
            chain_hash: chain_hash.clone(),
            record,
        };
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open '{}'", self.path.display()))?;
        file.write_all(serde_json::to_string(&line)?.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        self.latest_hash = chain_hash;
        self.next_seq += 1;
        Ok(())
    }

    pub fn status(&self) -> AuditStatus {
        AuditStatus {
            path: self.path.clone(),
            total_records: self.next_seq,
            latest_hash: Some(self.latest_hash.clone()),
            methods_summary: self.methods_summary.clone(),
        }
    }

    pub fn verify_path(path: &Path) -> Result<AuditVerifyReport> {
        let mut prev_hash = "0".repeat(64);
        let mut expected_seq = 0u64;
        let mut latest_hash = None;
        let mut errors = Vec::new();
        if !path.exists() {
            return Ok(AuditVerifyReport {
                valid: false,
                total_records: 0,
                latest_hash: None,
                errors: vec![format!("audit file '{}' does not exist", path.display())],
            });
        }
        for (idx, line) in fs::read_to_string(path)?.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let parsed: AuditLine = match serde_json::from_str(line) {
                Ok(line) => line,
                Err(err) => {
                    errors.push(format!("line {} is invalid JSON: {err}", idx + 1));
                    continue;
                }
            };
            if parsed.schema != AUDIT_SCHEMA {
                errors.push(format!("line {} has unsupported schema", idx + 1));
            }
            if parsed.seq != expected_seq {
                errors.push(format!(
                    "line {} seq {} does not match expected {}",
                    idx + 1,
                    parsed.seq,
                    expected_seq
                ));
            }
            if parsed.prev_hash != prev_hash {
                errors.push(format!("line {} prev_hash mismatch", idx + 1));
            }
            let expected_hash = chain_hash(&parsed.prev_hash, &parsed.record);
            if parsed.chain_hash != expected_hash {
                errors.push(format!("line {} chain_hash mismatch", idx + 1));
            }
            prev_hash = parsed.chain_hash.clone();
            latest_hash = Some(parsed.chain_hash);
            expected_seq += 1;
        }
        Ok(AuditVerifyReport {
            valid: errors.is_empty(),
            total_records: expected_seq,
            latest_hash,
            errors,
        })
    }
}

fn chain_hash(prev_hash: &str, record: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash.as_bytes());
    hasher.update(canonical_json(record));
    hex_encode(&hasher.finalize())
}

fn hash_json_value(value: &Value) -> String {
    hex_sha256(&canonical_json(value))
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn canonical_json(value: &Value) -> Vec<u8> {
    match value {
        Value::Null => b"null".to_vec(),
        Value::Bool(value) => {
            if *value {
                b"true".to_vec()
            } else {
                b"false".to_vec()
            }
        }
        Value::Number(value) => value.to_string().into_bytes(),
        Value::String(value) => {
            serde_json::to_vec(value).expect("string serialization cannot fail")
        }
        Value::Array(values) => {
            let mut out = Vec::from("[");
            for (idx, item) in values.iter().enumerate() {
                if idx > 0 {
                    out.push(b',');
                }
                out.extend(canonical_json(item));
            }
            out.push(b']');
            out
        }
        Value::Object(values) => {
            let mut out = Vec::from("{");
            let mut first = true;
            let sorted = values.iter().collect::<BTreeMap<_, _>>();
            for (key, value) in sorted {
                if !first {
                    out.push(b',');
                }
                first = false;
                out.extend(serde_json::to_vec(key).expect("string serialization cannot fail"));
                out.push(b':');
                out.extend(canonical_json(value));
            }
            out.push(b'}');
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn keypair(seed: u8) -> (String, String) {
        let private = [seed; 32];
        let signing = SigningKey::from_bytes(&private);
        (
            URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes()),
            URL_SAFE_NO_PAD.encode(private),
        )
    }

    fn config() -> GatewayConfig {
        let (public_key, private_key) = keypair(7);
        GatewayConfig {
            schema: GATEWAY_CONFIG_SCHEMA.to_string(),
            service_id: "client".to_string(),
            identity: Some(GatewayIdentity {
                public_key: public_key.clone(),
                private_key,
            }),
            mesh_generation: 1,
            token_ttl_sec: Some(60),
            so_mark: Some(DEFAULT_SO_MARK),
            trusted_services: BTreeMap::from([(
                "client".to_string(),
                GatewayTrustedService { public_key },
            )]),
            client_routes: Vec::new(),
            server_routes: Vec::new(),
        }
    }

    #[test]
    fn token_rejects_audience_mismatch() {
        let config = config();
        let token = sign_service_token(&config, "server", 8000, 100).unwrap();
        let err = verify_service_token(
            &token,
            &config.trusted_services,
            &audience("server", 8001),
            101,
            &ReplayCache::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("audience"));
    }

    #[test]
    fn empty_config_allows_missing_identity() {
        let config = GatewayConfig {
            schema: GATEWAY_CONFIG_SCHEMA.to_string(),
            service_id: "connect-only".to_string(),
            identity: None,
            mesh_generation: 0,
            token_ttl_sec: Some(60),
            so_mark: Some(DEFAULT_SO_MARK),
            trusted_services: BTreeMap::new(),
            client_routes: Vec::new(),
            server_routes: Vec::new(),
        };

        config.validate().unwrap();
    }

    #[test]
    fn client_route_requires_identity() {
        let config = GatewayConfig {
            schema: GATEWAY_CONFIG_SCHEMA.to_string(),
            service_id: "client".to_string(),
            identity: None,
            mesh_generation: 1,
            token_ttl_sec: Some(60),
            so_mark: Some(DEFAULT_SO_MARK),
            trusted_services: BTreeMap::new(),
            client_routes: vec![ClientRoute {
                listen_host: "127.0.0.1".to_string(),
                listen_port: 31000,
                tng_host: "127.0.0.1".to_string(),
                tng_port: 32000,
                target_service: "server".to_string(),
                target_port: 8000,
            }],
            server_routes: Vec::new(),
        };

        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("identity"));
    }

    #[test]
    fn token_rejects_replayed_jti() {
        let config = config();
        let token = sign_service_token(&config, "server", 8000, 100).unwrap();
        let cache = ReplayCache::default();
        verify_service_token(
            &token,
            &config.trusted_services,
            &audience("server", 8000),
            101,
            &cache,
        )
        .unwrap();
        let err = verify_service_token(
            &token,
            &config.trusted_services,
            &audience("server", 8000),
            102,
            &cache,
        )
        .unwrap_err();
        assert!(err.to_string().contains("replayed"));
    }

    #[test]
    fn token_rejects_expired_claims() {
        let config = config();
        let token = sign_service_token(&config, "server", 8000, 100).unwrap();
        let err = verify_service_token(
            &token,
            &config.trusted_services,
            &audience("server", 8000),
            200,
            &ReplayCache::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("expired"));
    }

    #[test]
    fn http_response_uses_status_reason_phrase() {
        let response = HttpResponse::json(502, json!({"error":"upstream"})).unwrap();
        let rendered = String::from_utf8(response.to_bytes()).unwrap();
        assert!(rendered.starts_with("HTTP/1.1 502 Bad Gateway\r\n"));
    }

    #[test]
    fn json_rpc_error_response_keeps_virtual_tool_errors_in_protocol() {
        let response =
            json_rpc_error_response(json!(7), -32000, "attestation failed".to_string()).unwrap();
        let body: Value = serde_json::from_slice(&response.body).unwrap();

        assert_eq!(response.status, 200);
        assert_eq!(body["id"], json!(7));
        assert_eq!(body["error"]["code"], json!(-32000));
        assert_eq!(body["error"]["message"], json!("attestation failed"));
    }

    #[test]
    fn mcp_upstream_failure_returns_json_rpc_error() {
        let upstream_port = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let downstream_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let downstream_addr = downstream_listener.local_addr().unwrap();
        let audit_path =
            std::env::temp_dir().join(format!("cai-gateway-test-{}.jsonl", new_jti().unwrap()));
        let audit = Arc::new(Mutex::new(
            AuditStore::open(audit_path.clone(), "server").unwrap(),
        ));
        let route = ServerRoute {
            listen_host: "127.0.0.1".to_string(),
            listen_port: 0,
            upstream_host: "127.0.0.1".to_string(),
            upstream_port,
            protocol: "mcp".to_string(),
            audit_path: Some(audit_path.clone()),
        };

        let handle = thread::spawn(move || {
            let (stream, _) = downstream_listener.accept().unwrap();
            handle_mcp_connection(
                &route,
                audit,
                VerifiedCaller {
                    service_id: "client".to_string(),
                    jti: "jti".to_string(),
                },
                stream,
            )
            .unwrap();
        });
        let mut client = TcpStream::connect(downstream_addr).unwrap();
        let body = r#"{"jsonrpc":"2.0","id":7,"method":"tools/list","params":{}}"#;
        client
            .write_all(
                format!(
                    "POST /mcp HTTP/1.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();

        handle.join().unwrap();
        let _ = fs::remove_file(audit_path);
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("\"id\":7"));
        assert!(response.contains("\"code\":-32502"));
        assert!(response.contains("upstream MCP server unavailable"));
    }

    #[test]
    fn mcp_rejects_unstructured_request_before_upstream() {
        let upstream_port = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let downstream_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let downstream_addr = downstream_listener.local_addr().unwrap();
        let audit_path =
            std::env::temp_dir().join(format!("cai-gateway-test-{}.jsonl", new_jti().unwrap()));
        let audit = Arc::new(Mutex::new(
            AuditStore::open(audit_path.clone(), "server").unwrap(),
        ));
        let route = ServerRoute {
            listen_host: "127.0.0.1".to_string(),
            listen_port: 0,
            upstream_host: "127.0.0.1".to_string(),
            upstream_port,
            protocol: "mcp".to_string(),
            audit_path: Some(audit_path.clone()),
        };

        let handle = thread::spawn(move || {
            let (stream, _) = downstream_listener.accept().unwrap();
            handle_mcp_connection(
                &route,
                audit,
                VerifiedCaller {
                    service_id: "client".to_string(),
                    jti: "jti".to_string(),
                },
                stream,
            )
            .unwrap();
        });
        let mut client = TcpStream::connect(downstream_addr).unwrap();
        let body = br#"{"jsonrpc":"2.0","id":9,"params":{}}"#;
        client
            .write_all(
                format!(
                    "POST /mcp HTTP/1.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
        client.write_all(body).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();

        handle.join().unwrap();
        let _ = fs::remove_file(audit_path);
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("\"id\":9"));
        assert!(response.contains("\"code\":-32600"));
        assert!(!response.contains("upstream MCP server unavailable"));
    }

    #[test]
    fn upstream_response_read_is_bounded() {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_port = upstream_listener.local_addr().unwrap().port();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let handle = thread::spawn(move || {
            let (_stream, _) = upstream_listener.accept().unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
        });
        let route = ServerRoute {
            listen_host: "127.0.0.1".to_string(),
            listen_port: 0,
            upstream_host: "127.0.0.1".to_string(),
            upstream_port,
            protocol: "mcp".to_string(),
            audit_path: None,
        };
        let request = HttpRequest {
            method: "POST".to_string(),
            path: "/mcp".to_string(),
            headers: Vec::new(),
            body: br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#.to_vec(),
            connection_close: true,
        };

        let err = forward_http_request_with_timeout(&route, &request, Duration::from_millis(100))
            .unwrap_err();
        let _ = release_tx.send(());

        handle.join().unwrap();
        let err = format!("{err:#}");
        assert!(
            err.contains("timed out")
                || err.contains("would block")
                || err.contains("Resource temporarily unavailable"),
            "unexpected timeout error: {err}"
        );
    }

    #[test]
    fn http_response_decodes_chunked_body_and_drops_transfer_encoding() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#.to_string();
        let first = body[..body.len() / 2].to_string();
        let second = body[body.len() / 2..].to_string();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n{:x};source=test\r\n{}\r\n0\r\nX-Trailer: ok\r\n\r\n",
                        first.len(),
                        first,
                        second.len(),
                        second
                    )
                    .as_bytes(),
                )
                .unwrap();
        });
        let mut stream = TcpStream::connect(addr).unwrap();

        let response = read_http_response(&mut stream).unwrap();
        handle.join().unwrap();

        assert_eq!(response.body, body.as_bytes());
        assert!(!response
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("transfer-encoding")));
        let rendered = String::from_utf8(response.to_bytes()).unwrap();
        assert!(rendered.contains(&format!("content-length: {}\r\n", body.len())));
        assert!(!rendered.contains("\r\n0\r\n"));
    }

    #[test]
    fn http_response_rejects_chunked_with_content_length() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 1\r\nConnection: close\r\n\r\n1\r\nx\r\n0\r\n\r\n",
                )
                .unwrap();
        });
        let mut stream = TcpStream::connect(addr).unwrap();

        let err = read_http_response(&mut stream).unwrap_err();
        handle.join().unwrap();

        assert!(err
            .to_string()
            .contains("both Transfer-Encoding: chunked and Content-Length"));
    }

    #[test]
    fn http_response_rejects_chunked_trailing_data() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n1\r\nx\r\n0\r\n\r\njunk",
                )
                .unwrap();
        });
        let mut stream = TcpStream::connect(addr).unwrap();

        let err = read_http_response(&mut stream).unwrap_err();
        handle.join().unwrap();

        assert!(err
            .to_string()
            .contains("trailing data after final trailer"));
    }

    #[test]
    fn http_response_rejects_conflicting_duplicate_content_length() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\nConnection: close\r\n\r\nx",
                )
                .unwrap();
        });
        let mut stream = TcpStream::connect(addr).unwrap();

        let err = read_http_response(&mut stream).unwrap_err();
        handle.join().unwrap();

        assert!(err
            .to_string()
            .contains("conflicting duplicate HTTP response Content-Length"));
    }

    #[test]
    fn mcp_tools_list_injection_handles_chunked_sse_upstream_response() {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream_port = upstream_listener.local_addr().unwrap().port();
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n";
        let first = body[..body.len() / 2].to_string();
        let second = body[body.len() / 2..].to_string();
        let handle = thread::spawn(move || {
            let (mut stream, _) = upstream_listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n{:x}\r\n{}\r\n0\r\n\r\n",
                        first.len(),
                        first,
                        second.len(),
                        second
                    )
                    .as_bytes(),
                )
                .unwrap();
        });
        let route = ServerRoute {
            listen_host: "127.0.0.1".to_string(),
            listen_port: 0,
            upstream_host: "127.0.0.1".to_string(),
            upstream_port,
            protocol: "mcp".to_string(),
            audit_path: None,
        };
        let request = HttpRequest {
            method: "POST".to_string(),
            path: "/mcp".to_string(),
            headers: Vec::new(),
            body: br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#.to_vec(),
            connection_close: true,
        };

        let response = forward_http_request(&route, &request).unwrap();
        handle.join().unwrap();
        let response = inject_virtual_tools(response).unwrap();
        let rendered = String::from_utf8(response.body).unwrap();

        assert!(rendered.contains("\"name\":\"tee_attest\""));
        assert!(rendered.contains("\"name\":\"audit_status\""));
        assert!(rendered.contains("\"name\":\"audit_verify\""));
        assert!(!rendered.contains("\r\n0\r\n"));
    }

    #[test]
    fn inject_virtual_tools_handles_sse_tools_list_response() {
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n";
        let response = HttpResponse {
            status: 200,
            headers: vec![
                ("content-type".to_string(), "text/event-stream".to_string()),
                ("content-length".to_string(), body.len().to_string()),
            ],
            body: body.as_bytes().to_vec(),
        };

        let response = inject_virtual_tools(response).unwrap();
        let rendered = String::from_utf8(response.body).unwrap();

        assert!(rendered.contains("\"name\":\"tee_attest\""));
        assert!(rendered.contains("\"name\":\"audit_status\""));
        assert!(rendered.contains("\"name\":\"audit_verify\""));
        assert!(response
            .headers
            .iter()
            .any(|(name, value)| name == "content-length" && value == &rendered.len().to_string()));
    }

    #[test]
    fn http_request_rejects_oversized_body_before_allocation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(
                    format!(
                        "POST /mcp HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                        MAX_HTTP_BODY_BYTES + 1
                    )
                    .as_bytes(),
                )
                .unwrap();
        });
        let mut stream = TcpStream::connect(addr).unwrap();
        let err = read_http_request(&mut stream).unwrap_err();
        handle.join().unwrap();
        assert!(err.to_string().contains("body too large"));
    }

    #[test]
    fn http_response_rejects_oversized_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\n")
                .unwrap();
            stream
                .write_all(&vec![
                    b'x';
                    MAX_HTTP_BODY_BYTES + MAX_HTTP_HEADERS_BYTES + 1
                ])
                .unwrap();
        });
        let mut stream = TcpStream::connect(addr).unwrap();
        let err = read_http_response(&mut stream).unwrap_err();
        handle.join().unwrap();
        assert!(err.to_string().contains("too large"));
    }

    #[test]
    fn frame_rejects_bad_magic() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"BAD!\x01\x00\x01x").unwrap();
        });
        let mut stream = TcpStream::connect(addr).unwrap();
        let err = read_frame(&mut stream).unwrap_err();
        handle.join().unwrap();
        assert!(err.to_string().contains("magic"));
    }

    #[test]
    fn audit_detects_tampering() {
        let dir = tempfile_dir();
        let path = dir.join("audit.jsonl");
        let mut store = AuditStore::open(path.clone(), "svc").unwrap();
        store
            .append(AuditInput {
                timestamp: 1,
                caller_service_id: "client".to_string(),
                caller_jti: "jti".to_string(),
                method: "tools/call".to_string(),
                tool_name: Some("create_entities".to_string()),
                params_hash: Some("p".to_string()),
                result_hash: Some("r".to_string()),
                http_status: 200,
            })
            .unwrap();
        assert!(AuditStore::verify_path(&path).unwrap().valid);
        let mut content = fs::read_to_string(&path).unwrap();
        content = content.replace("create_entities", "delete_all_entities");
        fs::write(&path, content).unwrap();
        let report = AuditStore::verify_path(&path).unwrap();
        assert!(!report.valid);
        assert!(report.errors.iter().any(|err| err.contains("chain_hash")));
    }

    #[test]
    fn canonical_json_sorts_keys() {
        let left = json!({"b":2,"a":{"z":1,"c":3}});
        let right = json!({"a":{"c":3,"z":1},"b":2});
        assert_eq!(canonical_json(&left), canonical_json(&right));
    }

    fn tempfile_dir() -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "cai-gateway-test-{}-{}",
            std::process::id(),
            new_jti().unwrap()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
