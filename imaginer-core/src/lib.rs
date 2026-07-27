//! Core image logic for Imaginer: decoding, metadata, and (later) caching and edits.
//!
//! Deliberately free of any UI dependency so it stays testable headlessly.

pub mod decode;
pub mod metadata;

pub use decode::{Decoded, DecodeError, Stage, decode_full, decode_preview};
pub use metadata::Orientation;
