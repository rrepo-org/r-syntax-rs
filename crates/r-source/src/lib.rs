//! Immutable, deterministic source storage and coordinate conversion for R.
//!
//! This crate deliberately does not consult the process locale or an R
//! installation. Callers select explicit compatibility and encoding profiles.

mod decode;
mod line_index;
mod logical;
mod profile;
mod source;

pub use decode::{decode, DecodeError, DecodeIssue, DecodedSource, OffsetBias, OffsetMap};
pub use line_index::{LineIndex, LinePosition, PositionError, TextRange};
pub use logical::{LineDirective, LogicalLocation, LogicalSourceMap, LogicalSourceMapError};
pub use profile::{
    CompatibilityProfile, DecodeMode, EncodingProfile, LocaleProfile, RVersion, SourceEncoding,
};
pub use source::{SourceError, SourceLimits, SourceText};
