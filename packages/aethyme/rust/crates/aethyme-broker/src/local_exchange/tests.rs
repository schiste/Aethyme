use super::*;
fn sha(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}
fn packet() -> Metadata {
    Metadata::Contribution {
        contribution: sha('a'),
        source: sha('b'),
        availability: SourceAvailability::LocalOnly,
        scope: vec!["src/lib.rs".into()],
        brief: Some("untrusted text".into()),
    }
}
fn stream() -> StreamWitness {
    StreamWitness {
        binding: sha('a'),
        reader: "agent-one".into(),
        scope_digest: sha('b'),
        visibility_generation: "acl-1".into(),
        incarnation: "stream-1".into(),
        remote_authority_epoch: "epoch-1".into(),
    }
}
fn peer() -> PeerSpec {
    PeerSpec {
        profile: PROFILE.into(),
        service_id: "coord.example".into(),
        project: "Playground".into(),
        audience: "project-readers".into(),
        reader: "agent-one".into(),
        scope: Scope::Paths(vec!["src/lib.rs".into()]),
        max_attempts: 3,
        deadline_seconds: 5,
    }
}
#[test]
fn independent_packet_vector_binds_source_and_attributed_audience() {
    let vector: serde_json::Value =
        serde_json::from_str(include_str!("packet-vector.json")).unwrap();
    let bytes = vector["canonical"].as_str().unwrap().as_bytes();
    let value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    let packet: Metadata = serde_json::from_value(value["spec"].clone()).unwrap();
    let record = packet.record().unwrap();
    assert_eq!(record.canonical_bytes(), bytes);
    assert_eq!(record.id().as_str(), vector["id"].as_str().unwrap());
    for key in ["contribution", "source", "brief"] {
        let mut other = value.clone();
        other["spec"][key] = "changed".into();
        assert_ne!(
            Record::decode(&serde_json::to_vec(&other).unwrap(), &[&PACKET])
                .unwrap()
                .id(),
            record.id()
        );
    }
    let mut other = value;
    other["spec"]["availability"]["audience"] = "different-readers".into();
    let other: Metadata = serde_json::from_value(other["spec"].clone()).unwrap();
    assert_ne!(other.record().unwrap().id(), record.id());
}
#[test]
fn unknown_source_availability_never_becomes_a_retained_receipt() {
    let mut value = serde_json::to_value(packet()).unwrap();
    value["availability"] = serde_json::json!({"kind":"verified_and_adopted"});
    assert!(serde_json::from_value::<Metadata>(value).is_err());
    assert_eq!(DATA_ROLE, "untrusted_advisory_metadata");
    let offered = Metadata::GenerationOffer {
        generation: sha('a'),
        prepared_receipt: sha('b'),
        source: sha('c'),
        pending_contributions: vec![sha('d')],
    };
    assert!(offered.record().is_ok());
    // A valid offer encodes identifiers; it carries no authorization lease.
    let encoded = serde_json::to_value(offered).unwrap();
    assert!(encoded.get("authorization").is_none());
    assert!(encoded.get("adopted").is_none());
}
#[test]
fn malformed_identifiers_scope_and_duplicate_pending_work_are_refused() {
    for bad in [
        "",
        "../private",
        "/absolute",
        "src/./x",
        "src//x",
        "src\\x",
        "src\nsecret",
    ] {
        let mut p = packet();
        if let Metadata::Contribution { scope, .. } = &mut p {
            *scope = vec![bad.into()];
        }
        assert!(p.record().is_err(), "{bad:?}");
    }
    let mut p = packet();
    if let Metadata::Contribution { source, .. } = &mut p {
        *source = "guessed".into();
    }
    assert!(p.record().is_err());
    let mut p = packet();
    if let Metadata::Contribution { scope, .. } = &mut p {
        scope.push(scope[0].clone());
    }
    assert!(p.record().is_err());
    let offered = Metadata::GenerationOffer {
        generation: sha('a'),
        prepared_receipt: sha('b'),
        source: sha('c'),
        pending_contributions: vec![sha('d'), sha('d')],
    };
    assert!(offered.record().is_err());
}
#[test]
fn bounded_packets_and_exact_provider_source_claims_are_required() {
    let mut p = packet();
    if let Metadata::Contribution { brief, .. } = &mut p {
        *brief = Some("x".repeat(8193));
    }
    assert!(matches!(p.record(), Err(ExchangeError::Limit)));
    let mut p = packet();
    if let Metadata::Contribution { availability, .. } = &mut p {
        *availability = SourceAvailability::ProviderReachable {
            provider: "github.com".into(),
            exact_commit: "refs/heads/main".into(),
        };
    }
    assert!(p.record().is_err());
    if let Metadata::Contribution { availability, .. } = &mut p {
        *availability = SourceAvailability::ProviderReachable {
            provider: "github.com".into(),
            exact_commit: "a".repeat(40),
        };
    }
    assert!(p.record().is_ok());
}
#[test]
fn reader_scope_visibility_and_stream_incarnation_bind_event_identity() {
    let event = Event {
        stream: stream(),
        sequence: 1,
        packet: packet(),
    };
    let record = event.record().unwrap();
    for key in [
        "binding",
        "reader",
        "scope_digest",
        "visibility_generation",
        "incarnation",
        "remote_authority_epoch",
    ] {
        let mut v = serde_json::to_value(&event).unwrap();
        v["stream"][key] = if key == "binding" || key == "scope_digest" {
            sha('c')
        } else {
            "different".into()
        }
        .into();
        let other: Event = serde_json::from_value(v).unwrap();
        assert_ne!(other.record().unwrap().id(), record.id(), "{key}");
    }
    for sequence in [0, MAX_STREAM_POSITION + 1] {
        let mut other = event.clone();
        other.sequence = sequence;
        assert!(other.record().is_err());
    }
    let mut other = event;
    other.stream.incarnation.clear();
    assert!(other.record().is_err());
}
#[test]
fn finite_peer_limits_and_supported_profile_are_explicit() {
    assert!(peer().validate().is_ok());
    for attempts in [0, 21] {
        let mut p = peer();
        p.max_attempts = attempts;
        assert!(p.validate().is_err());
    }
    for seconds in [0, 31] {
        let mut p = peer();
        p.deadline_seconds = seconds;
        assert!(p.validate().is_err());
    }
    let mut p = peer();
    p.profile = "automatic-migration".into();
    assert!(matches!(p.validate(), Err(ExchangeError::Unsupported)));
    let mut p = peer();
    p.scope = Scope::Paths(vec![]);
    assert!(p.validate().is_err());
}
