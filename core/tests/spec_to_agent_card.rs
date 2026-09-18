use confidential_agent_core::agent_card::{
    agent_card_reference_values, confidential_extension, derive_tng_client_config,
    render_agent_card, validate_confidential_agent_card, validate_id,
};
use confidential_agent_core::spec::AgentSpec;
use confidential_agent_core::util::rekor_payload;
use serde_json::json;
use std::path::Path;

const FULL_SPEC: &str = r#"
schema: confidential-agent/v1
service:
  id: openclaw
  ports: [18789, 18800]
  connect: [18789]
  app_service: cai-openclaw-gateway.service
build:
  base_image: ./base.qcow2
  image_name: openclaw-agent
  resize: 30G
  with_network: true
  packages: [nodejs]
  scripts: [./install-openclaw.sh]
  files:
    - source: ./skill.md
      target: /usr/local/share/confidential-agent/openclaw/skill.md
  variants:
    release:
      enabled: true
    debug:
      enabled: true
      ssh_public_key: ./secrets/debug_ssh.pub
deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g8i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-l
attestation:
  tee: tdx
  mode: challenge
  reference_values: rekor
  rekor:
    artifact_id: openclaw-agent
    artifact_type: uki
    slsa_generator: ./slsa-gen
a2a:
  enabled: true
  id: openclaw-agent
  name: OpenClaw Alpha
  description: A confidential OpenClaw agent
  skills:
    - id: chat
      name: Chat
      description: General chat
resources: {}
"#;

fn rekor_meta() -> serde_json::Value {
    json!({
        "rekor_url": "https://rekor.sigstore.dev",
        "artifact_id": "openclaw-agent",
        "artifact_type": "uki",
        "artifact_version": "v0.1.0-20260520-build1",
        "rv_name": "openclaw"
    })
}

#[test]
fn spec_parse_to_card_to_tng_config() {
    let spec = AgentSpec::from_yaml(FULL_SPEC, Path::new("/project")).unwrap();
    assert_eq!(spec.service.id, "openclaw");
    assert_eq!(spec.service.connect, vec![confidential_agent_core::spec::ConnectPort::Plain(18789)]);

    let a2a = spec.a2a.as_ref().unwrap();
    assert!(a2a.enabled);
    validate_id("a2a.id", &a2a.id).unwrap();

    let card = render_agent_card(&spec, "47.93.100.200", &rekor_meta(), None).unwrap();
    validate_confidential_agent_card(&card).unwrap();
    assert_eq!(card.name, "OpenClaw Alpha");
    assert_eq!(card.skills.len(), 1);
    assert_eq!(card.skills[0].id, "chat");
    assert_eq!(
        card.supported_interfaces[0].url,
        "http://47.93.100.200:18789/a2a"
    );

    let ext = confidential_extension(&card).unwrap();
    assert_eq!(ext.ports.len(), 1);
    assert_eq!(ext.ports[0].port, 18789);

    let rv = agent_card_reference_values(&card).unwrap();
    let rv_arr = rv.as_array().unwrap();
    assert_eq!(rv_arr.len(), 1);
    assert_eq!(rv_arr[0]["type"], "slsa");
    let content = &rv_arr[0]["payload"]["content"];
    assert!(content["rv_list"].is_array());

    let tng_config = derive_tng_client_config(&card).unwrap();
    assert!(tng_config["control_interface"]["restful"]["port"].is_u64());
    let ingress = tng_config["add_ingress"].as_array().unwrap();
    assert_eq!(ingress.len(), 1);
    assert_eq!(ingress[0]["mapping"]["out"]["host"], "47.93.100.200");
    assert_eq!(ingress[0]["mapping"]["out"]["port"], 18789);
    assert!(ingress[0]["verify"]["reference_values"].is_array());
    assert_eq!(ingress[0]["rats_tls"]["multiplex"], false);
    assert_eq!(ingress[0]["verify"]["attestation_policy"]["type"], "path");
    assert!(ingress[0]["verify"].get("policy").is_none());
    assert!(ingress[0]["verify"].get("policy_ids").is_none());
}

#[test]
fn spec_with_sample_rv_produces_inline_reference_values() {
    let spec = AgentSpec::from_yaml(FULL_SPEC, Path::new("/project")).unwrap();
    let sample = json!({"tdx": {"mr_td": "abc123def456"}});
    let card = render_agent_card(&spec, "1.2.3.4", &rekor_meta(), Some(&sample)).unwrap();

    validate_confidential_agent_card(&card).unwrap();

    let rv = agent_card_reference_values(&card).unwrap();
    let arr = rv.as_array().unwrap();
    assert_eq!(arr[0]["type"], "sample");
    assert_eq!(arr[0]["payload"]["type"], "inline");
    assert_eq!(arr[0]["payload"]["content"]["tdx"]["mr_td"], "abc123def456");
}

#[test]
fn rekor_metadata_round_trips_through_payload() {
    let meta = json!({
        "artifact_id": "openclaw-agent",
        "artifact_version": "v0.1.0-build42",
        "artifact_type": "uki",
        "rekor_url": "https://rekor.sigstore.dev",
    });
    let payload = rekor_payload(&meta).unwrap();
    let rv_list = payload["rv_list"].as_array().unwrap();
    assert_eq!(rv_list.len(), 1);
    assert_eq!(rv_list[0]["id"], "openclaw-agent");
    assert_eq!(rv_list[0]["type"], "uki");
    assert_eq!(
        rv_list[0]["provenance_info"]["type"],
        "slsa-intoto-statements"
    );
}
