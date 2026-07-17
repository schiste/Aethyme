//! Deduplicating `Arc<str>` factory plus an `InternedStr` newtype wrapper.
//!
//! Used by Phase 2 of the build-memory work: entity types (Symbol,
//! FunctionNode, Edge, …) hold `InternedStr` for their string fields so that
//! cloning becomes a refcount increment instead of a heap allocation.
//! `ArcInterner::intern` ensures that the same logical string lives in
//! exactly one heap allocation, no matter how many times it appears.
//!
//! Not thread-safe by itself — the build pipeline owns one per `&mut`
//! context. Wrap with a Mutex/sharded structure if you need concurrent
//! interning across rayon threads.

use std::borrow::Borrow;
use std::collections::HashSet;
use std::ops::Deref;
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
        Self {
            table: HashSet::new(),
        }
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

// ─────────────────────────────────────────────────────────────────────────
// InternedStr — newtype wrapper over Arc<str> with the PartialEq impls,
// Display, Hash, Borrow<str>, and From conversions that make it a true
// drop-in replacement for `String` in entity types. Cloning is an atomic
// refcount increment.
//
// Defined here (not a separate module) because every consumer that uses
// `ArcInterner` will also want `InternedStr`.
// ─────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct InternedStr(Arc<str>);

impl InternedStr {
    pub fn new(s: impl Into<Arc<str>>) -> Self {
        Self(s.into())
    }

    /// Borrow the underlying `Arc<str>` (e.g. to share with another
    /// `InternedStr` or to push into a HashMap keyed on `Arc<str>`).
    pub fn as_arc(&self) -> &Arc<str> {
        &self.0
    }

    /// Borrow as `&str`. Provided so consumers don't need to write `&*x` or
    /// `x.as_ref()` — and so they don't accidentally hit the unstable
    /// `str::as_str` method via Deref.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for InternedStr {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for InternedStr {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for InternedStr {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for InternedStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&*self.0, f)
    }
}

// ── PartialEq with str-shaped types in BOTH directions ──────────────────
impl PartialEq<str> for InternedStr {
    fn eq(&self, other: &str) -> bool {
        &**self == other
    }
}
impl PartialEq<&str> for InternedStr {
    fn eq(&self, other: &&str) -> bool {
        &**self == *other
    }
}
impl PartialEq<String> for InternedStr {
    fn eq(&self, other: &String) -> bool {
        &**self == other.as_str()
    }
}
impl PartialEq<InternedStr> for str {
    fn eq(&self, other: &InternedStr) -> bool {
        self == &**other
    }
}
impl PartialEq<InternedStr> for &str {
    fn eq(&self, other: &InternedStr) -> bool {
        *self == &**other
    }
}
impl PartialEq<InternedStr> for String {
    fn eq(&self, other: &InternedStr) -> bool {
        self.as_str() == &**other
    }
}

// ── From conversions (covers all common construction sites) ─────────────
impl From<&str> for InternedStr {
    fn from(s: &str) -> Self {
        Self(Arc::from(s))
    }
}
impl From<String> for InternedStr {
    fn from(s: String) -> Self {
        Self(Arc::from(s.as_str()))
    }
}
impl From<&String> for InternedStr {
    fn from(s: &String) -> Self {
        Self(Arc::from(s.as_str()))
    }
}
impl From<Arc<str>> for InternedStr {
    fn from(s: Arc<str>) -> Self {
        Self(s)
    }
}
impl From<&Arc<str>> for InternedStr {
    fn from(s: &Arc<str>) -> Self {
        Self(Arc::clone(s))
    }
}
impl From<&InternedStr> for InternedStr {
    fn from(s: &InternedStr) -> Self {
        s.clone()
    }
}

// Boundary conversions for code that holds owned `String` (e.g. Anchor,
// ScopeItem, FunctionFact). These allocate; prefer keeping data as
// `InternedStr` end-to-end where possible.
impl From<InternedStr> for String {
    fn from(s: InternedStr) -> Self {
        s.0.to_string()
    }
}
impl From<&InternedStr> for String {
    fn from(s: &InternedStr) -> Self {
        s.0.to_string()
    }
}

// ── Serde — transparent string serialization ────────────────────────────
impl serde::Serialize for InternedStr {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for InternedStr {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        Ok(Self(Arc::from(s.as_str())))
    }
}

// `ArcInterner` can return `InternedStr` directly when desired.
impl ArcInterner {
    pub fn intern_to(&mut self, value: &str) -> InternedStr {
        InternedStr::from(self.intern(value))
    }
}

pub mod arc_str_opt {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Option<Arc<str>>, ser: S) -> Result<S::Ok, S::Error> {
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
    fn interned_str_partial_eq_against_all_str_shapes() {
        let s = InternedStr::from("hello");
        // &str
        assert_eq!(s, "hello");
        // String
        assert_eq!(s, String::from("hello"));
        // Reverse direction (matters for `find(|x| "literal" == x.id)`)
        assert_eq!("hello", s);
        assert_eq!(String::from("hello"), s);
        // Inequality
        assert_ne!(s, "world");
    }

    #[test]
    fn interned_str_clone_shares_heap_buffer() {
        let a = InternedStr::from("share me");
        let b = a.clone();
        assert!(Arc::ptr_eq(a.as_arc(), b.as_arc()));
    }

    #[test]
    fn interned_str_display_and_deref() {
        let s = InternedStr::from("payload");
        assert_eq!(format!("{}", s), "payload");
        assert_eq!(&*s, "payload");
        assert_eq!(s.len(), 7); // via Deref<Target=str>
    }

    #[test]
    fn interned_str_serde_roundtrip() {
        use serde::{Deserialize, Serialize};

        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrap {
            name: InternedStr,
        }

        let original = Wrap {
            name: InternedStr::from("payload"),
        };
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(json, r#"{"name":"payload"}"#);
        let back: Wrap = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn arc_interner_intern_to_returns_interned_str() {
        let mut interner = ArcInterner::new();
        let a = interner.intern_to("dup");
        let b = interner.intern_to("dup");
        // Same heap buffer, even though they're separate InternedStr values.
        assert!(Arc::ptr_eq(a.as_arc(), b.as_arc()));
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
