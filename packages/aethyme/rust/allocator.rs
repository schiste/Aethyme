//! Optional process-wide allocators, installed only by binary entrypoints.
//! With neither feature enabled, the Rust default remains unchanged.

#[cfg(all(feature = "allocator-rusty", feature = "allocator-mimalloc"))]
compile_error!("allocator-rusty and allocator-mimalloc are mutually exclusive");

#[cfg(feature = "allocator-rusty")]
#[global_allocator]
static ALLOC: rusty_alloc_api::RustyAlloc = rusty_alloc_api::RustyAlloc;

#[cfg(all(feature = "allocator-mimalloc", not(feature = "allocator-rusty")))]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;
