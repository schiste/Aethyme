#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry { pub id: u64, pub priority: i32, pub created: u64 }
pub fn page(entries: &[Entry], after: Option<(i32,u64,u64)>, limit: usize) -> Vec<Entry> {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|a,b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
    sorted.into_iter().filter(|e| after.is_none_or(|(p,c,id)| e.priority < p || (e.priority == p && (e.created,e.id) > (c,id)))).take(limit).collect()
}
