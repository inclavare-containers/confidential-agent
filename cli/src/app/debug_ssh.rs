use super::*;

#[cfg(any(feature = "full", test))]
pub(super) fn ensure_debug_ssh_key(
    paths: &ContextPaths,
    spec: &mut AgentSpec,
) -> Result<Option<LocalDebugSshKey>> {
    let service_id = spec.service.id.clone();
    ensure_debug_ssh_key_with_generator(paths, spec, |private_key, public_key| {
        generate_debug_ssh_key(&service_id, private_key, public_key)
    })
}

#[cfg(any(feature = "full", test))]
pub(super) fn ensure_debug_ssh_key_with_generator<F>(
    paths: &ContextPaths,
    spec: &mut AgentSpec,
    generate: F,
) -> Result<Option<LocalDebugSshKey>>
where
    F: FnOnce(&Path, &Path) -> Result<()>,
{
    if !spec.deploys_debug_image() {
        return Ok(None);
    }
    let Some(debug) = spec.build.variants.debug.as_mut() else {
        bail!("deploy.image_variant=debug requires build.variants.debug");
    };
    if debug.ssh_public_key.is_some() {
        return Ok(None);
    }

    fs::create_dir_all(&paths.secrets_dir)
        .with_context(|| format!("failed to create '{}'", paths.secrets_dir.display()))?;
    let private_key = paths.secrets_dir.join("debug_ssh");
    let public_key = paths.secrets_dir.join("debug_ssh.pub");
    match (private_key.exists(), public_key.exists()) {
        (true, true) => {}
        (false, false) => generate(&private_key, &public_key)?,
        _ => bail!(
            "incomplete generated debug SSH key under '{}'; remove both debug_ssh files or configure build.variants.debug.ssh_public_key explicitly",
            paths.secrets_dir.display()
        ),
    }
    if !private_key.exists() || !public_key.exists() {
        bail!(
            "debug SSH key generator did not create '{}' and '{}'",
            private_key.display(),
            public_key.display()
        );
    }
    set_mode(&private_key, 0o600)?;
    set_mode(&public_key, 0o644)?;
    debug.ssh_public_key = Some(public_key.clone());
    Ok(Some(LocalDebugSshKey {
        private_key,
        public_key,
    }))
}

#[cfg(any(feature = "full", test))]
pub(super) fn stage_mkosi_debug_ssh_authorized_keys(
    assets: &mut GuestAssets,
    paths: &ContextPaths,
    public_key: Option<&Path>,
) -> Result<()> {
    let Some(public_key) = public_key else {
        return Ok(());
    };
    let content = fs::read_to_string(public_key)
        .with_context(|| format!("failed to read '{}'", public_key.display()))?;
    let authorized_keys = paths.guest_staging_dir.join("debug_authorized_keys");
    fs::write(&authorized_keys, content)
        .with_context(|| format!("failed to write '{}'", authorized_keys.display()))?;
    set_mode(&authorized_keys, 0o600)?;
    assets.extra_files.push(GuestFileAsset {
        source: authorized_keys,
        destination: "/root/.ssh/authorized_keys".to_string(),
        executable: false,
    });
    let marker = paths.guest_staging_dir.join("debug-ssh-enabled");
    fs::write(&marker, b"1\n")
        .with_context(|| format!("failed to write '{}'", marker.display()))?;
    set_mode(&marker, 0o644)?;
    assets.extra_files.push(GuestFileAsset {
        source: marker,
        destination: "/etc/confidential-agent/debug-ssh-enabled".to_string(),
        executable: false,
    });
    Ok(())
}

#[cfg(any(feature = "full", test))]
pub(super) fn generate_debug_ssh_key(
    service_id: &str,
    private_key: &Path,
    public_key: &Path,
) -> Result<()> {
    generate_ssh_key(service_id, "debug", private_key, public_key)
}

pub(super) fn generate_ssh_key(
    service_id: &str,
    purpose: &str,
    private_key: &Path,
    public_key: &Path,
) -> Result<()> {
    let mut rng = OsRng;
    let mut seed = [0_u8; 32];
    rng.fill_bytes(&mut seed);
    let (public_key_bytes, private_key_bytes) = ed25519_keypair_from_seed(&seed);
    let comment = format!("confidential-agent:{service_id}:{purpose}");
    let public_blob = openssh_ed25519_public_blob(&public_key_bytes)?;
    let private_pem = openssh_ed25519_private_key(
        &public_blob,
        &public_key_bytes,
        &private_key_bytes,
        &comment,
        rng.next_u32(),
    )?;
    let public_line = format!(
        "ssh-ed25519 {} {}\n",
        BASE64_STANDARD.encode(&public_blob),
        comment
    );
    fs::write(private_key, private_pem)
        .with_context(|| format!("failed to write '{}'", private_key.display()))?;
    fs::write(public_key, public_line)
        .with_context(|| format!("failed to write '{}'", public_key.display()))?;
    Ok(())
}

pub(super) fn ed25519_keypair_from_seed(seed: &[u8; 32]) -> ([u8; 32], [u8; 64]) {
    let digest = Sha512::digest(seed);
    let mut scalar_bytes = [0_u8; 32];
    scalar_bytes.copy_from_slice(&digest[..32]);
    scalar_bytes[0] &= 248;
    scalar_bytes[31] &= 63;
    scalar_bytes[31] |= 64;

    let scalar = Scalar::from_bytes_mod_order(scalar_bytes);
    let public_key = (ED25519_BASEPOINT_POINT * scalar).compress().to_bytes();
    let mut private_key = [0_u8; 64];
    private_key[..32].copy_from_slice(seed);
    private_key[32..].copy_from_slice(&public_key);
    (public_key, private_key)
}

pub(super) fn openssh_ed25519_public_blob(public_key: &[u8; 32]) -> Result<Vec<u8>> {
    let mut blob = Vec::new();
    put_ssh_string(&mut blob, b"ssh-ed25519")?;
    put_ssh_string(&mut blob, public_key)?;
    Ok(blob)
}

pub(super) fn openssh_ed25519_private_key(
    public_blob: &[u8],
    public_key: &[u8; 32],
    private_key: &[u8; 64],
    comment: &str,
    check: u32,
) -> Result<String> {
    let mut private = Vec::new();
    put_u32(&mut private, check);
    put_u32(&mut private, check);
    put_ssh_string(&mut private, b"ssh-ed25519")?;
    put_ssh_string(&mut private, public_key)?;
    put_ssh_string(&mut private, private_key)?;
    put_ssh_string(&mut private, comment.as_bytes())?;
    let padding = 8 - (private.len() % 8);
    for value in 1..=padding {
        private.push(value as u8);
    }

    let mut outer = Vec::new();
    outer.extend_from_slice(b"openssh-key-v1\0");
    put_ssh_string(&mut outer, b"none")?;
    put_ssh_string(&mut outer, b"none")?;
    put_ssh_string(&mut outer, b"")?;
    put_u32(&mut outer, 1);
    put_ssh_string(&mut outer, public_blob)?;
    put_ssh_string(&mut outer, &private)?;

    let encoded = BASE64_STANDARD.encode(outer);
    let mut pem = String::from("-----BEGIN OPENSSH PRIVATE KEY-----\n");
    for chunk in encoded.as_bytes().chunks(70) {
        pem.push_str(std::str::from_utf8(chunk).context("base64 output is not valid UTF-8")?);
        pem.push('\n');
    }
    pem.push_str("-----END OPENSSH PRIVATE KEY-----\n");
    Ok(pem)
}

pub(super) fn put_ssh_string(buf: &mut Vec<u8>, value: &[u8]) -> Result<()> {
    let len = u32::try_from(value.len()).context("OpenSSH string is too large")?;
    put_u32(buf, len);
    buf.extend_from_slice(value);
    Ok(())
}

pub(super) fn put_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use confidential_agent_core::spec::AgentSpec;

    const DEBUG_SPEC: &str = r#"
schema: confidential-agent/v1
service:
  id: test-svc
  ports: [8080]
build:
  image_name: test-agent
  variants:
    release:
      enabled: true
    debug:
      enabled: true
deploy:
  provider: aliyun
  image_variant: debug
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#;

    const RELEASE_SPEC: &str = r#"
schema: confidential-agent/v1
service:
  id: test-svc
  ports: [8080]
build:
  image_name: test-agent
  variants:
    release:
      enabled: true
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
attestation:
  tee: tdx
  mode: challenge
  reference_values: sample
resources: {}
"#;

    #[test]
    fn ensure_debug_ssh_key_skips_for_release_image() {
        let dir = tempfile::tempdir().unwrap();
        let paths = context_paths(dir.path(), "test-svc");
        let mut spec = AgentSpec::from_yaml(RELEASE_SPEC, Path::new("/project")).unwrap();
        let result = ensure_debug_ssh_key_with_generator(&paths, &mut spec, |_, _| {
            panic!("should not be called");
        })
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn ensure_debug_ssh_key_generates_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let paths = context_paths(dir.path(), "test-svc");
        let mut spec = AgentSpec::from_yaml(DEBUG_SPEC, Path::new("/project")).unwrap();
        let result = ensure_debug_ssh_key_with_generator(&paths, &mut spec, |priv_key, pub_key| {
            fs::write(priv_key, "PRIVATE")?;
            fs::write(pub_key, "PUBLIC")?;
            Ok(())
        })
        .unwrap();
        let ssh = result.unwrap();
        assert!(ssh.private_key.exists());
        assert!(ssh.public_key.exists());
        assert!(spec
            .build
            .variants
            .debug
            .as_ref()
            .unwrap()
            .ssh_public_key
            .is_some());
    }

    #[test]
    fn ensure_debug_ssh_key_skips_when_already_configured() {
        let dir = tempfile::tempdir().unwrap();
        let paths = context_paths(dir.path(), "test-svc");
        let mut spec = AgentSpec::from_yaml(DEBUG_SPEC, Path::new("/project")).unwrap();
        spec.build.variants.debug.as_mut().unwrap().ssh_public_key =
            Some(PathBuf::from("/existing/key.pub"));
        let result = ensure_debug_ssh_key_with_generator(&paths, &mut spec, |_, _| {
            panic!("should not be called");
        })
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn ensure_debug_ssh_key_reuses_existing_keys() {
        let dir = tempfile::tempdir().unwrap();
        let paths = context_paths(dir.path(), "test-svc");
        fs::create_dir_all(&paths.secrets_dir).unwrap();
        fs::write(paths.secrets_dir.join("debug_ssh"), "PRIV").unwrap();
        fs::write(paths.secrets_dir.join("debug_ssh.pub"), "PUB").unwrap();
        let mut spec = AgentSpec::from_yaml(DEBUG_SPEC, Path::new("/project")).unwrap();
        let result = ensure_debug_ssh_key_with_generator(&paths, &mut spec, |_, _| {
            panic!("should not be called");
        })
        .unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn ensure_debug_ssh_key_errors_on_half_state() {
        let dir = tempfile::tempdir().unwrap();
        let paths = context_paths(dir.path(), "test-svc");
        fs::create_dir_all(&paths.secrets_dir).unwrap();
        fs::write(paths.secrets_dir.join("debug_ssh"), "PRIV").unwrap();
        let mut spec = AgentSpec::from_yaml(DEBUG_SPEC, Path::new("/project")).unwrap();
        let err =
            ensure_debug_ssh_key_with_generator(&paths, &mut spec, |_, _| Ok(())).unwrap_err();
        assert!(err.to_string().contains("incomplete"));
    }

    #[test]
    fn ed25519_keypair_deterministic() {
        let seed = [42u8; 32];
        let (pk1, sk1) = ed25519_keypair_from_seed(&seed);
        let (pk2, sk2) = ed25519_keypair_from_seed(&seed);
        assert_eq!(pk1, pk2);
        assert_eq!(sk1, sk2);
    }

    #[test]
    fn ed25519_keypair_different_seeds() {
        let (pk1, _) = ed25519_keypair_from_seed(&[1u8; 32]);
        let (pk2, _) = ed25519_keypair_from_seed(&[2u8; 32]);
        assert_ne!(pk1, pk2);
    }

    #[test]
    fn openssh_public_blob_starts_with_key_type() {
        let (pk, _) = ed25519_keypair_from_seed(&[42u8; 32]);
        let blob = openssh_ed25519_public_blob(&pk).unwrap();
        assert!(blob.len() > 32);
        assert!(String::from_utf8_lossy(&blob).contains("ssh-ed25519"));
    }

    #[test]
    fn openssh_private_key_pem_format() {
        let (pk, sk) = ed25519_keypair_from_seed(&[42u8; 32]);
        let blob = openssh_ed25519_public_blob(&pk).unwrap();
        let pem = openssh_ed25519_private_key(&blob, &pk, &sk, "test-comment", 12345).unwrap();
        assert!(pem.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----\n"));
        assert!(pem.ends_with("-----END OPENSSH PRIVATE KEY-----\n"));
    }

    #[test]
    fn generate_debug_ssh_key_creates_valid_files() {
        let dir = tempfile::tempdir().unwrap();
        let priv_path = dir.path().join("debug_ssh");
        let pub_path = dir.path().join("debug_ssh.pub");
        generate_debug_ssh_key("test-svc", &priv_path, &pub_path).unwrap();
        let pub_content = fs::read_to_string(&pub_path).unwrap();
        assert!(pub_content.starts_with("ssh-ed25519 "));
        assert!(pub_content.contains("confidential-agent:test-svc:debug"));
        let priv_content = fs::read_to_string(&priv_path).unwrap();
        assert!(priv_content.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"));
    }

    #[test]
    fn put_ssh_string_encodes_length_prefix() {
        let mut buf = Vec::new();
        put_ssh_string(&mut buf, b"hello").unwrap();
        assert_eq!(&buf[..4], &[0, 0, 0, 5]);
        assert_eq!(&buf[4..], b"hello");
    }

    #[test]
    fn put_u32_big_endian() {
        let mut buf = Vec::new();
        put_u32(&mut buf, 0x01020304);
        assert_eq!(buf, vec![1, 2, 3, 4]);
    }
}
