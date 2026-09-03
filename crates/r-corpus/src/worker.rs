//! Data-only worker protocol and the isolated parser implementation.

use std::{
    fs,
    io::{self, Read, Write},
    path::PathBuf,
    time::Instant,
};

use r_conformance::{
    parse_snapshot_fingerprints, roxygen_associations_fingerprint, roxygen_mappings_fingerprint,
    roxygen_sidecar_fingerprint,
};
use r_syntax::{NodeOrToken, SyntaxKind, TextRange, TextSize};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_PARSER_CONFIG: &str = "r-parser-default-v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceIdentity {
    /// Path to an already decoded UTF-8 source file.
    pub path: PathBuf,
    /// Lowercase SHA-256 of the decoded bytes.
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParserIdentity {
    /// Source-control revision of the parser under test.
    pub commit: String,
    /// Stable description of all parser choices.
    pub config: String,
    /// SHA-256 of the stable parser configuration description.
    pub config_sha256: String,
}

impl ParserIdentity {
    pub fn new(commit: impl Into<String>, config: impl Into<String>) -> Self {
        let config = config.into();
        Self {
            commit: commit.into(),
            config_sha256: sha256_hex(config.as_bytes()),
            config,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoxygenMode {
    Disabled,
    WhenPresent,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRequest {
    pub protocol_version: u32,
    pub decoded_source: SourceIdentity,
    pub parser: ParserIdentity,
    #[serde(default = "default_roxygen_mode")]
    pub roxygen: RoxygenMode,
}

const fn default_roxygen_mode() -> RoxygenMode {
    RoxygenMode::WhenPresent
}

impl WorkerRequest {
    pub fn new(decoded_source: SourceIdentity, parser: ParserIdentity) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            decoded_source,
            parser,
            roxygen: RoxygenMode::WhenPresent,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkerResponse {
    pub protocol_version: u32,
    pub decoded_source: Option<SourceIdentity>,
    pub parser: Option<ParserIdentity>,
    pub roxygen: Option<RoxygenMode>,
    #[serde(flatten)]
    pub outcome: WorkerOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WorkerOutcome {
    Parsed { report: Box<ParseReport> },
    Failed { error: WorkerError },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkerError {
    pub kind: WorkerErrorKind,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerErrorKind {
    InvalidRequest,
    UnsupportedProtocol,
    UnsupportedConfig,
    ReadSource,
    HashMismatch,
    InvalidUtf8,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParserStatus {
    Empty,
    Complete,
    Incomplete,
    Invalid,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ByteRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveryRecord {
    pub kind: String,
    pub range: ByteRange,
    pub token: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiagnosticRecord {
    pub code: String,
    pub severity: String,
    pub range: ByteRange,
    pub message: String,
    pub recovery: RecoveryRecord,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Fingerprints {
    pub algorithm: String,
    pub snapshot: String,
    pub tree: String,
    pub diagnostics: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ValidationReport {
    pub root_lossless: bool,
    pub token_concatenation_lossless: bool,
    pub ranges_valid: bool,
    pub snapshot_valid: bool,
    pub snapshot_errors: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RoxygenReport {
    pub sidecar: String,
    pub associations: String,
    pub mappings: String,
    pub block_count: usize,
    pub diagnostic_count: usize,
    pub limited: bool,
    pub discovery_incomplete: bool,
    pub diagnostics_truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParseReport {
    pub source_bytes: usize,
    pub parse_nanos: u64,
    pub status: ParserStatus,
    pub resource_limited: bool,
    pub diagnostics_truncated: bool,
    pub diagnostics: Vec<DiagnosticRecord>,
    pub node_count: usize,
    pub token_count: usize,
    pub tree_token_count: usize,
    pub fingerprints: Fingerprints,
    pub validation: ValidationReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roxygen: Option<RoxygenReport>,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

/// Handles one validated request without performing any process or network work.
pub fn process_request(request: WorkerRequest) -> WorkerResponse {
    let source_identity = request.decoded_source.clone();
    let parser_identity = request.parser.clone();
    let result = process_request_inner(&request);
    WorkerResponse {
        protocol_version: PROTOCOL_VERSION,
        decoded_source: Some(source_identity),
        parser: Some(parser_identity),
        roxygen: Some(request.roxygen),
        outcome: match result {
            Ok(report) => WorkerOutcome::Parsed {
                report: Box::new(report),
            },
            Err(error) => WorkerOutcome::Failed { error },
        },
    }
}

fn process_request_inner(request: &WorkerRequest) -> Result<ParseReport, WorkerError> {
    if request.protocol_version != PROTOCOL_VERSION {
        return Err(worker_error(
            WorkerErrorKind::UnsupportedProtocol,
            format!(
                "protocol version {} is unsupported; expected {PROTOCOL_VERSION}",
                request.protocol_version
            ),
        ));
    }
    if request.parser.config != DEFAULT_PARSER_CONFIG {
        return Err(worker_error(
            WorkerErrorKind::UnsupportedConfig,
            format!(
                "parser config {:?} is unsupported; expected {DEFAULT_PARSER_CONFIG:?}",
                request.parser.config
            ),
        ));
    }
    if request.parser.config_sha256 != sha256_hex(request.parser.config.as_bytes()) {
        return Err(worker_error(
            WorkerErrorKind::UnsupportedConfig,
            "parser configuration hash does not match its description".into(),
        ));
    }
    let bytes = fs::read(&request.decoded_source.path).map_err(|error| {
        worker_error(
            WorkerErrorKind::ReadSource,
            format!("failed to read decoded source: {error}"),
        )
    })?;
    let actual_hash = sha256_hex(&bytes);
    if request.decoded_source.sha256 != actual_hash {
        return Err(worker_error(
            WorkerErrorKind::HashMismatch,
            format!(
                "decoded source SHA-256 mismatch: expected {}, got {actual_hash}",
                request.decoded_source.sha256
            ),
        ));
    }
    let source = std::str::from_utf8(&bytes).map_err(|error| {
        worker_error(
            WorkerErrorKind::InvalidUtf8,
            format!("decoded source is not UTF-8: {error}"),
        )
    })?;

    let started = Instant::now();
    let parsed = r_parser::parse_source(source, &r_parser::ParserConfig::default());
    let parse_nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let root = parsed.root();
    let node_count = root.descendants().count();
    let tree_token_count = root
        .descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .count();
    let source_end = TextSize::of(source);
    let ranges_valid = parsed.tokens().iter().all(|token| {
        valid_range(token.range, source_end)
            && source.is_char_boundary(usize::from(token.range.start()))
            && source.is_char_boundary(usize::from(token.range.end()))
    }) && parsed.snapshot().diagnostics().iter().all(|diagnostic| {
        valid_range(diagnostic.range, source_end)
            && valid_range(diagnostic.recovery.range, source_end)
    });
    let snapshot_errors = r_syntax::validate_snapshot(parsed.snapshot())
        .err()
        .unwrap_or_default()
        .into_iter()
        .map(|error| format!("{:?}: {}", error.kind, error.message))
        .collect::<Vec<_>>();
    let snapshot_fingerprints = parse_snapshot_fingerprints(parsed.snapshot());
    let diagnostics = parsed
        .snapshot()
        .diagnostics()
        .iter()
        .map(|diagnostic| DiagnosticRecord {
            code: diagnostic.code.as_str().to_owned(),
            severity: format!("{:?}", diagnostic.severity),
            range: byte_range(diagnostic.range),
            message: diagnostic.message.clone(),
            recovery: RecoveryRecord {
                kind: format!("{:?}", diagnostic.recovery.kind),
                range: byte_range(diagnostic.recovery.range),
                token: diagnostic.recovery.token.map(|kind| format!("{kind:?}")),
            },
        })
        .collect();
    let roxygen = if request.roxygen == RoxygenMode::WhenPresent
        && parsed
            .tokens()
            .iter()
            .any(|token| token.kind == SyntaxKind::ROXYGEN_COMMENT)
    {
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
        let status = sidecar.status();
        Some(RoxygenReport {
            sidecar: roxygen_sidecar_fingerprint(&sidecar).to_hex(),
            associations: roxygen_associations_fingerprint(sidecar.associations()).to_hex(),
            mappings: roxygen_mappings_fingerprint(&mappings).to_hex(),
            block_count: sidecar.blocks().len(),
            diagnostic_count: sidecar.diagnostics().len(),
            limited: status.limited,
            discovery_incomplete: status.discovery_incomplete,
            diagnostics_truncated: status.diagnostics_truncated,
        })
    } else {
        None
    };

    Ok(ParseReport {
        source_bytes: bytes.len(),
        parse_nanos,
        status: match parsed.status() {
            r_parser::ParseStatus::Empty => ParserStatus::Empty,
            r_parser::ParseStatus::Complete => ParserStatus::Complete,
            r_parser::ParseStatus::Incomplete => ParserStatus::Incomplete,
            r_parser::ParseStatus::Invalid => ParserStatus::Invalid,
        },
        resource_limited: parsed.resource_limited(),
        diagnostics_truncated: parsed.diagnostics_truncated(),
        diagnostics,
        node_count,
        token_count: parsed.tokens().len(),
        tree_token_count,
        fingerprints: Fingerprints {
            algorithm: r_conformance::Fingerprint::ALGORITHM.to_owned(),
            snapshot: snapshot_fingerprints.snapshot.to_hex(),
            tree: snapshot_fingerprints.tree.to_hex(),
            diagnostics: snapshot_fingerprints.diagnostics.to_hex(),
        },
        validation: ValidationReport {
            root_lossless: root.text() == source,
            token_concatenation_lossless: parsed
                .tokens()
                .iter()
                .map(|token| token.text(source))
                .collect::<String>()
                == source,
            ranges_valid,
            snapshot_valid: snapshot_errors.is_empty(),
            snapshot_errors,
        },
        roxygen,
    })
}

fn valid_range(range: TextRange, source_end: TextSize) -> bool {
    range.start() <= range.end() && range.end() <= source_end
}

fn byte_range(range: TextRange) -> ByteRange {
    ByteRange {
        start: range.start().into(),
        end: range.end().into(),
    }
}

fn worker_error(kind: WorkerErrorKind, message: String) -> WorkerError {
    WorkerError { kind, message }
}

/// Reads exactly one JSON request and writes exactly one JSON response.
pub fn run_stdio() -> io::Result<()> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input)?;
    let response = match serde_json::from_slice::<WorkerRequest>(&input) {
        Ok(request) => process_request(request),
        Err(error) => WorkerResponse {
            protocol_version: PROTOCOL_VERSION,
            decoded_source: None,
            parser: None,
            roxygen: None,
            outcome: WorkerOutcome::Failed {
                error: worker_error(
                    WorkerErrorKind::InvalidRequest,
                    format!("invalid worker request: {error}"),
                ),
            },
        },
    };
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(&mut output, &response).map_err(io::Error::other)?;
    output.write_all(b"\n")?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_validates_a_decoded_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.r");
        let source = b"#' docs\nx <- 1 + 2\n";
        fs::write(&path, source).unwrap();
        let response = process_request(WorkerRequest::new(
            SourceIdentity {
                path,
                sha256: sha256_hex(source),
            },
            ParserIdentity::new("test", DEFAULT_PARSER_CONFIG),
        ));
        let WorkerOutcome::Parsed { report } = response.outcome else {
            panic!("worker did not parse source")
        };
        assert_eq!(report.status, ParserStatus::Complete);
        assert!(report.validation.root_lossless);
        assert!(report.validation.token_concatenation_lossless);
        assert!(report.validation.ranges_valid);
        assert!(report.validation.snapshot_valid);
        assert!(report.roxygen.is_some());
    }

    #[test]
    fn rejects_a_hash_mismatch_before_parsing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.r");
        fs::write(&path, "x").unwrap();
        let response = process_request(WorkerRequest::new(
            SourceIdentity {
                path,
                sha256: "0".repeat(64),
            },
            ParserIdentity::new("test", DEFAULT_PARSER_CONFIG),
        ));
        assert!(matches!(
            response.outcome,
            WorkerOutcome::Failed {
                error: WorkerError {
                    kind: WorkerErrorKind::HashMismatch,
                    ..
                }
            }
        ));
    }
}
