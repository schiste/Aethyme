//! Portable records, identities and canonical encodings for Aethyme Local v3.
//!
//! This crate holds the pure parts of the contracts described in the Local v3
//! plan (§2.4, §5): identity derivation, canonical encodings and validation.
//! It performs no I/O and knows nothing about Git, databases, the broker or
//! the CLI; adapters elsewhere feed it bytes.
//!
//! Everything lives under [`experimental_v0`]. Nothing here is a stable public
//! contract: the plan keeps shared schema experimental until real consumers and
//! its qualification gates (Q1, Q2, Q5) provide evidence. Do not persist these
//! identities in retained records until the relevant decision (D10) is closed.

pub mod experimental_v0;
