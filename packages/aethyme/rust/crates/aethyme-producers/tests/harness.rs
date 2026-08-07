//! Integration tests for `assert_overlay_producer_is_deterministic`.
//!
//! Two fake producers anchor the harness contract:
//!
//! - [`DeterministicFakeProducer`] — payload built from a `BTreeMap`,
//!   no interior state. The harness MUST accept it. Anchors the
//!   happy path so a regression that makes the harness reject
//!   legitimate producers is caught.
//! - [`NonDeterministicFakeProducer`] — payload depends on a
//!   `Cell<u32>` counter incremented on every `produce()` call. The
//!   harness MUST panic on it. Anchors the failure path so a
//!   regression that silently passes non-deterministic producers is
//!   caught — the whole point of the harness.
//!
//! Both producers share a `FragmentStore` fixture (real on-disk
//! layout under a tempdir + `bootstrap_repo`) because `ProducerCtx`
//! requires a store reference even when the producer doesn't read
//! from it. The producers ignore the store; the fixture just
//! satisfies the type system.

use std::cell::Cell;
use std::collections::BTreeMap;

use aethyme_graph_storage::{FragmentStore, OverlayFragment, bootstrap_repo};
use aethyme_producers::{
    OverlayProducer, ProducerCtx, ProducerError, assert_overlay_producer_is_deterministic,
};
use tempfile::TempDir;

/// Bootstrap a fresh repo layout and open a `FragmentStore` against
/// it. Returned `TempDir` must outlive the store — drop order matters
/// here because the store holds the path that the tempdir cleans up.
fn fixture() -> (TempDir, FragmentStore) {
    let tmp = tempfile::tempdir().expect("tempdir");
    bootstrap_repo(tmp.path(), "test-engine-0.0.0").expect("bootstrap");
    let store = FragmentStore::open(tmp.path()).expect("open store");
    (tmp, store)
}

// -- Deterministic fake -----------------------------------------------

struct DeterministicFakeProducer;

impl OverlayProducer for DeterministicFakeProducer {
    type Payload = BTreeMap<String, String>;

    fn kind(&self) -> &'static str {
        "test_deterministic"
    }

    fn producer_version(&self) -> &'static str {
        "test_deterministic/0.1.0"
    }

    fn produce(
        &self,
        _ctx: &ProducerCtx<'_>,
    ) -> Result<OverlayFragment<Self::Payload>, ProducerError> {
        // BTreeMap's iteration order is structural — the same
        // inserts always produce the same on-disk byte sequence.
        let mut payload = BTreeMap::new();
        payload.insert("alpha".to_string(), "1".to_string());
        payload.insert("beta".to_string(), "2".to_string());
        payload.insert("gamma".to_string(), "3".to_string());
        OverlayFragment::new(self.kind(), self.producer_version(), payload)
            .map_err(ProducerError::from)
    }
}

#[test]
fn deterministic_producer_passes_harness() {
    let (_tmp, store) = fixture();
    let ctx = ProducerCtx::new(&store);
    let producer = DeterministicFakeProducer;

    // No panic = pass. The harness internally calls produce() twice
    // and byte-compares the encoded outputs.
    assert_overlay_producer_is_deterministic(&producer, &ctx);
}

// -- Non-deterministic fake -------------------------------------------

/// Each call to `produce()` reads-and-increments the counter, so
/// successive payloads differ. This is what a real producer that
/// accidentally captured wall-clock time or HashMap order would look
/// like to the harness.
struct NonDeterministicFakeProducer {
    counter: Cell<u32>,
}

impl OverlayProducer for NonDeterministicFakeProducer {
    type Payload = BTreeMap<String, String>;

    fn kind(&self) -> &'static str {
        "test_non_deterministic"
    }

    fn producer_version(&self) -> &'static str {
        "test_non_deterministic/0.1.0"
    }

    fn produce(
        &self,
        _ctx: &ProducerCtx<'_>,
    ) -> Result<OverlayFragment<Self::Payload>, ProducerError> {
        let n = self.counter.get();
        self.counter.set(n + 1);
        let mut payload = BTreeMap::new();
        payload.insert("call_index".to_string(), n.to_string());
        OverlayFragment::new(self.kind(), self.producer_version(), payload)
            .map_err(ProducerError::from)
    }
}

#[test]
#[should_panic(expected = "non-deterministic")]
fn non_deterministic_producer_panics_harness() {
    let (_tmp, store) = fixture();
    let ctx = ProducerCtx::new(&store);
    let producer = NonDeterministicFakeProducer {
        counter: Cell::new(0),
    };

    // Expected to panic with the harness's "non-deterministic"
    // message; the #[should_panic(expected = ...)] attribute is the
    // assertion.
    assert_overlay_producer_is_deterministic(&producer, &ctx);
}

// -- Belt-and-braces: harness must not silently swallow store types --

/// Sanity check that `ProducerCtx::new` accepts a real store and
/// that the trait's `Payload` bounds (`Serialize + DeserializeOwned
/// + Clone + PartialEq`) compile for a non-trivial payload. If this
/// fails to compile, the harness suite is broken in a way that
/// affects every downstream producer crate.
#[test]
fn payload_bounds_compile_for_btreemap() {
    let (_tmp, store) = fixture();
    let _ctx = ProducerCtx::new(&store);

    let mut payload: BTreeMap<String, String> = BTreeMap::new();
    payload.insert("k".into(), "v".into());
    let frag = OverlayFragment::new("test_bounds", "test_bounds/0.1.0", payload.clone())
        .expect("build overlay");
    assert_eq!(frag.payload(), &payload);
}
