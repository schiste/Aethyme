//! Determinism harness for overlay producers.
//!
//! Determinism is the one property the storage layer cannot
//! enforce on a producer's behalf — bincode produces stable bytes
//! for deterministic *input*, but if the producer hands it a
//! `HashMap`, an unsorted `Vec`, or a wall-clock timestamp, the
//! bytes drift. This module ships the assertion every concrete
//! producer should call from its tests so that drift is caught at
//! the producer rather than at the §5.7 CI determinism gate
//! (where the failure is one repo-wide-regenerate away from
//! masking the cause).
//!
//! ## Why a separate module
//!
//! Producers in 4.7.3+ live in different crates / different
//! tests. Centralizing the byte-equality check here means every
//! producer's determinism test is one line, and a future
//! enhancement to the check (e.g. "also assert the wire-format
//! version is current") lands in one place.

use aethyme_graph_storage::write_overlay_bytes;

use crate::{OverlayProducer, ProducerCtx};

/// Run the producer twice against the same context and assert the
/// two encoded outputs are byte-identical.
///
/// Panics with a producer-identifying message on either:
/// - a `produce()` error on either run (treated as a failure to
///   exercise the determinism contract at all — the test isn't
///   meaningful unless both calls succeed),
/// - non-matching bytes between runs (the producer is not
///   deterministic).
///
/// This is a test helper. It panics rather than returning a Result
/// because the only thing a test would do with a Result here is
/// `.unwrap()` it anyway.
pub fn assert_overlay_producer_is_deterministic<P>(
    producer: &P,
    ctx: &ProducerCtx<'_>,
) where
    P: OverlayProducer,
{
    let first = producer.produce(ctx).unwrap_or_else(|e| {
        panic!(
            "OverlayProducer {} (version {}): first produce() call failed: {e}",
            producer.kind(),
            producer.producer_version(),
        )
    });
    let second = producer.produce(ctx).unwrap_or_else(|e| {
        panic!(
            "OverlayProducer {} (version {}): second produce() call failed: {e}",
            producer.kind(),
            producer.producer_version(),
        )
    });

    let first_bytes = write_overlay_bytes(&first).unwrap_or_else(|e| {
        panic!(
            "OverlayProducer {} (version {}): first overlay failed to encode: {e}",
            producer.kind(),
            producer.producer_version(),
        )
    });
    let second_bytes = write_overlay_bytes(&second).unwrap_or_else(|e| {
        panic!(
            "OverlayProducer {} (version {}): second overlay failed to encode: {e}",
            producer.kind(),
            producer.producer_version(),
        )
    });

    if first_bytes != second_bytes {
        panic!(
            "OverlayProducer {} (version {}) is non-deterministic: \
             encoded bytes differ between consecutive produce() calls \
             ({first_len} vs {second_len} bytes). Canonicalize the \
             payload before constructing the OverlayFragment — sort \
             collections, prefer BTreeMap over HashMap, avoid \
             wall-clock timestamps. See docs/architecture/\
             graph-schema.md §5.4.",
            producer.kind(),
            producer.producer_version(),
            first_len = first_bytes.len(),
            second_len = second_bytes.len(),
        );
    }
}
