#[path = "source.rs"]
mod subject;

fn code(status: &subject::Status) -> u8 {
    match status {
        subject::Status::Running => 0,
        subject::Status::Unknown => 1,
        subject::Status::Completed => 2,
        subject::Status::Failed => 3,
    }
}

fn notice_projection(notices: &[(u64, subject::Status)]) -> Vec<(u64, u8)> {
    notices.iter().map(|(id, status)| (*id, code(status))).collect()
}

#[test]
fn requested_exact_status_text() {
    assert_eq!(subject::status_text(subject::Status::Running), "In progress");
    assert_eq!(subject::status_text(subject::Status::Unknown), "Completion unknown");
    assert_eq!(subject::status_text(subject::Status::Completed), "Completed");
    assert_eq!(subject::status_text(subject::Status::Failed), "Failed");
}

#[test]
fn requested_notice_dedup_is_exact_pair_and_preserves_first_order() {
    use subject::Status::{Completed, Failed, Running, Unknown};
    let notices = vec![
        (9, Unknown), (9, Unknown), (9, Running), (8, Unknown),
        (9, Failed), (9, Running), (8, Unknown), (9, Completed),
    ];
    let before = notice_projection(&notices);
    assert_eq!(
        notice_projection(&subject::notifications(&notices)),
        vec![(9, 1), (9, 0), (8, 1), (9, 3), (9, 2)]
    );
    assert_eq!(notice_projection(&notices), before);
    assert!(subject::notifications(&[]).is_empty());
    assert_eq!(
        notice_projection(&subject::notifications(&[
            (u64::MAX, Running), (u64::MAX, Unknown),
            (u64::MAX, Completed), (u64::MAX, Failed),
        ])),
        vec![(u64::MAX, 0), (u64::MAX, 1), (u64::MAX, 2), (u64::MAX, 3)]
    );
}

#[test]
fn survives_lost_ack_unknown_and_replay_only_for_failed() {
    assert_eq!(code(&subject::lost_ack()), 1);
    assert_eq!(subject::status_text(subject::lost_ack()), "Completion unknown");
    assert!(!subject::may_replay(subject::Status::Running));
    assert!(!subject::may_replay(subject::Status::Unknown));
    assert!(!subject::may_replay(subject::Status::Completed));
    assert!(subject::may_replay(subject::Status::Failed));
    assert!(!subject::may_replay(subject::lost_ack()));
}
