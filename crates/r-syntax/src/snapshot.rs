use std::{ops::Range, sync::Arc};

use r_source::{OffsetBias, SourceText};

use crate::{Diagnostic, GreenNode, SyntaxNode};

/// Identity of a decoded document. Callers should allocate IDs monotonically.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl Version {
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

/// Compatibility targets that affect deterministic syntax decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SyntaxProfile {
    pub r: Version,
    pub roxygen2: Version,
}

impl SyntaxProfile {
    pub const R_4_6_1_ROXYGEN2_8_1_0: Self = Self {
        r: Version::new(4, 6, 1),
        roxygen2: Version::new(8, 1, 0),
    };
}

impl Default for SyntaxProfile {
    fn default() -> Self {
        Self::R_4_6_1_ROXYGEN2_8_1_0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Completeness {
    /// Parsing reached EOF without syntax errors.
    Complete,
    /// A valid prefix likely needs more input (for example an open delimiter).
    Incomplete,
    /// A tree was produced, but the input is malformed rather than incomplete.
    Invalid,
}

/// Immutable, thread-safe parse output. Red Rowan handles are never retained.
#[derive(Debug, Clone)]
pub struct ParseSnapshot {
    document: DocumentId,
    source: Arc<str>,
    source_text: Option<Arc<SourceText>>,
    green: GreenNode,
    diagnostics: Arc<[Diagnostic]>,
    completeness: Completeness,
    profile: SyntaxProfile,
}

impl ParseSnapshot {
    pub fn new(
        document: DocumentId,
        source: impl Into<Arc<str>>,
        green: GreenNode,
        diagnostics: impl Into<Arc<[Diagnostic]>>,
        completeness: Completeness,
        profile: SyntaxProfile,
    ) -> Self {
        Self {
            document,
            source: source.into(),
            source_text: None,
            green,
            diagnostics: diagnostics.into(),
            completeness,
            profile,
        }
    }

    /// Creates a snapshot retaining the decoded source and all of its maps.
    pub fn new_with_source_text(
        document: DocumentId,
        source_text: impl Into<Arc<SourceText>>,
        green: GreenNode,
        diagnostics: impl Into<Arc<[Diagnostic]>>,
        completeness: Completeness,
        profile: SyntaxProfile,
    ) -> Self {
        let source_text = source_text.into();
        Self {
            document,
            source: source_text.text_arc(),
            source_text: Some(source_text),
            green,
            diagnostics: diagnostics.into(),
            completeness,
            profile,
        }
    }

    pub fn document(&self) -> DocumentId {
        self.document
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn source_arc(&self) -> Arc<str> {
        Arc::clone(&self.source)
    }
    pub fn source_text(&self) -> Option<&SourceText> {
        self.source_text.as_deref()
    }
    pub fn source_text_arc(&self) -> Option<Arc<SourceText>> {
        self.source_text.as_ref().map(Arc::clone)
    }
    /// Projects a decoded UTF-8 range to the original byte range.
    pub fn original_byte_range(&self, range: rowan::TextRange) -> Option<Range<usize>> {
        let source = self.source_text()?;
        let start = source
            .offset_map()
            .decoded_to_original(u32::from(range.start()) as usize, OffsetBias::Left)?;
        let end = source
            .offset_map()
            .decoded_to_original(u32::from(range.end()) as usize, OffsetBias::Right)?;
        Some(start..end)
    }
    pub fn green(&self) -> &GreenNode {
        &self.green
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub fn completeness(&self) -> Completeness {
        self.completeness
    }
    pub fn profile(&self) -> SyntaxProfile {
        self.profile
    }

    /// Recreates a red root on the calling thread.
    pub fn root(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }

    /// Limits a red root to a callback, making the intended lifetime explicit.
    pub fn with_root<T>(&self, f: impl FnOnce(&SyntaxNode) -> T) -> T {
        f(&self.root())
    }
}
