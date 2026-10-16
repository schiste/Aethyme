use super::paths_within_peer_scope;

#[test]
fn empty_or_out_of_scope_packet_paths_are_not_visible_to_path_scope() {
    let peer_scope = vec!["src".to_string()];

    assert!(!paths_within_peer_scope(&peer_scope, &[]));
    assert!(paths_within_peer_scope(
        &peer_scope,
        &["src/lib.rs".to_string()]
    ));
    assert!(!paths_within_peer_scope(
        &peer_scope,
        &["tests/case.rs".to_string()]
    ));
    assert!(!paths_within_peer_scope(
        &peer_scope,
        &["src-other/lib.rs".to_string()]
    ));
}
