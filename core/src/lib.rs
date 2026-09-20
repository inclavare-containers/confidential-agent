pub mod a2a;
pub mod agent_card;
pub mod agent_card_fetch;
pub mod agent_card_signing;
pub mod mesh;
pub mod peerings;
pub mod schema;
pub mod spec;
pub mod trustee;
pub mod util;

#[cfg(test)]
mod schema_tests {
    use crate::schema::{
        confidential_ports, BootstrapConfig, GatewayIdentity, GuestResource, LocalBuildState,
        LocalDebugSshKey, LocalDeployState, LocalGatewayIdentity, LocalServiceNetwork,
        LocalServiceState, LocalSpecState, MeshBundle, MeshService, BOOTSTRAP_SCHEMA_VERSION,
        LOCAL_SERVICE_STATE_SCHEMA_VERSION, MESH_SCHEMA_VERSION,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[test]
    fn local_service_state_round_trips_ports_and_connect() {
        let state = LocalServiceState {
            schema: LOCAL_SERVICE_STATE_SCHEMA_VERSION.to_string(),
            service_id: "openclaw".to_string(),
            generation: 1,
            phase: "active".to_string(),
            attestation_mode: "challenge".to_string(),
            spec: LocalSpecState {
                path: PathBuf::from("/project/openclaw.yaml"),
                sha256: "spec-hash".to_string(),
            },
            build: LocalBuildState {
                build_id: "openclaw-qwen-release".to_string(),
                image_name: "openclaw-qwen".to_string(),
                variant: "release".to_string(),
                image_path: PathBuf::from("/state/services/openclaw/shelter/images/openclaw-qwen-release/image-openclaw-qwen-release.qcow2"),
                images_dir: PathBuf::from("/state/services/openclaw/artifacts"),
                cache_dir: PathBuf::from("/state/services/openclaw/cache"),
                debug_ssh: Some(LocalDebugSshKey {
                    private_key: PathBuf::from("/state/services/openclaw/secrets/debug_ssh"),
                    public_key: PathBuf::from("/state/services/openclaw/secrets/debug_ssh.pub"),
                }),
                sample_rv: None,
                rekor_meta: None,
                remote: false,
                published: BTreeMap::new(),
            },
            deploy: LocalDeployState {
                provider: "aliyun".to_string(),
                run_id: "20260429201011".to_string(),
                resource_name: "openclaw-20260429201011".to_string(),
                terraform_dir: None,
                image_source: None,
                image_import_name: None,
                bucket: None,
                instance_id: Some("i-xxx".to_string()),
                security_group_id: None,
                private_ip: Some("10.0.0.8".to_string()),
                public_ip: Some("1.2.3.4".to_string()),
                tee: "tdx".to_string(),
                published_image_id: None,
                instance_ids: Vec::new(),
                instance_private_ips: Vec::new(),
                slb_vip: None,
            },
            service: LocalServiceNetwork {
                ports: vec![18789, 18800],
                connect: vec![crate::spec::ConnectPort::Plain(18789)],
                mcp_ports: vec![18800],
            },
            gateway_identity: Some(LocalGatewayIdentity {
                public_key: "pub".to_string(),
                private_key_path: PathBuf::from("/state/services/openclaw/secrets/gateway_identity.seed"),
            }),
            resources: BTreeMap::new(),
            mesh_generation: 1,
            reference_values: "rekor".to_string(),
        };

        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: LocalServiceState = serde_json::from_str(&encoded).unwrap();
        let mut legacy: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        legacy.as_object_mut().unwrap().remove("attestation_mode");
        let legacy: LocalServiceState = serde_json::from_value(legacy).unwrap();

        assert_eq!(decoded.attestation_mode, "challenge");
        assert_eq!(legacy.attestation_mode, "challenge");
        assert_eq!(decoded.service.ports, vec![18789, 18800]);
        assert_eq!(decoded.service.connect, vec![crate::spec::ConnectPort::Plain(18789)]);
        assert_eq!(decoded.service.mcp_ports, vec![18800]);
        assert_eq!(decoded.gateway_identity.unwrap().public_key, "pub");
        assert_eq!(decoded.deploy.private_mesh_ip(), Some("10.0.0.8"));
        assert_eq!(
            decoded.build.debug_ssh.unwrap().private_key,
            PathBuf::from("/state/services/openclaw/secrets/debug_ssh")
        );
    }

    #[test]
    fn mesh_bundle_round_trips_service_ports() {
        let bundle = MeshBundle {
            schema: MESH_SCHEMA_VERSION.to_string(),
            generation: 1,
            updated_at: 1700000000,
            services: BTreeMap::from([(
                "svc-a".to_string(),
                MeshService {
                    phase: "active".to_string(),
                    private_ip: Some("10.0.0.7".to_string()),
                    public_ip: Some("1.2.3.4".to_string()),
                    ports: vec![18789, 18800],
                    connect: vec![crate::spec::ConnectPort::Plain(18789)],
                    mcp_ports: vec![18800],
                    gateway_public_key: Some("pub".to_string()),
                },
            )]),
            reference_values: BTreeMap::new(),
            rekor_reference_values: BTreeMap::new(),
        };

        let encoded = serde_json::to_string(&bundle).unwrap();
        let decoded: MeshBundle = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded.services["svc-a"].ports, vec![18789, 18800]);
        assert_eq!(decoded.services["svc-a"].connect, vec![crate::spec::ConnectPort::Plain(18789)]);
        assert_eq!(decoded.services["svc-a"].mcp_ports, vec![18800]);
        assert_eq!(
            decoded.services["svc-a"].gateway_public_key.as_deref(),
            Some("pub")
        );
    }

    #[test]
    fn confidential_ports_are_ports_minus_connect() {
        assert_eq!(
            confidential_ports(&[18789, 3001, 3001, 18800], &[crate::spec::ConnectPort::Plain(18789)]),
            vec![3001, 18800]
        );
        assert!(confidential_ports(&[18789], &[crate::spec::ConnectPort::Plain(18789)]).is_empty());
    }

    #[test]
    fn bootstrap_config_round_trips_resources() {
        let bootstrap = BootstrapConfig {
            schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
            generation: 3,
            service_id: "svc-a".to_string(),
            mode: "challenge".to_string(),
            ports: vec![18789, 18800],
            connect: vec![crate::spec::ConnectPort::Plain(18789)],
            mcp_ports: vec![18800],
            gateway_identity: Some(GatewayIdentity {
                public_key: "pub".to_string(),
                private_key: "priv".to_string(),
            }),
            resources: vec![GuestResource {
                id: "config".to_string(),
                resource_path: "default/local-resources/config".to_string(),
                target: PathBuf::from("/etc/app/config.json"),
                owner: None,
                group: None,
                mode: "0600".to_string(),
                required: true,
                mutable: false,
                sha256: Some("abc".to_string()),
            }],
            app_service: Some("app.service".to_string()),
            peers: Vec::new(),
            agent_card: None,
            tng_peers: Vec::new(),
        };

        let encoded = serde_json::to_string(&bootstrap).unwrap();
        let decoded: BootstrapConfig = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded.generation, 3);
        assert_eq!(
            decoded.resources[0].resource_path,
            "default/local-resources/config"
        );
        assert!(decoded.resources[0].required);
        assert_eq!(decoded.app_service.as_deref(), Some("app.service"));
    }

    #[test]
    fn bootstrap_config_round_trips_peers_and_agent_card() {
        use crate::agent_card::{confidential_extension, CONFIDENTIAL_AGENT_EXTENSION};
        use crate::schema::{
            AgentCard, AgentCardCapabilities, AgentCardConfidential, AgentCardPort, AgentCardRekor,
            AgentCardSkill, AgentExtension, AgentInterface, BootstrapPeer,
            BootstrapPeerPortMapping,
        };

        let bootstrap = BootstrapConfig {
            schema: BOOTSTRAP_SCHEMA_VERSION.to_string(),
            generation: 5,
            service_id: "svc-a".to_string(),
            mode: "challenge".to_string(),
            ports: vec![18789],
            connect: vec![crate::spec::ConnectPort::Plain(18789)],
            mcp_ports: Vec::new(),
            gateway_identity: None,
            resources: Vec::new(),
            app_service: None,
            peers: vec![BootstrapPeer {
                id: "remote-agent".to_string(),
                url: "http://1.2.3.4:8089/.well-known/agent-card.json".to_string(),
                policy: "required".to_string(),
                refresh_interval_sec: 60,
                ports: vec![3001],
                port_mappings: vec![BootstrapPeerPortMapping {
                    remote: 18789,
                    local: 18790,
                }],
            }],
            agent_card: Some(AgentCard {
                protocol_version: "1.0".to_string(),
                name: "test-agent".to_string(),
                description: "A test agent".to_string(),
                version: Some("1.0.0".to_string()),
                supported_interfaces: vec![AgentInterface {
                    url: "http://1.2.3.4:18789/a2a".to_string(),
                    protocol_binding: "JSONRPC".to_string(),
                    protocol_version: "1.0".to_string(),
                    tenant: None,
                }],
                preferred_transport: Some("JSONRPC".to_string()),
                skills: vec![AgentCardSkill {
                    id: "chat".to_string(),
                    name: "Chat".to_string(),
                    description: None,
                    tags: Vec::new(),
                    examples: Vec::new(),
                    input_modes: Vec::new(),
                    output_modes: Vec::new(),
                }],
                default_input_modes: vec!["text".to_string()],
                default_output_modes: vec!["text".to_string()],
                capabilities: AgentCardCapabilities {
                    extensions: vec![AgentExtension {
                        uri: CONFIDENTIAL_AGENT_EXTENSION.to_string(),
                        description: None,
                        required: true,
                        params: serde_json::to_value(AgentCardConfidential {
                            id: "test-agent".to_string(),
                            cache_ttl_sec: 300,
                            public_ip: "1.2.3.4".to_string(),
                            ports: vec![AgentCardPort {
                                name: "chat".to_string(),
                                port: 18789,
                            }],
                            reference_values: None,
                            rekor: AgentCardRekor {
                                rekor_url: "https://rekor.sigstore.dev".to_string(),
                                artifact_id: "test-agent-release".to_string(),
                                artifact_type: "uki".to_string(),
                                artifact_version: "20260512".to_string(),
                                rv_name: "measurement.uki.SHA-384".to_string(),
                            },
                            tee: "tdx".to_string(),
                        })
                        .unwrap(),
                    }],
                    ..Default::default()
                },
                provider: None,
                security_schemes: None,
                security: Vec::new(),
                supports_authenticated_extended_card: Some(false),
                signatures: Vec::new(),
            }),
            tng_peers: Vec::new(),
        };

        let encoded = serde_json::to_string(&bootstrap).unwrap();
        let decoded: BootstrapConfig = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded.peers.len(), 1);
        assert_eq!(decoded.peers[0].id, "remote-agent");
        assert_eq!(decoded.peers[0].policy, "required");
        assert_eq!(decoded.peers[0].ports, vec![3001]);
        assert_eq!(decoded.peers[0].port_mappings[0].remote, 18789);
        assert_eq!(decoded.peers[0].port_mappings[0].local, 18790);

        let card = decoded.agent_card.unwrap();
        assert_eq!(card.name, "test-agent");
        let confidential = confidential_extension(&card).unwrap();
        assert_eq!(confidential.tee, "tdx");
        assert_eq!(confidential.id, "test-agent");
        assert_eq!(confidential.public_ip, "1.2.3.4");
        assert_eq!(confidential.ports[0].port, 18789);
        let rekor = confidential.rekor;
        assert_eq!(rekor.artifact_id, "test-agent-release");
    }
}

#[cfg(test)]
mod util_tests {
    use crate::util::{rekor_payload, sha256_file};
    use serde_json::json;
    use std::fs;

    #[test]
    fn rekor_payload_accepts_existing_rv_list() {
        let metadata = json!({"rv_list": [{"id": "image"}]});

        assert_eq!(rekor_payload(&metadata).unwrap(), metadata);
    }

    #[test]
    fn rekor_payload_builds_trustee_rv_list_from_metadata() {
        let payload = rekor_payload(&json!({
            "artifact_id": "openclaw-agent-release",
            "artifact_version": "20260430",
            "artifact_type": "application/vnd.confidential-agent.image",
            "rekor_url": "https://rekor.example",
            "rekor_api_version": 1,
            "rv_name": "openclaw-sample",
            "provenance_source": {"type": "local"}
        }))
        .unwrap();

        assert_eq!(payload["rv_list"][0]["id"], "openclaw-agent-release");
        assert_eq!(payload["rv_list"][0]["operation_type"], "add");
        assert_eq!(payload["rv_list"][0]["rv_name"], "openclaw-sample");
        assert_eq!(
            payload["rv_list"][0]["provenance_info"]["type"],
            "slsa-intoto-statements"
        );
    }

    #[test]
    fn sha256_file_returns_lower_hex_digest() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload");
        fs::write(&path, "abc").unwrap();

        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
