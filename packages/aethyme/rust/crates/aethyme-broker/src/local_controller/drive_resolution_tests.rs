use super::*;
use crate::local_target::tests::Fixture;
use serde_json::{Value, json};

fn failed_gate(name: &str, class: &str, definition: &str) -> Value {
    json!({
        "gate": name,
        "status": "fail",
        "host_fault": false,
        "failure_class": class,
        "definition_hash": definition,
    })
}

#[test]
fn native_resolution_ids_require_complete_current_trusted_failures() {
    let hash = "a".repeat(64);
    let observation = json!({
        "current_authority": true,
        "status": "failed",
        "gates": [
            {"gate":"pass", "status":"pass"},
            failed_gate("unit tests", "test_failure", &hash),
            failed_gate("cached", "cached_prior_fail", &"b".repeat(64)),
        ],
    });
    let checks = trusted_native_failed_checks(&observation);
    assert_eq!(checks.len(), 2);
    assert_eq!(checks[0].check, format!("native-gate:unit tests:{hash}"));
    assert_eq!(
        checks[1].check,
        format!("native-gate:cached:{}", "b".repeat(64))
    );
}

#[test]
fn native_resolution_refuses_infrastructure_stale_or_incomplete_failure_ids() {
    let hash = "c".repeat(64);
    let valid = failed_gate("unit", "test_failure", &hash);
    let invalid = failed_gate("bad", "test_failure", "not-a-definition-hash");
    let observations = [
        json!({"current_authority":false,"status":"failed","gates":[valid.clone()]}),
        json!({"current_authority":true,"status":"deferred","gates":[valid.clone()]}),
        json!({"current_authority":true,"status":"failed","gates":[
            valid.clone(),
            {"gate":"host","status":"fail","host_fault":true,"failure_class":"test_failure","definition_hash":hash},
        ]}),
        json!({"current_authority":true,"status":"failed","gates":[
            valid.clone(),
            {"gate":"infra","status":"error","host_fault":false,"failure_class":"environment","definition_hash":hash},
        ]}),
        json!({"current_authority":true,"status":"failed","gates":[valid,invalid]}),
        json!({"current_authority":true,"status":"failed","gates":[]}),
    ];
    for observation in observations {
        assert!(
            trusted_native_failed_checks(&observation).is_empty(),
            "{observation}"
        );
    }
}

fn minimal_policy() -> DrivePolicy {
    let limits = snapshot::ResourceLimits {
        timeout_ms: 1000,
        memory_bytes: 64 * 1024 * 1024,
        output_bytes: 1024 * 1024,
        pids: 32,
    };
    let command = snapshot::CommandSpec {
        program: "/bin/true".into(),
        arguments: Vec::new(),
        environment: Default::default(),
    };
    let digest = |bytes: &[u8]| format!("sha256:{}", ObjectDigest::of(bytes).hex());
    DrivePolicy {
        execution: ExecutionSpec {
            candidate_source: digest(b"candidate"),
            preparation: None,
            dependencies: None,
            configuration: None,
            harness: digest(b"harness"),
            dataset: None,
            assertion_inventory: digest(b"inventory"),
            materialization_profile: snapshot::MATERIALIZATION_PROFILE.into(),
            profile: snapshot::LinuxProfile {
                build_image: digest(b"image"),
                verifier_image: digest(b"image"),
                architecture: "x86_64".into(),
                runtime_profile: digest(b"runtime"),
                build_limits: limits,
                verifier_limits: limits,
            },
            build: command.clone(),
            verify: command,
        },
        wave_policy: wave::WavePolicy::default(),
        max_wall_ms: 1000,
        allow_empty_native_gates: true,
    }
}

#[test]
fn resumed_legacy_wave_without_source_is_held_without_creating_a_request() {
    let mut fixture = Fixture::new();
    let owner =
        crate::local_target::acquire_controller(&mut fixture.store, "target", "appointed").unwrap();
    let mut target_spec = fixture.spec();
    target_spec.selected = vec!["A".into()];
    let revision =
        crate::local_target::propose_revision(&mut fixture.store, &owner, target_spec).unwrap();
    crate::local_target::select_revision(&mut fixture.store, &owner, &revision.id).unwrap();
    wave::available(&mut fixture.store, &owner, "A", 0).unwrap();
    let pending = wave::admit(
        &mut fixture.store,
        &owner,
        wave::WavePolicy {
            debounce_ms: 0,
            ..Default::default()
        },
        0,
    )
    .unwrap();
    let pending = match pending {
        wave::Admission::Frozen(wave) => wave,
        other => panic!("expected frozen wave, got {other:?}"),
    };
    let composed = wave::compose_with_source(
        &mut fixture.store,
        &owner,
        fixture._repo.path(),
        &pending.id,
    )
    .unwrap();
    fixture
        .store
        .connection()
        .execute(
            "UPDATE local_waves SET composition_source_record_id=NULL, composition_source_object=NULL WHERE wave=?1",
            [pending.id.as_str()],
        )
        .unwrap();
    let resumed = wave::compose_with_source(
        &mut fixture.store,
        &owner,
        fixture._repo.path(),
        &pending.id,
    )
    .unwrap();
    assert!(resumed.source.is_none());

    let spec = RunSpec {
        target: owner.target().into(),
        revision: revision.id.to_string(),
        epoch: owner.authority_epoch().as_str().into(),
        wave: pending.id.to_string(),
        policy: minimal_policy(),
        deadline_ms: i64::MAX,
    };
    let reason = raise_failed_check_request(
        &mut fixture.store,
        &owner,
        fixture._repo.path(),
        &spec,
        &pending.id,
        &AtomicBool::new(false),
        resumed.source.as_ref(),
        vec![FailedCheck {
            check:
                "native-gate:unit:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .into(),
        }],
    )
    .unwrap();
    assert_eq!(
        reason,
        Some("legacy wave has no retained composition context for resolution")
    );
    assert!(resumed.wave.resolution_request.is_none());
    assert_eq!(
        wave::load(&fixture.store, &pending.id)
            .unwrap()
            .resolution_request,
        None
    );
    assert_eq!(composed.wave.phase, wave::WavePhase::Verifying);
}
