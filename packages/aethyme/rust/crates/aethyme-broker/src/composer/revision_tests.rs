use super::*;

fn composition_request(deliveries: Vec<String>) -> CompositionRequest {
    CompositionRequest {
        baseline: CommitOid::parse(&"0".repeat(40)).expect("commit id"),
        accepted: Vec::new(),
        catalog: vec![ContributionSpec {
            id: "A".into(),
            lineage: None,
            requires: Vec::new(),
            atomic_group: None,
            revision_of: None,
            derived_from: Vec::new(),
        }],
        deliveries,
        budget: CompositionBudget::default(),
    }
}

fn revision_id(digit: char) -> RecordId {
    RecordId::parse(&format!("sha256:{}", digit.to_string().repeat(64)))
        .expect("record id")
}

#[test]
fn unbound_recipe_order_keeps_the_legacy_shape() {
    let order = recipe_order_entry(
        "A",
        "sha256:lineage",
        "sha256:base",
        "sha256:result",
        None,
    );
    assert_eq!(
        order,
        object(vec![
            ("id", text("A")),
            ("lineage", text("sha256:lineage")),
            ("base", text("sha256:base")),
            ("result", text("sha256:result")),
        ])
    );
}

#[test]
fn bound_recipe_order_carries_the_exact_immutable_revision_id() {
    let order = recipe_order_entry(
        "A",
        "sha256:lineage",
        "sha256:base",
        "sha256:result",
        Some(revision_id('a').as_str()),
    );
    assert_eq!(
        order,
        object(vec![
            ("id", text("A")),
            ("lineage", text("sha256:lineage")),
            ("base", text("sha256:base")),
            ("result", text("sha256:result")),
            ("revision", text(&format!("sha256:{}", "a".repeat(64)))),
        ])
    );
}

#[test]
fn revision_binding_map_is_exact_for_every_delivery() {
    let request = composition_request(vec!["A".into()]);
    assert!(validate_revision_bindings(
        &request,
        &request.deliveries,
        &BTreeMap::from([("A".into(), revision_id('a'))]),
    )
    .is_ok());
    assert!(validate_revision_bindings(&request, &request.deliveries, &BTreeMap::new()).is_err());
    assert!(validate_revision_bindings(
        &request,
        &request.deliveries,
        &BTreeMap::from([("outside-catalog".into(), revision_id('a'))]),
    )
    .is_err());
}

#[test]
fn one_revision_cannot_be_pinned_to_two_contributions() {
    let mut request = composition_request(vec!["A".into()]);
    request.catalog.push(ContributionSpec {
        id: "B".into(),
        lineage: None,
        requires: Vec::new(),
        atomic_group: None,
        revision_of: None,
        derived_from: Vec::new(),
    });
    let revision = revision_id('a');
    assert!(validate_revision_bindings(
        &request,
        &request.deliveries,
        &BTreeMap::from([("A".into(), revision.clone()), ("B".into(), revision)]),
    )
    .is_err());
}
