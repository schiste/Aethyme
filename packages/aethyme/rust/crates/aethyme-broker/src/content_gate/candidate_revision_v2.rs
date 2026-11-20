//! Independent draft snapshot framing. A digest alone does not establish that
//! a snapshot is authentic, complete, current, or suitable for publication.
use serde::Deserialize;
use sha2::{Digest, Sha256};

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_ITEMS: usize = 100_000;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub scope: String,
    pub site_id: String,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub site_base_url: Option<String>,
    pub base: Base,
    pub complete: bool,
    pub items: Vec<Item>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Base {
    pub kind: BaseKind,
    pub revision: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BaseKind {
    Git,
    Opaque,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub entity_id: String,
    pub public_path: String,
    pub media_type: String,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub revision: Option<String>,
    pub sha256: String,
    pub source: String,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub links: Option<Vec<String>>,
}

fn add(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), &'static str> {
    if out
        .len()
        .checked_add(bytes.len())
        .filter(|n| *n <= MAX_BYTES)
        .is_none()
    {
        return Err("candidate profile exceeds size limit");
    }
    out.extend_from_slice(bytes);
    Ok(())
}
fn string(out: &mut Vec<u8>, s: &str) -> Result<(), &'static str> {
    add(out, &(s.len() as u64).to_be_bytes())?;
    add(out, s.as_bytes())
}
fn optional(out: &mut Vec<u8>, s: Option<&str>) -> Result<(), &'static str> {
    match s {
        None => add(out, &[0]),
        Some(s) => {
            add(out, &[1])?;
            string(out, s)
        }
    }
}

/// Compute the published candidate-revision/v1 framing for host-provided data.
/// This does not validate routes, content digests, inventories, or provenance.
/// When parsing untrusted JSON, reject duplicate keys before deserialization.
pub fn snapshot_digest(snapshot: &Snapshot) -> Result<[u8; 32], &'static str> {
    if snapshot.items.len() > MAX_ITEMS {
        return Err("too many candidate items");
    }
    let mut scope = b"content-gate:candidate-scope:v1\0".to_vec();
    string(&mut scope, &snapshot.scope)?;
    string(&mut scope, &snapshot.site_id)?;
    optional(&mut scope, snapshot.site_base_url.as_deref())?;
    let mut descriptor = b"content-gate:candidate-snapshot:v1\0".to_vec();
    string(
        &mut descriptor,
        match snapshot.base.kind {
            BaseKind::Git => "git",
            BaseKind::Opaque => "opaque",
        },
    )?;
    string(&mut descriptor, &snapshot.base.revision)?;
    add(&mut descriptor, &[u8::from(snapshot.complete)])?;
    let mut total = scope
        .len()
        .checked_add(descriptor.len())
        .ok_or("candidate profile exceeds size limit")?;
    if total > MAX_BYTES {
        return Err("candidate profile exceeds size limit");
    }
    let mut items: Vec<_> = snapshot.items.iter().collect();
    items.sort_unstable_by(|a, b| a.entity_id.as_bytes().cmp(b.entity_id.as_bytes()));
    let mut hash = Sha256::new();
    hash.update(b"content-gate:revision-set:v1\0");
    hash.update((scope.len() as u64).to_be_bytes());
    hash.update(&scope);
    hash.update((descriptor.len() as u64).to_be_bytes());
    hash.update(&descriptor);
    hash.update((items.len() as u32).to_be_bytes());
    let mut previous: Option<&str> = None;
    for item in items {
        let id = &item.entity_id;
        if id.is_empty() || id.len() > 4096 || id.chars().any(char::is_control) {
            return Err("invalid entity id");
        }
        if previous == Some(id) {
            return Err("duplicate entity id");
        }
        previous = Some(id);
        let mut record = b"content-gate:candidate-item:v1\0".to_vec();
        string(&mut record, &item.public_path)?;
        string(&mut record, &item.media_type)?;
        optional(&mut record, item.revision.as_deref())?;
        string(&mut record, &item.sha256)?;
        string(&mut record, &item.source)?;
        match &item.links {
            None => add(&mut record, &[0])?,
            Some(links) => {
                add(&mut record, &[1])?;
                add(
                    &mut record,
                    &u32::try_from(links.len())
                        .map_err(|_| "too many candidate links")?
                        .to_be_bytes(),
                )?;
                for link in links {
                    string(&mut record, link)?;
                }
            }
        }
        total = total
            .checked_add(id.len())
            .and_then(|n| n.checked_add(record.len()))
            .ok_or("candidate profile exceeds size limit")?;
        if total > MAX_BYTES {
            return Err("candidate profile exceeds size limit");
        }
        hash.update((id.len() as u32).to_be_bytes());
        hash.update(id.as_bytes());
        hash.update((record.len() as u64).to_be_bytes());
        hash.update(&record);
    }
    Ok(hash.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Snapshot, Item) {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("portable-verdict-vector.json")).unwrap();
        (
            serde_json::from_value(v["base"].clone()).unwrap(),
            serde_json::from_value(v["after"][0].clone()).unwrap(),
        )
    }
    fn hex(hash: [u8; 32]) -> String {
        hash.iter().map(|b| format!("{b:02x}")).collect()
    }
    #[test]
    fn matches_independent_aexeo_vector() {
        let (mut base, item) = fixture();
        assert_eq!(
            hex(snapshot_digest(&base).unwrap()),
            "69a706fb08665399c92496d0c4d7321e5d9857d469fc1ef0d8ee844526dd4ea7"
        );
        base.items.push(item);
        assert_eq!(
            hex(snapshot_digest(&base).unwrap()),
            "5b3bbd7e90693296330978fdd708f309cc3c52bac6a8f17f450567d472b1c8a2"
        );
    }
    #[test]
    fn binds_fields_and_distinguishes_unavailable_links() {
        let (mut snapshot, item) = fixture();
        snapshot.items.push(item);
        let expected = snapshot_digest(&snapshot).unwrap();
        let mut variants = Vec::new();
        let mut v = snapshot.clone();
        v.scope.push('x');
        variants.push(v);
        let mut v = snapshot.clone();
        v.site_id.push('x');
        variants.push(v);
        let mut v = snapshot.clone();
        v.site_base_url = Some(String::new());
        variants.push(v);
        let mut v = snapshot.clone();
        v.base.kind = BaseKind::Git;
        variants.push(v);
        let mut v = snapshot.clone();
        v.base.revision.push('x');
        variants.push(v);
        let mut v = snapshot.clone();
        v.complete = false;
        variants.push(v);
        let mut v = snapshot.clone();
        v.items[0].entity_id.push('x');
        variants.push(v);
        let mut v = snapshot.clone();
        v.items[0].public_path.push('x');
        variants.push(v);
        let mut v = snapshot.clone();
        v.items[0].media_type.push('x');
        variants.push(v);
        let mut v = snapshot.clone();
        v.items[0].revision = Some(String::new());
        variants.push(v);
        let mut v = snapshot.clone();
        v.items[0].sha256.push('x');
        variants.push(v);
        let mut v = snapshot.clone();
        v.items[0].source.push('x');
        variants.push(v);
        let mut v = snapshot.clone();
        v.items[0].links = Some(vec![]);
        variants.push(v);
        for variant in variants {
            assert_ne!(expected, snapshot_digest(&variant).unwrap());
        }
    }
    #[test]
    fn sorts_items_and_rejects_invalid_ids() {
        let (mut snapshot, item) = fixture();
        snapshot.items.push(item.clone());
        let mut second = item;
        second.entity_id = "other".into();
        snapshot.items.push(second);
        let expected = snapshot_digest(&snapshot).unwrap();
        snapshot.items.reverse();
        assert_eq!(expected, snapshot_digest(&snapshot).unwrap());
        snapshot.items[0].entity_id = snapshot.items[1].entity_id.clone();
        assert!(snapshot_digest(&snapshot).is_err());
        snapshot.items[0].entity_id.clear();
        assert!(snapshot_digest(&snapshot).is_err());
        snapshot.items[0].entity_id = "bad\nid".into();
        assert!(snapshot_digest(&snapshot).is_err());
        snapshot.items[0].entity_id = "x".repeat(4097);
        assert!(snapshot_digest(&snapshot).is_err());
    }
    #[test]
    fn rejects_excessive_size_and_missing_nullable_fields() {
        let (mut snapshot, item) = fixture();
        snapshot.items.push(item);
        snapshot.items[0].source = "x".repeat(MAX_BYTES);
        assert!(snapshot_digest(&snapshot).is_err());
        let v: serde_json::Value =
            serde_json::from_str(include_str!("portable-verdict-vector.json")).unwrap();
        let mut base = v["base"].clone();
        base.as_object_mut().unwrap().remove("site_base_url");
        assert!(serde_json::from_value::<Snapshot>(base).is_err());
    }
}
