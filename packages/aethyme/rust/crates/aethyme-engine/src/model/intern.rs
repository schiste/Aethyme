//! Deduplicating `Arc<str>` factory.
//!
//! Used by Phase 2 of the build-memory work: entity types (Symbol,
//! FunctionNode, Edge, …) hold `Arc<str>` for their string fields so that
//! cloning becomes a refcount increment instead of a heap allocation.
//! `ArcInterner::intern` ensures that the same logical string lives in
//! exactly one heap allocation, no matter how many times it appears.
//!
//! Not thread-safe by itself — the build pipeline owns one per `&mut`
//! context. Wrap with a Mutex/sharded structure if you need concurrent
//! interning across rayon threads.

use std::collections::HashSet;
use std::sync::Arc;

/// Deduplicates `&str` inputs into shared `Arc<str>` outputs. Within one
/// interner, the same logical string is always backed by the same heap
/// allocation.
#[derive(Debug, Default)]
pub struct ArcInterner {
    table: HashSet<Arc<str>>,
}

impl ArcInterner {
    pub fn new() -> Self {
        Self { table: HashSet::new() }
    }

    /// Return the canonical `Arc<str>` for `value`, allocating a new heap
    /// buffer only the first time `value` is seen.
    pub fn intern(&mut self, value: &str) -> Arc<str> {
        if let Some(existing) = self.table.get(value) {
            return Arc::clone(existing);
        }
        let arc: Arc<str> = Arc::from(value);
        self.table.insert(Arc::clone(&arc));
        arc
    }

    /// Same as `intern` but for `Option<&str>`. Returns `None` if input is `None`.
    pub fn intern_opt(&mut self, value: Option<&str>) -> Option<Arc<str>> {
        value.map(|v| self.intern(v))
    }

    /// Number of unique strings interned so far.
    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }
}

// ─────────────────────────────────────────────────────────────────────────
// serde helpers
//
// `Arc<str>` does not derive Serialize/Deserialize directly. These helpers
// let entity types use `#[serde(with = "arc_str")]` (and the `_opt` variant)
// on their `Arc<str>` fields without pulling in `serde_with`.
// ─────────────────────────────────────────────────────────────────────────

pub mod arc_str {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Arc<str>, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(value)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Arc<str>, D::Error> {
        let s = String::deserialize(de)?;
        Ok(Arc::from(s.as_str()))
    }
}

pub mod arc_str_opt {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<Arc<str>>,
        ser: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(s) => ser.serialize_some(s.as_ref()),
            None => ser.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Arc<str>>, D::Error> {
        let opt: Option<String> = Option::deserialize(de)?;
        Ok(opt.map(|s| Arc::from(s.as_str())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_returns_same_arc_for_same_string() {
        let mut interner = ArcInterner::new();
        let a = interner.intern("hello");
        let b = interner.intern("hello");
        // Both Arcs point to the same heap allocation.
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(&*a, "hello");
        assert_eq!(interner.len(), 1);
    }

    #[test]
    fn intern_returns_different_arc_for_different_strings() {
        let mut interner = ArcInterner::new();
        let a = interner.intern("foo");
        let b = interner.intern("bar");
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn intern_opt_handles_none() {
        let mut interner = ArcInterner::new();
        assert!(interner.intern_opt(None).is_none());
        assert_eq!(interner.len(), 0);
        let some = interner.intern_opt(Some("x"));
        assert_eq!(some.as_deref(), Some("x"));
        assert_eq!(interner.len(), 1);
    }

    #[test]
    fn many_clones_share_one_heap_allocation() {
        let mut interner = ArcInterner::new();
        let original = interner.intern("shared");
        let clones: Vec<_> = (0..1000).map(|_| Arc::clone(&original)).collect();
        // 1001 references (original + 1000 clones), but only one heap buffer.
        assert_eq!(Arc::strong_count(&original), 1001 + 1); // +1 for the interner's own copy
        for clone in &clones {
            assert!(Arc::ptr_eq(&original, clone));
        }
        assert_eq!(interner.len(), 1);
    }

    #[test]
    fn serde_roundtrip_arc_str() {
        use serde::{Deserialize, Serialize};

        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrap(#[serde(with = "super::arc_str")] Arc<str>);

        let original = Wrap(Arc::from("payload"));
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(json, "\"payload\"");
        let back: Wrap = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn serde_roundtrip_arc_str_opt() {
        use serde::{Deserialize, Serialize};

        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrap(#[serde(with = "super::arc_str_opt")] Option<Arc<str>>);

        let some = Wrap(Some(Arc::from("hi")));
        let some_json = serde_json::to_string(&some).unwrap();
        assert_eq!(some_json, "\"hi\"");
        let some_back: Wrap = serde_json::from_str(&some_json).unwrap();
        assert_eq!(some_back, some);

        let none = Wrap(None);
        let none_json = serde_json::to_string(&none).unwrap();
        assert_eq!(none_json, "null");
        let none_back: Wrap = serde_json::from_str(&none_json).unwrap();
        assert_eq!(none_back, none);
    }
}
