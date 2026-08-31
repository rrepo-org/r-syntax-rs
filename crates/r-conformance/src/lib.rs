//! Process-free conformance primitives for R syntax implementations.
//!
//! This crate intentionally has no API for discovering or executing R. Parser
//! adapters provide stable textual kinds and exact token text as tree events.

use std::cmp::Ordering;
use std::error::Error;
use std::fmt;

use r_roxygen::{BlockAssociation, EmbeddedMapping, RoxygenParse};
use r_syntax::{NodeOrToken, ParseSnapshot, SyntaxNode};

/// The exact default compatibility profile fixed by Phase 1.
pub const DEFAULT_R_VERSION: &str = "4.6.1";
pub const DEFAULT_ROXYGEN2_VERSION: &str = "8.1.0";
pub const DEFAULT_ROWAN_VERSION: &str = "0.17";

/// Version targets carried with cases and results.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatibilityProfile {
    pub r: String,
    pub roxygen2: String,
    pub rowan: String,
}

impl Default for CompatibilityProfile {
    fn default() -> Self {
        Self {
            r: DEFAULT_R_VERSION.into(),
            roxygen2: DEFAULT_ROXYGEN2_VERSION.into(),
            rowan: DEFAULT_ROWAN_VERSION.into(),
        }
    }
}

/// A canonical preorder tree event. Kinds must be stable names, not enum values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TreeEvent<'a> {
    StartNode { kind: &'a str },
    Token { kind: &'a str, text: &'a str },
    FinishNode,
}

/// A stable 256-bit regression fingerprint.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    pub const ALGORITHM: &'static str = "r-conformance-fnv4-v1";

    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn to_hex(self) -> String {
        let mut output = String::with_capacity(64);
        for byte in self.0 {
            use fmt::Write as _;
            write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
        }
        output
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

/// Structural errors rejected before a tree fingerprint is produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TreeFingerprintError {
    Empty,
    MultipleRoots { event: usize },
    TokenOutsideRoot { event: usize },
    UnexpectedFinish { event: usize },
    UnclosedNodes { count: usize },
}

impl fmt::Display for TreeFingerprintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("tree event stream is empty"),
            Self::MultipleRoots { event } => {
                write!(formatter, "second root starts at event {event}")
            }
            Self::TokenOutsideRoot { event } => {
                write!(formatter, "token outside root at event {event}")
            }
            Self::UnexpectedFinish { event } => {
                write!(formatter, "unmatched finish at event {event}")
            }
            Self::UnclosedNodes { count } => write!(formatter, "tree has {count} unclosed node(s)"),
        }
    }
}

impl Error for TreeFingerprintError {}

/// Validates and fingerprints one rooted tree in canonical preorder.
pub fn tree_fingerprint<'a>(
    events: impl IntoIterator<Item = &'a TreeEvent<'a>>,
) -> Result<Fingerprint, TreeFingerprintError> {
    let mut hasher = StableHasher::new(b"tree");
    let mut depth = 0usize;
    let mut saw_root = false;
    let mut finished_root = false;
    let mut count = 0usize;

    for (index, event) in events.into_iter().enumerate() {
        count += 1;
        match event {
            TreeEvent::StartNode { kind } => {
                if depth == 0 {
                    if saw_root || finished_root {
                        return Err(TreeFingerprintError::MultipleRoots { event: index });
                    }
                    saw_root = true;
                }
                hasher.field(1, kind.as_bytes());
                depth += 1;
            }
            TreeEvent::Token { kind, text } => {
                if depth == 0 {
                    return Err(TreeFingerprintError::TokenOutsideRoot { event: index });
                }
                hasher.field(2, kind.as_bytes());
                hasher.field(3, text.as_bytes());
            }
            TreeEvent::FinishNode => {
                if depth == 0 {
                    return Err(TreeFingerprintError::UnexpectedFinish { event: index });
                }
                hasher.field(4, &[]);
                depth -= 1;
                finished_root = depth == 0;
            }
        }
    }

    if count == 0 || !saw_root {
        Err(TreeFingerprintError::Empty)
    } else if depth != 0 {
        Err(TreeFingerprintError::UnclosedNodes { count: depth })
    } else {
        Ok(hasher.finish())
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

/// A UTF-8 byte range in the decoded source, represented as `[start, end)`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TextRange {
    pub start: u32,
    pub end: u32,
}

impl TextRange {
    pub const fn new(start: u32, end: u32) -> Option<Self> {
        if start <= end {
            Some(Self { start, end })
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub range: TextRange,
    pub code: String,
    pub message: String,
    pub notes: Vec<String>,
}

/// Fingerprints one diagnostic, preserving note order.
pub fn diagnostic_fingerprint(diagnostic: &Diagnostic) -> Fingerprint {
    let mut hasher = StableHasher::new(b"diagnostic");
    hash_diagnostic(&mut hasher, diagnostic);
    hasher.finish()
}

/// Fingerprints diagnostics as a canonical multiset, independent of input order.
pub fn diagnostics_fingerprint(diagnostics: &[Diagnostic]) -> Fingerprint {
    let mut ordered: Vec<&Diagnostic> = diagnostics.iter().collect();
    ordered.sort_by(|left, right| compare_diagnostics(left, right));
    let mut hasher = StableHasher::new(b"diagnostic-set");
    hasher.field(10, &(ordered.len() as u64).to_le_bytes());
    for diagnostic in ordered {
        hash_diagnostic(&mut hasher, diagnostic);
    }
    hasher.finish()
}

fn compare_diagnostics(left: &Diagnostic, right: &Diagnostic) -> Ordering {
    (
        left.severity,
        left.range,
        left.code.as_str(),
        left.message.as_str(),
        left.notes.as_slice(),
    )
        .cmp(&(
            right.severity,
            right.range,
            right.code.as_str(),
            right.message.as_str(),
            right.notes.as_slice(),
        ))
}

fn hash_diagnostic(hasher: &mut StableHasher, diagnostic: &Diagnostic) {
    let severity = match diagnostic.severity {
        Severity::Error => 1,
        Severity::Warning => 2,
        Severity::Note => 3,
    };
    hasher.field(1, &[severity]);
    hasher.field(2, &diagnostic.range.start.to_le_bytes());
    hasher.field(3, &diagnostic.range.end.to_le_bytes());
    hasher.field(4, diagnostic.code.as_bytes());
    hasher.field(5, diagnostic.message.as_bytes());
    hasher.field(6, &(diagnostic.notes.len() as u64).to_le_bytes());
    for note in &diagnostic.notes {
        hasher.field(7, note.as_bytes());
    }
}

/// A portable corpus identifier suitable for a relative fixture path.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CaseId(String);

impl CaseId {
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidCaseId> {
        let value = value.into();
        let valid = !value.is_empty()
            && !value.starts_with('/')
            && !value.ends_with('/')
            && value
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/')
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(InvalidCaseId(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidCaseId(String);

impl fmt::Display for InvalidCaseId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid portable corpus case ID: {:?}", self.0)
    }
}

impl Error for InvalidCaseId {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorpusCase {
    pub id: CaseId,
    pub source: String,
    pub profile: CompatibilityProfile,
    pub tags: Vec<String>,
    pub expected: Option<ExpectedResult>,
    pub oracle: Option<OracleRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpectedResult {
    pub tree: Fingerprint,
    pub diagnostics: Fingerprint,
    pub diagnostic_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Completion {
    Complete,
    Recovered,
    Rejected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorpusResult {
    pub case_id: CaseId,
    pub tree: Option<Fingerprint>,
    pub diagnostics: Fingerprint,
    pub diagnostic_count: usize,
    pub completion: Completion,
}

impl CorpusResult {
    pub fn matches(&self, expected: &ExpectedResult) -> bool {
        self.tree == Some(expected.tree)
            && self.diagnostics == expected.diagnostics
            && self.diagnostic_count == expected.diagnostic_count
    }
}

/// Provenance for a stored fixture. This data-only type cannot execute a command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleRecord {
    pub implementation: String,
    pub version: String,
    pub generated_at: String,
    pub script_revision: String,
    pub command_description: String,
}

/// Canonical production-parser output fingerprints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotFingerprints {
    pub snapshot: Fingerprint,
    pub tree: Fingerprint,
    pub diagnostics: Fingerprint,
}

/// Length-framed canonical serialization of a production parse snapshot.
///
/// The document ID is intentionally omitted: it identifies a caller-owned
/// document, not a parse result. Source, profile, completeness, tree, and full
/// diagnostics (including recovery) are included.
pub fn serialize_parse_snapshot(snapshot: &ParseSnapshot) -> Vec<u8> {
    let mut out = Canonical::new(b"parse-snapshot-v1");
    out.field(1, snapshot.source().as_bytes());
    out.version(2, snapshot.profile().r);
    out.version(3, snapshot.profile().roxygen2);
    out.field(4, format!("{:?}", snapshot.completeness()).as_bytes());
    out.field(5, &serialize_r_tree(snapshot));
    out.field(6, &serialize_r_diagnostics(snapshot));
    out.finish()
}

/// Canonical preorder serialization of an R Rowan tree.
pub fn serialize_r_tree(snapshot: &ParseSnapshot) -> Vec<u8> {
    let mut out = Canonical::new(b"r-tree-v1");
    serialize_r_node(&mut out, &snapshot.root());
    out.finish()
}

fn serialize_r_node(out: &mut Canonical, node: &SyntaxNode) {
    out.field(1, format!("{:?}", node.kind()).as_bytes());
    for child in node.children_with_tokens() {
        match child {
            NodeOrToken::Node(node) => serialize_r_node(out, &node),
            NodeOrToken::Token(token) => {
                out.field(2, format!("{:?}", token.kind()).as_bytes());
                out.field(3, token.text().as_bytes());
            }
        }
    }
    out.field(4, &[]);
}

/// Canonical serialization of diagnostics in parser-provided order.
pub fn serialize_r_diagnostics(snapshot: &ParseSnapshot) -> Vec<u8> {
    let mut out = Canonical::new(b"r-diagnostics-v1");
    out.usize(1, snapshot.diagnostics().len());
    for diagnostic in snapshot.diagnostics() {
        out.field(2, diagnostic.code.as_str().as_bytes());
        out.field(3, format!("{:?}", diagnostic.severity).as_bytes());
        out.range(4, diagnostic.range);
        out.field(5, diagnostic.message.as_bytes());
        out.field(6, format!("{:?}", diagnostic.recovery.kind).as_bytes());
        out.range(7, diagnostic.recovery.range);
        out.field(
            8,
            diagnostic
                .recovery
                .token
                .map(|kind| format!("{kind:?}"))
                .unwrap_or_default()
                .as_bytes(),
        );
    }
    out.finish()
}

pub fn parse_snapshot_fingerprints(snapshot: &ParseSnapshot) -> SnapshotFingerprints {
    SnapshotFingerprints {
        snapshot: serialized_fingerprint(b"parse-snapshot", &serialize_parse_snapshot(snapshot)),
        tree: serialized_fingerprint(b"r-tree", &serialize_r_tree(snapshot)),
        diagnostics: serialized_fingerprint(b"r-diagnostics", &serialize_r_diagnostics(snapshot)),
    }
}

/// Canonical serialization of sidecar tree, projected text, metadata, and status.
pub fn serialize_roxygen_sidecar(sidecar: &RoxygenParse) -> Vec<u8> {
    let mut out = Canonical::new(b"roxygen-sidecar-v1");
    out.field(1, sidecar.projected_source().as_bytes());
    serialize_roxygen_node(&mut out, &sidecar.root());
    out.usize(10, sidecar.tokens().len());
    for token in sidecar.tokens() {
        out.field(11, format!("{:?}", token.kind).as_bytes());
        out.range(12, token.projected_range);
        out.range(13, token.host_range);
    }
    out.usize(20, sidecar.blocks().len());
    for block in sidecar.blocks() {
        out.range(21, block.host_range);
        out.range(22, block.projected_range);
        out.usize(23, block.line_count);
    }
    out.usize(30, sidecar.diagnostics().len());
    for diagnostic in sidecar.diagnostics() {
        out.field(31, diagnostic.code.as_str().as_bytes());
        out.field(32, format!("{:?}", diagnostic.severity).as_bytes());
        out.range(33, diagnostic.range);
        out.field(34, diagnostic.message.as_bytes());
    }
    out.field(40, &serialize_roxygen_associations(sidecar.associations()));
    out.usize(41, sidecar.embedded_parses().len());
    for embedded in sidecar.embedded_parses() {
        out.usize(42, embedded.block_index);
        out.range(43, embedded.projected_range);
        out.range(44, embedded.host_range);
        out.field(45, &serialize_parse_snapshot(embedded.snapshot()));
        out.field(46, &serialize_roxygen_mappings(embedded.mappings()));
    }
    let status = sidecar.status();
    out.boolean(50, status.limited);
    out.boolean(51, status.discovery_incomplete);
    out.boolean(52, status.diagnostics_truncated);
    out.finish()
}

fn serialize_roxygen_node(out: &mut Canonical, node: &r_roxygen::RoxygenNode) {
    out.field(2, format!("{:?}", node.kind()).as_bytes());
    for child in node.children_with_tokens() {
        match child {
            NodeOrToken::Node(node) => serialize_roxygen_node(out, &node),
            NodeOrToken::Token(token) => {
                out.field(3, format!("{:?}", token.kind()).as_bytes());
                out.field(4, token.text().as_bytes());
            }
        }
    }
    out.field(5, &[]);
}

pub fn serialize_roxygen_associations(associations: &[BlockAssociation]) -> Vec<u8> {
    let mut out = Canonical::new(b"roxygen-associations-v1");
    out.usize(1, associations.len());
    for association in associations {
        out.usize(2, association.block_index);
        out.field(3, format!("{:?}", association.state).as_bytes());
        out.optional_range(4, association.expression_range);
        out.range(5, association.intervening_host_range);
    }
    out.finish()
}

pub fn serialize_roxygen_mappings(mappings: &[EmbeddedMapping]) -> Vec<u8> {
    let mut out = Canonical::new(b"roxygen-mappings-v1");
    out.usize(1, mappings.len());
    for mapping in mappings {
        out.range(2, mapping.embedded_range);
        out.range(3, mapping.projected_range);
        out.range(4, mapping.host_range);
    }
    out.finish()
}

pub fn roxygen_sidecar_fingerprint(sidecar: &RoxygenParse) -> Fingerprint {
    serialized_fingerprint(b"roxygen-sidecar", &serialize_roxygen_sidecar(sidecar))
}

pub fn roxygen_associations_fingerprint(associations: &[BlockAssociation]) -> Fingerprint {
    serialized_fingerprint(
        b"roxygen-associations",
        &serialize_roxygen_associations(associations),
    )
}

pub fn roxygen_mappings_fingerprint(mappings: &[EmbeddedMapping]) -> Fingerprint {
    serialized_fingerprint(b"roxygen-mappings", &serialize_roxygen_mappings(mappings))
}

/// Runs reusable process-free properties over any valid Rust string.
pub fn check_arbitrary_source(source: &str) -> Result<(), String> {
    let config = r_parser::ParserConfig::default();
    let first = r_parser::parse_source(source, &config);
    let second = r_parser::parse_source(source, &config);
    if first.root().text() != source {
        return Err("R tree is not lossless".into());
    }
    if serialize_parse_snapshot(first.snapshot()) != serialize_parse_snapshot(second.snapshot()) {
        return Err("repeated R parsing is not deterministic".into());
    }
    if first
        .tokens()
        .iter()
        .map(|token| token.text(source))
        .collect::<String>()
        != source
    {
        return Err("parser token stream is not lossless".into());
    }
    let sidecar = r_roxygen::parse(first.snapshot());
    if sidecar.root().text() != sidecar.projected_source() {
        return Err("roxygen sidecar tree is not lossless".into());
    }
    Ok(())
}

/// Checks every prefix ending at a lexer token boundary.
pub fn check_token_boundary_truncations(source: &str) -> Result<(), String> {
    check_arbitrary_source("")?;
    let lexed = r_lexer::lex_default(source);
    for token in &lexed.tokens {
        let end = usize::from(token.range.end());
        check_arbitrary_source(&source[..end])?;
    }
    Ok(())
}

/// Checks that independent worker threads produce the same canonical snapshot.
pub fn check_concurrent_parsing(source: &str, workers: usize) -> Result<(), String> {
    if workers == 0 {
        return Err("worker count must be nonzero".into());
    }
    let expected = serialize_parse_snapshot(
        r_parser::parse_source(source, &r_parser::ParserConfig::default()).snapshot(),
    );
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let parsed = r_parser::parse_source(source, &r_parser::ParserConfig::default());
                    serialize_parse_snapshot(parsed.snapshot())
                })
            })
            .collect();
        for handle in handles {
            if handle.join().map_err(|_| "parse worker panicked")? != expected {
                return Err("concurrent R parsing is not deterministic".into());
            }
        }
        Ok(())
    })
}

fn serialized_fingerprint(domain: &[u8], bytes: &[u8]) -> Fingerprint {
    let mut hasher = StableHasher::new(domain);
    hasher.field(1, bytes);
    hasher.finish()
}

struct Canonical {
    bytes: Vec<u8>,
}

impl Canonical {
    fn new(domain: &[u8]) -> Self {
        let mut this = Self { bytes: Vec::new() };
        this.field(0, domain);
        this
    }
    fn field(&mut self, tag: u8, bytes: &[u8]) {
        self.bytes.push(tag);
        self.bytes
            .extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        self.bytes.extend_from_slice(bytes);
    }
    fn usize(&mut self, tag: u8, value: usize) {
        self.field(tag, &(value as u64).to_le_bytes());
    }
    fn boolean(&mut self, tag: u8, value: bool) {
        self.field(tag, &[u8::from(value)]);
    }
    fn range(&mut self, tag: u8, range: r_syntax::TextRange) {
        let mut bytes = [0; 8];
        bytes[..4].copy_from_slice(&u32::from(range.start()).to_le_bytes());
        bytes[4..].copy_from_slice(&u32::from(range.end()).to_le_bytes());
        self.field(tag, &bytes);
    }
    fn optional_range(&mut self, tag: u8, range: Option<r_syntax::TextRange>) {
        match range {
            Some(range) => {
                self.boolean(tag, true);
                self.range(tag, range);
            }
            None => self.boolean(tag, false),
        }
    }
    fn version(&mut self, tag: u8, version: r_syntax::Version) {
        let mut bytes = [0; 6];
        bytes[..2].copy_from_slice(&version.major.to_le_bytes());
        bytes[2..4].copy_from_slice(&version.minor.to_le_bytes());
        bytes[4..].copy_from_slice(&version.patch.to_le_bytes());
        self.field(tag, &bytes);
    }
    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

struct StableHasher {
    lanes: [u64; 4],
}

impl StableHasher {
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new(domain: &[u8]) -> Self {
        let mut hasher = Self {
            lanes: [
                0xcbf2_9ce4_8422_2325,
                0x8422_2325_cbf2_9ce4,
                0x9e37_79b9_7f4a_7c15,
                0x6a09_e667_f3bc_c909,
            ],
        };
        hasher.field(0, Fingerprint::ALGORITHM.as_bytes());
        hasher.field(0, domain);
        hasher
    }

    fn field(&mut self, tag: u8, bytes: &[u8]) {
        self.write(&[tag]);
        self.write(&(bytes.len() as u64).to_le_bytes());
        self.write(bytes);
    }

    fn write(&mut self, bytes: &[u8]) {
        for (lane_index, lane) in self.lanes.iter_mut().enumerate() {
            for byte in bytes {
                *lane ^= u64::from(*byte).wrapping_add((lane_index as u64) << 8);
                *lane = lane.wrapping_mul(Self::PRIME);
                *lane ^= *lane >> (29 + lane_index);
            }
        }
    }

    fn finish(self) -> Fingerprint {
        let mut bytes = [0; 32];
        for (index, lane) in self.lanes.into_iter().enumerate() {
            bytes[index * 8..(index + 1) * 8].copy_from_slice(&lane.to_le_bytes());
        }
        Fingerprint(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic(message: &str, start: u32) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            range: TextRange::new(start, start + 1).unwrap(),
            code: "P001".into(),
            message: message.into(),
            notes: vec!["note".into()],
        }
    }

    #[test]
    fn tree_text_and_boundaries_affect_fingerprint() {
        let first = [
            TreeEvent::StartNode { kind: "ROOT" },
            TreeEvent::Token {
                kind: "NAME",
                text: "ab",
            },
            TreeEvent::FinishNode,
        ];
        let second = [
            TreeEvent::StartNode { kind: "ROOT" },
            TreeEvent::Token {
                kind: "NAME",
                text: "a",
            },
            TreeEvent::Token {
                kind: "NAME",
                text: "b",
            },
            TreeEvent::FinishNode,
        ];
        assert_ne!(
            tree_fingerprint(&first).unwrap(),
            tree_fingerprint(&second).unwrap()
        );
    }

    #[test]
    fn malformed_tree_is_rejected() {
        let events = [TreeEvent::StartNode { kind: "ROOT" }];
        assert_eq!(
            tree_fingerprint(&events),
            Err(TreeFingerprintError::UnclosedNodes { count: 1 })
        );
    }

    #[test]
    fn diagnostic_set_ignores_collection_order() {
        let one = diagnostic("one", 0);
        let two = diagnostic("two", 2);
        assert_eq!(
            diagnostics_fingerprint(&[one.clone(), two.clone()]),
            diagnostics_fingerprint(&[two, one])
        );
    }

    #[test]
    fn case_ids_are_portable() {
        assert!(CaseId::new("parser/pipe-01.r").is_ok());
        assert!(CaseId::new("../outside").is_err());
        assert!(CaseId::new("spaces are bad").is_err());
    }

    #[test]
    fn fingerprint_hex_is_fixed_width() {
        assert_eq!(diagnostics_fingerprint(&[]).to_hex().len(), 64);
    }

    #[test]
    fn arbitrary_string_harness_covers_generated_utf8() {
        let alphabet = ['a', ' ', '\n', '\'', '`', '+', '#', '\u{0}', '\u{3bb}'];
        let mut state = 0x1234_5678_u64;
        for length in 0..64 {
            let mut source = String::new();
            for _ in 0..length {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                source.push(alphabet[(state as usize) % alphabet.len()]);
            }
            check_arbitrary_source(&source).unwrap();
        }
    }

    #[test]
    fn truncation_and_concurrency_harnesses_are_reusable() {
        let source = "#' @param x value\nf <- function(x) x + 1\n";
        check_token_boundary_truncations(source).unwrap();
        check_concurrent_parsing(source, 4).unwrap();
    }

    #[test]
    fn frozen_process_free_fixtures_match_canonical_adapters() {
        let mut actual = Vec::new();
        for (name, source) in [
            ("precedence", include_str!("../fixtures/precedence.r")),
            ("malformed", include_str!("../fixtures/malformed.r")),
            ("incomplete", include_str!("../fixtures/incomplete.r")),
            ("lossless", include_str!("../fixtures/lossless.r")),
            (
                "roxygen-grouping",
                include_str!("../fixtures/roxygen-grouping.r"),
            ),
            (
                "roxygen-tags-fences",
                include_str!("../fixtures/roxygen-tags-fences.r"),
            ),
            ("roxygen-null", include_str!("../fixtures/roxygen-null.r")),
        ] {
            let parsed = r_parser::parse_source(source, &r_parser::ParserConfig::default());
            let fingerprints = parse_snapshot_fingerprints(parsed.snapshot());
            let sidecar = r_roxygen::parse_sidecar(
                parsed.snapshot(),
                &r_roxygen::RoxygenConfig {
                    parse_examples: true,
                    ..r_roxygen::RoxygenConfig::default()
                },
            );
            let mappings = sidecar
                .embedded_parses()
                .iter()
                .flat_map(|embedded| embedded.mappings().iter().cloned())
                .collect::<Vec<_>>();
            match name {
                "roxygen-grouping" => {
                    assert_eq!(sidecar.blocks().len(), 2);
                    assert_eq!(
                        sidecar
                            .associations()
                            .iter()
                            .map(|association| association.state)
                            .collect::<Vec<_>>(),
                        [
                            r_roxygen::AssociationState::Normal,
                            r_roxygen::AssociationState::Detached,
                        ]
                    );
                }
                "roxygen-tags-fences" => {
                    assert!(!mappings.is_empty());
                    assert!(sidecar
                        .root()
                        .descendants_with_tokens()
                        .any(|element| { element.kind() == r_roxygen::RoxygenKind::CODE_BLOCK }));
                }
                "roxygen-null" => assert_eq!(
                    sidecar
                        .associations()
                        .iter()
                        .map(|association| association.state)
                        .collect::<Vec<_>>(),
                    [
                        r_roxygen::AssociationState::TerminatedByNull,
                        r_roxygen::AssociationState::Normal,
                    ]
                ),
                _ => {}
            }
            actual.push(format!(
                "{name}\t{:?}\t{}\t{}\t{}\t{}\t{}",
                parsed.status(),
                fingerprints.snapshot,
                fingerprints.diagnostics,
                roxygen_sidecar_fingerprint(&sidecar),
                roxygen_associations_fingerprint(sidecar.associations()),
                roxygen_mappings_fingerprint(&mappings),
            ));
        }
        let expected: Vec<_> = include_str!("../fixtures/cases.tsv")
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .collect();
        assert_eq!(actual, expected);
    }
}
