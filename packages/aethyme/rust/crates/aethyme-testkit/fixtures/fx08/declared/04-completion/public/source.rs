#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status { Running, Unknown, Completed, Failed }
pub fn lost_ack() -> Status { Status::Unknown }
pub fn may_replay(status: Status) -> bool { status == Status::Failed }
pub fn status_text(status: Status) -> &'static str {
    match status { Status::Running => "running", Status::Unknown => "unknown", Status::Completed => "completed", Status::Failed => "failed" }
}
pub fn notifications(events: &[(u64, Status)]) -> Vec<(u64, Status)> { events.to_vec() }
