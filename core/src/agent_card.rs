use crate::agent_card_signing::sign_agent_card_keyless;
use crate::schema::{
    AgentCard, AgentCardCapabilities, AgentCardConfidential, AgentCardPort, AgentCardRekor,
    AgentCardSkill, AgentExtension, AgentInterface,
};
use crate::spec::{AgentSpec, AttestationTee};
use crate::util::{rekor_payload, required_json_string};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

pub const CONFIDENTIAL_AGENT_EXTENSION: &str =
    "https://confidential-agent.dev/extensions/tee-rekor/v1";

pub fn confidential_extension(card: &AgentCard) -> Result<AgentCardConfidential> {
    let extension = card
        .capabilities
        .extensions
        .iter()
        .find(|extension| extension.uri == CONFIDENTIAL_AGENT_EXTENSION)
        .with_context(|| {
            format!("agent card is missing capabilities.extensions[{CONFIDENTIAL_AGENT_EXTENSION}]")
        })?;
    serde_json::from_value(extension.params.clone()).with_context(|| {
        format!("invalid capabilities.extensions[{CONFIDENTIAL_AGENT_EXTENSION}].params")
    })
}

pub fn validate_confidential_agent_card(card: &AgentCard) -> Result<()> {
    if card.protocol_version.trim().is_empty() {
        bail!("agent card metadata field protocolVersion must not be empty");
    }
    if card.protocol_version.trim() != "1.0" && card.protocol_version.trim() != "1.0.0" {
        bail!("agent card metadata field protocolVersion must be 1.0");
    }
    if card.name.trim().is_empty() {
        bail!("agent card metadata field name must not be empty");
    }
    if card.description.trim().is_empty() {
        bail!("agent card metadata field description must not be empty");
    }
    if card.supported_interfaces.is_empty() {
        bail!("agent card metadata field supportedInterfaces must not be empty");
    }
    for interface in &card.supported_interfaces {
        if interface.url.trim().is_empty() {
            bail!("agent card metadata field supportedInterfaces[].url must not be empty");
        }
        if interface.protocol_binding.trim().is_empty() {
            bail!(
                "agent card metadata field supportedInterfaces[].protocolBinding must not be empty"
            );
        }
        if interface.protocol_version.trim().is_empty() {
            bail!(
                "agent card metadata field supportedInterfaces[].protocolVersion must not be empty"
            );
        }
    }
    let ext = confidential_extension(card)?;
    validate_id("agent card confidential id", &ext.id)?;
    if ext.public_ip.trim().is_empty() {
        bail!("agent card security field confidential publicIp must not be empty");
    }
    if ext.ports.is_empty() {
        bail!("agent card security field confidential ports must not be empty");
    }
    if ext.reference_values.as_ref().is_some_and(Value::is_null) {
        bail!("agent card security field referenceValues must not be null");
    }
    for port in &ext.ports {
        if port.name.trim().is_empty() {
            bail!("agent card security field confidential port name must not be empty");
        }
        if port.port == 0 {
            bail!("agent card security field confidential ports must be greater than 0");
        }
    }
    let rekor = &ext.rekor;
    if rekor.rekor_url.trim().is_empty() {
        bail!("agent card security field rekorUrl must not be empty");
    }
    if rekor.artifact_id.trim().is_empty() {
        bail!("agent card security field artifactId must not be empty");
    }
    if rekor.artifact_type.trim().is_empty() {
        bail!("agent card security field artifactType must not be empty");
    }
    if rekor.artifact_version.trim().is_empty() {
        bail!("agent card security field artifactVersion must not be empty");
    }
    if rekor.rv_name.trim().is_empty() {
        bail!("agent card security field rvName must not be empty");
    }
    Ok(())
}

pub fn agent_card_reference_values(card: &AgentCard) -> Result<Value> {
    let ext = confidential_extension(card)?;
    if let Some(sample) = &ext.reference_values {
        return Ok(json!([{
            "type": "sample",
            "payload": {
                "type": "inline",
                "content": sample,
            },
        }]));
    }

    let rekor = &ext.rekor;
    let metadata = json!({
        "artifact_id": rekor.artifact_id,
        "artifact_version": rekor.artifact_version,
        "artifact_type": rekor.artifact_type,
        "rekor_url": rekor.rekor_url,
        "rv_name": rekor.rv_name,
    });
    let payload = rekor_payload(&metadata)?;
    Ok(json!([{
        "type": "slsa",
        "payload": {
            "type": "inline",
            "content": payload,
        },
    }]))
}

pub const DEFAULT_TNG_POLICY_PATH: &str =
    "/opt/confidential-agent/policies/trustee-opa-default.rego";

pub fn derive_tng_client_config(card: &AgentCard) -> Result<Value> {
    derive_tng_client_config_with_local_ports(card, Ok, 50000, DEFAULT_TNG_POLICY_PATH)
}

pub fn derive_tng_client_config_with_local_ports(
    card: &AgentCard,
    mut local_port_for: impl FnMut(u16) -> Result<u16>,
    control_port: u16,
    policy_path: &str,
) -> Result<Value> {
    let ext = confidential_extension(card)?;
    let reference_values = agent_card_reference_values(card)?;
    let mut ingress = Vec::new();
    for port in &ext.ports {
        let local_port = local_port_for(port.port)?;
        ingress.push(json!({
            "rats_tls": {
                "multiplex": false,
            },
            "mapping": {
                "in": {
                    "host": "127.0.0.1",
                    "port": local_port,
                },
                "out": {
                    "host": ext.public_ip,
                    "port": port.port,
                },
            },
            "verify": {
                "as_type": "builtin",
                "attestation_policy": {
                    "type": "path",
                    "path": policy_path,
                },
                "reference_values": reference_values,
            },
        }));
    }
    Ok(json!({
        "control_interface": {
            "restful": {
                "host": "127.0.0.1",
                "port": control_port,
            }
        },
        "add_ingress": ingress,
    }))
}

pub fn render_agent_card(
    spec: &AgentSpec,
    target_ip: &str,
    meta: &Value,
    sample_reference_values: Option<&Value>,
) -> Result<AgentCard> {
    let a2a = spec
        .a2a
        .as_ref()
        .context("a2a must be configured to render an AgentCard")?;
    if !a2a.enabled {
        bail!("a2a is disabled");
    }
    if spec.service.connect.is_empty() {
        bail!("a2a requires service.connect to expose at least one connect port");
    }

    let supported_interfaces = if a2a.interfaces.is_empty() {
        spec.service
            .connect
            .iter()
            .map(|cp| AgentInterface {
                url: format!("http://{target_ip}:{}/a2a", cp.port()),
                protocol_binding: "JSONRPC".to_string(),
                protocol_version: "1.0".to_string(),
                tenant: None,
            })
            .collect()
    } else {
        a2a.interfaces
            .iter()
            .map(|interface| AgentInterface {
                url: format!("http://{target_ip}:{}{}", interface.port, interface.path),
                protocol_binding: interface.protocol_binding.clone(),
                protocol_version: "1.0".to_string(),
                tenant: None,
            })
            .collect()
    };

    let confidential = AgentCardConfidential {
        id: a2a.id.clone(),
        cache_ttl_sec: a2a.cache_ttl_sec,
        public_ip: target_ip.to_string(),
        ports: spec
            .service
            .connect
            .iter()
            .map(|cp| AgentCardPort {
                name: format!("port-{}", cp.port()),
                port: cp.port(),
            })
            .collect(),
        reference_values: sample_reference_values.cloned(),
        rekor: AgentCardRekor {
            rekor_url: required_json_string(meta, "rekor_url")?.to_string(),
            artifact_id: required_json_string(meta, "artifact_id")?.to_string(),
            artifact_type: required_json_string(meta, "artifact_type")?.to_string(),
            artifact_version: required_json_string(meta, "artifact_version")?.to_string(),
            rv_name: required_json_string(meta, "rv_name")?.to_string(),
        },
        tee: tee_name(spec.attestation.tee).to_string(),
    };

    let mut card = AgentCard {
        protocol_version: "1.0".to_string(),
        name: a2a.name.clone(),
        description: a2a
            .description
            .clone()
            .unwrap_or_else(|| format!("Confidential Agent {}", a2a.name)),
        version: a2a.version.clone(),
        supported_interfaces,
        preferred_transport: Some("JSONRPC".to_string()),
        skills: a2a
            .skills
            .iter()
            .map(|s| AgentCardSkill {
                id: s.id.clone(),
                name: s.name.clone(),
                description: s.description.clone(),
                tags: s.tags.clone(),
                examples: s.examples.clone(),
                input_modes: s.input_modes.clone(),
                output_modes: s.output_modes.clone(),
            })
            .collect(),
        default_input_modes: vec!["text".to_string()],
        default_output_modes: vec!["text".to_string()],
        capabilities: AgentCardCapabilities {
            streaming: Some(false),
            push_notifications: Some(false),
            state_transition_history: Some(false),
            extended_agent_card: Some(false),
            extensions: vec![AgentExtension {
                uri: CONFIDENTIAL_AGENT_EXTENSION.to_string(),
                description: Some(
                    "Confidential Agent TEE/Rekor metadata for RATS-TLS routing".to_string(),
                ),
                required: true,
                params: serde_json::to_value(confidential)?,
            }],
        },
        provider: a2a.provider.clone(),
        security_schemes: None,
        security: Vec::new(),
        supports_authenticated_extended_card: Some(false),
        signatures: Vec::new(),
    };

    if a2a.signing.required {
        sign_agent_card_keyless(&mut card, a2a.signing.oidc_issuer.as_deref())?;
    }

    Ok(card)
}

fn tee_name(tee: AttestationTee) -> &'static str {
    match tee {
        AttestationTee::Tdx => "tdx",
    }
}

pub fn validate_id(field: &str, value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{field} must not be empty");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        bail!("{field} may only contain letters, numbers, underscores, and hyphens");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::*;
    use serde_json::json;

    fn test_rekor() -> AgentCardRekor {
        AgentCardRekor {
            rekor_url: "https://rekor.example.com".to_string(),
            artifact_id: "openclaw-agent".to_string(),
            artifact_type: "uki".to_string(),
            artifact_version: "v0.1.0-build42".to_string(),
            rv_name: "openclaw".to_string(),
        }
    }

    fn test_confidential() -> AgentCardConfidential {
        AgentCardConfidential {
            id: "openclaw-agent".to_string(),
            cache_ttl_sec: 300,
            public_ip: "1.2.3.4".to_string(),
            ports: vec![AgentCardPort {
                name: "main".to_string(),
                port: 18789,
            }],
            reference_values: None,
            rekor: test_rekor(),
            tee: "tdx".to_string(),
        }
    }

    fn test_card() -> AgentCard {
        AgentCard {
            protocol_version: "1.0".to_string(),
            name: "openclaw".to_string(),
            description: "A test agent".to_string(),
            version: None,
            supported_interfaces: vec![AgentInterface {
                url: "http://1.2.3.4:18789".to_string(),
                protocol_binding: "jsonrpc/http".to_string(),
                protocol_version: "1.0".to_string(),
                tenant: None,
            }],
            preferred_transport: None,
            skills: vec![],
            default_input_modes: vec![],
            default_output_modes: vec![],
            capabilities: AgentCardCapabilities {
                extensions: vec![AgentExtension {
                    uri: CONFIDENTIAL_AGENT_EXTENSION.to_string(),
                    description: None,
                    required: true,
                    params: serde_json::to_value(test_confidential()).unwrap(),
                }],
                ..Default::default()
            },
            provider: None,
            security_schemes: None,
            security: vec![],
            supports_authenticated_extended_card: None,
            signatures: vec![],
        }
    }

    fn with_conf(card: &mut AgentCard, f: impl FnOnce(&mut AgentCardConfidential)) {
        let mut conf = confidential_extension(card).unwrap();
        f(&mut conf);
        card.capabilities
            .extensions
            .iter_mut()
            .find(|e| e.uri == CONFIDENTIAL_AGENT_EXTENSION)
            .unwrap()
            .params = serde_json::to_value(conf).unwrap();
    }

    #[test]
    fn validate_id_accepts_alphanumeric_underscore_hyphen() {
        assert!(validate_id("test", "my-agent_v1").is_ok());
        assert!(validate_id("test", "simple").is_ok());
    }

    #[test]
    fn validate_id_rejects_empty() {
        let err = validate_id("field", "").unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn validate_id_rejects_whitespace_only() {
        let err = validate_id("field", "   ").unwrap_err();
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn validate_id_rejects_special_chars() {
        let err = validate_id("field", "my agent").unwrap_err();
        assert!(err.to_string().contains("may only contain"));
        assert!(validate_id("field", "a/b").is_err());
        assert!(validate_id("field", "a.b").is_err());
    }

    #[test]
    fn confidential_extension_returns_value_when_present() {
        let card = test_card();
        let ext = confidential_extension(&card).unwrap();
        assert_eq!(ext.id, "openclaw-agent");
    }

    #[test]
    fn confidential_extension_errors_when_missing() {
        let mut card = test_card();
        card.capabilities.extensions.clear();
        assert!(confidential_extension(&card).is_err());
    }

    #[test]
    fn validate_card_accepts_valid() {
        assert!(validate_confidential_agent_card(&test_card()).is_ok());
    }

    #[test]
    fn validate_card_rejects_empty_name() {
        let mut card = test_card();
        card.name = "".to_string();
        let err = validate_confidential_agent_card(&card).unwrap_err();
        assert!(err.to_string().contains("name must not be empty"));
    }

    #[test]
    fn validate_card_rejects_empty_protocol_version() {
        let mut card = test_card();
        card.protocol_version = "".to_string();
        assert!(validate_confidential_agent_card(&card).is_err());
    }

    #[test]
    fn validate_card_rejects_empty_description() {
        let mut card = test_card();
        card.description = "".to_string();
        assert!(validate_confidential_agent_card(&card).is_err());
    }

    #[test]
    fn validate_card_rejects_empty_supported_interfaces() {
        let mut card = test_card();
        card.supported_interfaces.clear();
        assert!(validate_confidential_agent_card(&card).is_err());
    }

    #[test]
    fn validate_card_rejects_empty_public_ip() {
        let mut card = test_card();
        with_conf(&mut card, |c| c.public_ip = "  ".to_string());
        assert!(validate_confidential_agent_card(&card).is_err());
    }

    #[test]
    fn validate_card_rejects_empty_ports() {
        let mut card = test_card();
        with_conf(&mut card, |c| c.ports = vec![]);
        let err = validate_confidential_agent_card(&card).unwrap_err();
        assert!(err.to_string().contains("ports must not be empty"));
    }

    #[test]
    fn validate_card_rejects_zero_port() {
        let mut card = test_card();
        with_conf(&mut card, |c| {
            c.ports = vec![AgentCardPort {
                name: "x".to_string(),
                port: 0,
            }];
        });
        assert!(validate_confidential_agent_card(&card).is_err());
    }

    #[test]
    fn validate_card_rejects_empty_port_name() {
        let mut card = test_card();
        with_conf(&mut card, |c| {
            c.ports = vec![AgentCardPort {
                name: "".to_string(),
                port: 8080,
            }];
        });
        assert!(validate_confidential_agent_card(&card).is_err());
    }

    #[test]
    fn validate_card_rejects_empty_rekor_fields() {
        let mut card = test_card();
        with_conf(&mut card, |c| c.rekor.rekor_url = "".to_string());
        assert!(validate_confidential_agent_card(&card).is_err());

        let mut card = test_card();
        with_conf(&mut card, |c| c.rekor.artifact_id = "".to_string());
        assert!(validate_confidential_agent_card(&card).is_err());

        let mut card = test_card();
        with_conf(&mut card, |c| c.rekor.artifact_type = "".to_string());
        assert!(validate_confidential_agent_card(&card).is_err());

        let mut card = test_card();
        with_conf(&mut card, |c| c.rekor.rv_name = "".to_string());
        assert!(validate_confidential_agent_card(&card).is_err());
    }

    #[test]
    fn reference_values_from_rekor() {
        let card = test_card();
        let rv = agent_card_reference_values(&card).unwrap();
        let arr = rv.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "slsa");
        let content = &arr[0]["payload"]["content"];
        assert!(content["rv_list"].is_array());
    }

    #[test]
    fn reference_values_from_sample() {
        let mut card = test_card();
        with_conf(&mut card, |c| {
            c.reference_values = Some(json!({"tdx": {"mr_td": "abc123"}}));
        });
        let rv = agent_card_reference_values(&card).unwrap();
        let arr = rv.as_array().unwrap();
        assert_eq!(arr[0]["type"], "sample");
        assert_eq!(arr[0]["payload"]["content"]["tdx"]["mr_td"], "abc123");
    }

    #[test]
    fn derive_tng_config_structure() {
        let card = test_card();
        let config = derive_tng_client_config(&card).unwrap();
        assert!(config["control_interface"]["restful"]["port"].is_u64());
        let ingress = config["add_ingress"].as_array().unwrap();
        assert_eq!(ingress.len(), 1);
        assert_eq!(ingress[0]["mapping"]["out"]["host"], "1.2.3.4");
        assert_eq!(ingress[0]["mapping"]["out"]["port"], 18789);
        assert_eq!(ingress[0]["mapping"]["in"]["host"], "127.0.0.1");
        assert_eq!(ingress[0]["rats_tls"]["multiplex"], false);
        assert_eq!(
            ingress[0]["verify"]["attestation_policy"]["path"],
            DEFAULT_TNG_POLICY_PATH
        );
        assert!(ingress[0]["verify"].get("policy").is_none());
        assert!(ingress[0]["verify"].get("policy_ids").is_none());
    }

    #[test]
    fn derive_tng_config_with_custom_port_mapping() {
        let card = test_card();
        let config = derive_tng_client_config_with_local_ports(
            &card,
            |_| Ok(9999),
            50000,
            DEFAULT_TNG_POLICY_PATH,
        )
        .unwrap();
        let ingress = config["add_ingress"].as_array().unwrap();
        assert_eq!(ingress[0]["mapping"]["in"]["port"], 9999);
        assert_eq!(config["control_interface"]["restful"]["port"], 50000);
    }

    #[test]
    fn derive_tng_config_port_mapper_error_propagates() {
        let card = test_card();
        let result = derive_tng_client_config_with_local_ports(
            &card,
            |_| anyhow::bail!("port conflict"),
            50000,
            DEFAULT_TNG_POLICY_PATH,
        );
        assert!(result.is_err());
    }

    #[test]
    fn derive_tng_config_multi_port() {
        let mut card = test_card();
        with_conf(&mut card, |c| {
            c.ports = vec![
                AgentCardPort {
                    name: "api".to_string(),
                    port: 18789,
                },
                AgentCardPort {
                    name: "admin".to_string(),
                    port: 18790,
                },
            ];
        });
        let config = derive_tng_client_config(&card).unwrap();
        let ingress = config["add_ingress"].as_array().unwrap();
        assert_eq!(ingress.len(), 2);
    }

    #[test]
    fn validate_card_rejects_whitespace_name() {
        let mut card = test_card();
        card.name = "   ".to_string();
        assert!(validate_confidential_agent_card(&card).is_err());
    }

    #[test]
    fn validate_card_rejects_empty_id() {
        let mut card = test_card();
        with_conf(&mut card, |c| c.id = "".to_string());
        assert!(validate_confidential_agent_card(&card).is_err());
    }

    #[test]
    fn render_agent_card_rejects_disabled_a2a() {
        let spec = AgentSpec::from_yaml(
            &format!(
                "{}\n{}",
                r#"
schema: confidential-agent/v1
service:
  id: test
  ports: [18789]
  connect: [18789]
build:
  image_name: test-agent
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
attestation:
  tee: tdx
  mode: challenge
resources: {}"#
                    .trim(),
                r#"
a2a:
  enabled: false
  id: test-agent
  name: test-agent"#
            ),
            std::path::Path::new("/project"),
        )
        .unwrap();
        let meta = json!({
            "artifact_id": "test",
            "artifact_version": "v1",
            "artifact_type": "uki",
            "rekor_url": "https://rekor.sigstore.dev"
        });

        let err = render_agent_card(&spec, "1.2.3.4", &meta, None).unwrap_err();
        assert!(err.to_string().contains("a2a is disabled"));
    }
}
