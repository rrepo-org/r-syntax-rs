//! Lossless concrete syntax primitives for R.
//!
//! This crate deliberately contains no parser.  Parsers produce immutable
//! [`rowan::GreenNode`]s and package them in [`ParseSnapshot`]s; consumers
//! recreate short-lived red views when they need to navigate a tree.
#![forbid(unsafe_code)]

mod ast;
mod diagnostic;
mod kind;
mod language;
mod location;
mod snapshot;
mod validation;

pub use ast::*;
pub use diagnostic::*;
pub use kind::SyntaxKind;
pub use language::{
    RLanguage, SyntaxElement, SyntaxElementChildren, SyntaxNode, SyntaxNodeChildren, SyntaxToken,
};
pub use location::{LocationError, SyntaxLocation};
pub use snapshot::{Completeness, DocumentId, ParseSnapshot, SyntaxProfile, Version};
pub use validation::{
    fingerprint, validate_snapshot, TreeFingerprint, ValidationError, ValidationErrorKind,
};

pub use rowan::{
    GreenNode, GreenNodeBuilder, GreenToken, NodeOrToken, TextRange, TextSize, WalkEvent,
};
