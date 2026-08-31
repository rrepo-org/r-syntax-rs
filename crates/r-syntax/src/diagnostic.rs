use rowan::TextRange;

/// Stable machine-readable diagnostic identifier (for example `R-PARSE-001`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiagnosticCode(&'static str);

impl DiagnosticCode {
    pub const UNEXPECTED_TOKEN: Self = Self("R-PARSE-001");
    pub const EXPECTED_EXPRESSION: Self = Self("R-PARSE-002");
    pub const EXPECTED_TOKEN: Self = Self("R-PARSE-003");
    pub const UNTERMINATED_LITERAL: Self = Self("R-LEX-001");
    pub const INVALID_ESCAPE: Self = Self("R-LEX-002");
    pub const INVALID_NUMBER: Self = Self("R-LEX-003");
    pub const INVALID_UTF8: Self = Self("R-SOURCE-001");
    pub const TREE_INVARIANT: Self = Self("R-SYNTAX-001");

    pub const fn new(code: &'static str) -> Self {
        Self(code)
    }
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl core::fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecoveryKind {
    None,
    InsertedToken,
    SkippedToken,
    WrappedErrorNode,
    ReachedDelimiter,
    ReachedLineBoundary,
    ReachedEndOfFile,
}

/// Details about how parsing continued after a diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Recovery {
    pub kind: RecoveryKind,
    /// Input consumed or skipped by recovery. Empty for pure insertion.
    pub range: TextRange,
    /// Expected or inserted token, when applicable.
    pub token: Option<crate::SyntaxKind>,
}

impl Recovery {
    pub const fn none(at: TextRange) -> Self {
        Self {
            kind: RecoveryKind::None,
            range: at,
            token: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub severity: Severity,
    pub range: TextRange,
    pub message: String,
    pub recovery: Recovery,
}

impl Diagnostic {
    pub fn new(
        code: DiagnosticCode,
        severity: Severity,
        range: TextRange,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity,
            range,
            message: message.into(),
            recovery: Recovery::none(range),
        }
    }

    pub fn with_recovery(mut self, recovery: Recovery) -> Self {
        self.recovery = recovery;
        self
    }
}
