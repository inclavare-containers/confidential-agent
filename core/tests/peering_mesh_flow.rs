use confidential_agent_core::mesh::render_mesh_bundle_at;
use confidential_agent_core::peerings::{
    ipv4_cidr_contains, validate_ipv4_cidr, PeeringEntry, PeeringRole, PeeringScope, PeeringsFile,
};
use confidential_agent_core::schema::{
    LocalBuildState, LocalDeployState, LocalGatewayIdentity, LocalServiceNetwork,
    LocalServiceState, LocalSpecState,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn operator(label: &str, cidr: &str) -> PeeringEntry {
    PeeringEntry {
        label: label.to_string(),
        role: PeeringRole::Operator,
        cidr: cidr.to_string(),
        scope: Vec::new(),
        note: None,
        added_at: None,
        added_by: None,
    }
}

fn peer(label: &str, cidr: &str) -> PeeringEntry {
    PeeringEntry {
        label: label.to_string(),
        role: PeeringRole::Peer,
        cidr: cidr.to_string(),
        scope: Vec::new(),
        note: None,
        added_at: None,
        added_by: None,
    }
}

fn service_state(
    id: &str,
    phase: &str,
    public_ip: &str,
    ports: Vec<u16>,
    connect: Vec<confidential_agent_core::spec::ConnectPort>,
) -> LocalServiceState {
    LocalServiceState {
        schema: "confidential-agent/service-state/v1".to_string(),
        service_id: id.to_string(),
        generation: 1,
        phase: phase.to_string(),
        attestation_mode: "challenge".to_string(),
        spec: LocalSpecState {
            path: PathBuf::from("/project/agent.yaml"),
            sha256: "spec-hash".to_string(),
        },
        build: LocalBuildState {
            build_id: "build-1".to_string(),
            image_name: format!("{id}-agent"),
            variant: "release".to_string(),
            image_path: PathBuf::from("/state/image.qcow2"),
            images_dir: PathBuf::from("/state/images"),
            cache_dir: PathBuf::from("/state/cache"),
            debug_ssh: None,
            sample_rv: None,
            rekor_meta: None,
            remote: false,
            published: BTreeMap::new(),
        },
        deploy: LocalDeployState {
            provider: "aliyun".to_string(),
            run_id: "run-1".to_string(),
            resource_name: format!("{id}-resource"),
            terraform_dir: None,
            image_source: None,
            image_import_name: None,
            bucket: None,
            instance_id: Some(format!("i-{id}")),
            security_group_id: None,
            private_ip: Some(format!("10.0.0.{}", if id == "alpha" { 1 } else { 2 })),
            public_ip: Some(public_ip.to_string()),
            tee: "tdx".to_string(),
            published_image_id: None,
            instance_ids: Vec::new(),
            instance_private_ips: Vec::new(),
            slb_vip: None,
        },
        service: LocalServiceNetwork {
            ports,
            connect,
            mcp_ports: Vec::new(),
        },
        gateway_identity: Some(LocalGatewayIdentity {
            public_key: format!("{id}-pub"),
            private_key_path: PathBuf::from(format!(
                "/state/services/{id}/secrets/gateway_identity.seed"
            )),
        }),
        resources: BTreeMap::new(),
        mesh_generation: 1,
        reference_values: "sample".to_string(),
    }
}

#[test]
fn peering_lifecycle_create_validate_write_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peerings.yaml");

    let mut file = PeeringsFile::empty();
    assert!(file.validate().is_ok());

    file.peerings.push(operator("ops", "203.0.113.0/24"));
    file.peerings.push(peer("beta", "198.51.100.10/32"));
    file.validate().unwrap();

    file.write_to_path(&path).unwrap();
    let loaded = PeeringsFile::from_path(&path).unwrap();

    assert_eq!(loaded.peerings.len(), 2);
    assert!(loaded.has_operator_control_status());

    let control_cidrs = loaded.cidrs_for_scope(PeeringScope::Control);
    assert_eq!(control_cidrs, vec!["203.0.113.0/24"]);

    let connect_cidrs = loaded.cidrs_for_scope(PeeringScope::Connect);
    assert_eq!(connect_cidrs.len(), 2);

    assert!(loaded
        .control_cidrs_contain("203.0.113.50".parse().unwrap())
        .unwrap());
    assert!(!loaded
        .control_cidrs_contain("10.0.0.1".parse().unwrap())
        .unwrap());
}

#[test]
fn peering_scope_isolation() {
    let mut ops = operator("ops", "10.0.0.0/8");
    ops.scope = vec![PeeringScope::Status, PeeringScope::Ssh];

    let file = PeeringsFile {
        version: 1,
        peerings: vec![ops, peer("beta", "172.16.0.0/12")],
    };
    file.validate().unwrap();

    assert!(!file.has_operator_control_status());
    assert!(file.cidrs_for_scope(PeeringScope::Control).is_empty());
    assert_eq!(file.cidrs_for_scope(PeeringScope::Ssh), vec!["10.0.0.0/8"]);
    assert_eq!(
        file.cidrs_for_scope(PeeringScope::AgentCard),
        vec!["172.16.0.0/12"]
    );
}

#[test]
fn mesh_bundle_contains_only_active_services_and_their_reference_values() {
    let states = vec![
        service_state("alpha", "active", "1.2.3.4", vec![18789], vec![confidential_agent_core::spec::ConnectPort::Plain(18789)]),
        service_state("beta", "deleted", "5.6.7.8", vec![9090], vec![confidential_agent_core::spec::ConnectPort::Plain(9090)]),
        service_state("gamma", "active", "9.9.9.9", vec![18800], vec![]),
    ];
    let sample = BTreeMap::from([
        (
            "alpha".to_string(),
            serde_json::json!({"tdx": {"mr_td": "abc"}}),
        ),
        (
            "beta".to_string(),
            serde_json::json!({"tdx": {"mr_td": "stale"}}),
        ),
    ]);
    let rekor = BTreeMap::from([
        (
            "beta".to_string(),
            serde_json::json!({
                "artifact_id": "beta-agent",
                "artifact_version": "v1",
                "artifact_type": "uki",
                "rekor_url": "https://rekor.example.com"
            }),
        ),
        (
            "gamma".to_string(),
            serde_json::json!({
                "artifact_id": "gamma-agent",
                "artifact_version": "v1",
                "artifact_type": "uki",
                "rekor_url": "https://rekor.example.com"
            }),
        ),
    ]);

    let bundle = render_mesh_bundle_at(&states, &sample, &rekor, 3, 1716000000);

    assert_eq!(bundle.generation, 3);
    assert_eq!(bundle.updated_at, 1716000000);
    assert_eq!(
        bundle.services.keys().cloned().collect::<Vec<_>>(),
        vec!["alpha".to_string(), "gamma".to_string()]
    );

    let alpha = &bundle.services["alpha"];
    assert_eq!(alpha.ports, vec![18789]);
    assert_eq!(alpha.connect, vec![confidential_agent_core::spec::ConnectPort::Plain(18789)]);

    assert!(bundle.reference_values.contains_key("alpha"));
    assert!(!bundle.reference_values.contains_key("beta"));
    assert!(bundle.rekor_reference_values.contains_key("gamma"));
    assert!(!bundle.rekor_reference_values.contains_key("beta"));
}

#[test]
fn peering_firewall_rules_cover_all_peer_ips() {
    let file = PeeringsFile {
        version: 1,
        peerings: vec![
            operator("ops", "203.0.113.0/24"),
            peer("alpha", "47.93.100.200/32"),
            peer("beta", "47.93.100.201/32"),
        ],
    };
    file.validate().unwrap();

    let mesh_cidrs = file.cidrs_for_scope(PeeringScope::Mesh);
    assert!(mesh_cidrs.is_empty());

    let agent_card_cidrs = file.cidrs_for_scope(PeeringScope::AgentCard);
    assert_eq!(agent_card_cidrs.len(), 3);

    for cidr in &agent_card_cidrs {
        validate_ipv4_cidr("test", cidr).unwrap();
    }

    assert!(ipv4_cidr_contains("47.93.100.200/32", "47.93.100.200".parse().unwrap()).unwrap());
    assert!(!ipv4_cidr_contains("47.93.100.200/32", "47.93.100.201".parse().unwrap()).unwrap());
}
