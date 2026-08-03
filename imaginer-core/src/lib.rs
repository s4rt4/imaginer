//! Core image logic for Imaginer: decoding, metadata, and (later) caching and edits.
//!
//! Deliberately free of any UI dependency so it stays testable headlessly.

// Re-exported so the UI can name pixel buffers without depending on `image`
// directly — one crate deciding the version is one fewer way for the two to drift.
pub use image;

pub mod adjust;
pub mod cache;
pub mod convert;
pub mod decode;
pub mod edit;
pub mod export;
pub mod folder;
pub mod metadata;
pub mod svg;

pub use adjust::Adjust;
pub use cache::{ImageCache, Stamp};
pub use convert::{Outcome as ConvertOutcome, convert_all, convert_file};
pub use decode::{
    DecodeError, Decoded, SUPPORTED_EXTENSIONS, Stage, decode_full, decode_preview, is_supported,
};
pub use edit::{Edits, Op};
pub use export::{ExportError, Format, Settings as ExportSettings};
pub use folder::{Folder, Order, SortKey};
pub use metadata::{Info, Orientation};
pub use svg::Svg;
