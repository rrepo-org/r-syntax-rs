//! Process-free roxygen sidecars for lossless R parse trees.
//!
//! A sidecar projects runs of host `ROXYGEN_COMMENT` tokens into an independent
//! Rowan tree. It never changes the host parse and never executes documentation.
#![forbid(unsafe_code)]
#![allow(non_camel_case_types)]

use std::{ops::Range, sync::Arc};

#[cfg(test)]
use r_lexer::{lex, LexerConfig};
use r_source::CompatibilityProfile;
use r_syntax::{
    DiagnosticCode, DocumentId, GreenNode, GreenNodeBuilder, NodeOrToken, ParseSnapshot, Severity,
    SyntaxKind, SyntaxProfile, TextRange, TextSize, Version,
};

const UNEXPECTED_TOKEN: DiagnosticCode = DiagnosticCode::new("R-ROXYGEN-001");
const EXPECTED_TOKEN: DiagnosticCode = DiagnosticCode::new("R-ROXYGEN-002");
const UNTERMINATED_FRAGMENT: DiagnosticCode = DiagnosticCode::new("R-ROXYGEN-003");
const RESOURCE_LIMIT: DiagnosticCode = DiagnosticCode::new("R-ROXYGEN-004");

macro_rules! kinds {
    ($($name:ident = $value:literal),+ $(,)?) => {
        /// Stable token and node tags for the roxygen sidecar language.
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        #[repr(u16)]
        pub enum RoxygenKind { $($name = $value),+ }

        impl RoxygenKind {
            pub const fn as_u16(self) -> u16 { self as u16 }
            pub const fn from_u16(value: u16) -> Option<Self> {
                match value { $($value => Some(Self::$name),)+ _ => None }
            }
            pub const fn is_token(self) -> bool { self.as_u16() < 256 }
            pub const fn is_node(self) -> bool { !self.is_token() }
        }
    };
}

kinds! {
    LINE_PREFIX = 1,
    WHITESPACE = 2,
    NEWLINE = 3,
    TAG_MARK = 4,
    TAG_NAME = 5,
    TEXT = 6,
    IDENTIFIER = 7,
    COMMA = 8,
    COLON = 9,
    L_PAREN = 10,
    R_PAREN = 11,
    L_BRACKET = 12,
    R_BRACKET = 13,
    CODE_SPAN = 14,
    CODE_BLOCK = 15,
    ERROR_TOKEN = 16,
    MISSING = 17,

    SIDECAR = 256,
    ROXYGEN_BLOCK = 257,
    ROXYGEN_LINE = 258,
    TAG = 259,
    TAG_BODY = 260,
    PARAM_BODY = 261,
    NAME_LIST = 262,
    SECTION_BODY = 263,
    CODE_BODY = 264,
    GENERIC_BODY = 265,
    DESCRIPTION = 266,
    INLINE = 267,
    LINK = 268,
    PUNCTUATION = 269,
    ERROR = 270,
    OPAQUE_BODY = 271,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RoxygenLanguage {}

impl rowan::Language for RoxygenLanguage {
    type Kind = RoxygenKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
        RoxygenKind::from_u16(raw.0).unwrap_or(RoxygenKind::ERROR_TOKEN)
    }

    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind.as_u16())
    }
}

pub type RoxygenNode = rowan::SyntaxNode<RoxygenLanguage>;
pub type RoxygenToken = rowan::SyntaxToken<RoxygenLanguage>;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RoxygenLimits {
    pub max_blocks: usize,
    pub max_lines: usize,
    pub max_tokens: usize,
    pub max_diagnostics: usize,
    pub max_depth: usize,
}

impl RoxygenLimits {
    pub const DEFAULT: Self = Self {
        max_blocks: 100_000,
        max_lines: 1_000_000,
        max_tokens: 8_000_000,
        max_diagnostics: 10_000,
        max_depth: 256,
    };
    pub const UNLIMITED: Self = Self {
        max_blocks: usize::MAX,
        max_lines: usize::MAX,
        max_tokens: usize::MAX,
        max_diagnostics: usize::MAX,
        max_depth: usize::MAX,
    };
}

impl Default for RoxygenLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Parsing strategy for a registered tag body.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BodyMode {
    Generic,
    Parameter,
    Section,
    Code,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TagDefinition {
    name: Arc<str>,
    body_mode: BodyMode,
}

impl TagDefinition {
    pub fn new(name: impl Into<Arc<str>>, body_mode: BodyMode) -> Self {
        Self {
            name: name.into(),
            body_mode,
        }
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn body_mode(&self) -> BodyMode {
        self.body_mode
    }
}

/// Immutable tag vocabulary. Extending a registry returns a new value.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TagRegistry {
    version: Version,
    definitions: Arc<[TagDefinition]>,
}

impl TagRegistry {
    pub fn roxygen2_8_1_0() -> Self {
        const PARAMETER: &[&str] = &["param"];
        const SECTION: &[&str] = &["section"];
        const CODE: &[&str] = &["eval", "example", "examples", "examplesIf"];
        const GENERIC: &[&str] = &[
            "aliases",
            "author",
            "backref",
            "concept",
            "describeIn",
            "description",
            "details",
            "docType",
            "evalRd",
            "export",
            "exportClass",
            "exportMethod",
            "exportPattern",
            "exportS3Method",
            "family",
            "field",
            "format",
            "import",
            "importClassesFrom",
            "importFrom",
            "importMethodsFrom",
            "include",
            "includeRmd",
            "inherit",
            "inheritDotParams",
            "inheritParams",
            "inheritSection",
            "keywords",
            "md",
            "method",
            "name",
            "noMd",
            "noRd",
            "note",
            "order",
            "rawNamespace",
            "rawRd",
            "rdname",
            "references",
            "return",
            "returns",
            "seealso",
            "slot",
            "source",
            "template",
            "templateVar",
            "title",
            "usage",
            "useDynLib",
        ];
        let definitions = PARAMETER
            .iter()
            .map(|name| TagDefinition::new(*name, BodyMode::Parameter))
            .chain(
                SECTION
                    .iter()
                    .map(|name| TagDefinition::new(*name, BodyMode::Section)),
            )
            .chain(
                CODE.iter()
                    .map(|name| TagDefinition::new(*name, BodyMode::Code)),
            )
            .chain(
                GENERIC
                    .iter()
                    .map(|name| TagDefinition::new(*name, BodyMode::Generic)),
            )
            .collect::<Vec<_>>();
        Self {
            version: Version::new(8, 1, 0),
            definitions: definitions.into(),
        }
    }
    pub fn version(&self) -> Version {
        self.version
    }
    pub fn definitions(&self) -> &[TagDefinition] {
        &self.definitions
    }
    pub fn get(&self, name: &str) -> Option<&TagDefinition> {
        self.definitions.iter().find(|item| item.name() == name)
    }
    pub fn extended(&self, definition: TagDefinition) -> Self {
        let mut definitions = self
            .definitions
            .iter()
            .filter(|item| item.name() != definition.name())
            .cloned()
            .collect::<Vec<_>>();
        definitions.push(definition);
        Self {
            version: self.version,
            definitions: definitions.into(),
        }
    }
}

impl Default for TagRegistry {
    fn default() -> Self {
        Self::roxygen2_8_1_0()
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RoxygenConfig {
    pub document: DocumentId,
    pub compatibility: CompatibilityProfile,
    pub profile: SyntaxProfile,
    pub limits: RoxygenLimits,
    pub registry: TagRegistry,
    /// Parse `@examples` bodies with `r-parser`; this never evaluates code.
    pub parse_examples: bool,
}

impl Default for RoxygenConfig {
    fn default() -> Self {
        Self {
            document: DocumentId(0),
            compatibility: CompatibilityProfile::default(),
            profile: SyntaxProfile::default(),
            limits: RoxygenLimits::default(),
            registry: TagRegistry::default(),
            parse_examples: false,
        }
    }
}

/// A projected token and its location in both coordinate spaces.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ProjectedToken {
    pub kind: RoxygenKind,
    pub projected_range: TextRange,
    pub host_range: TextRange,
}

/// Metadata for one maximal newline-separated run of roxygen comments.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RoxygenBlock {
    pub host_range: TextRange,
    pub projected_range: TextRange,
    pub line_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AssociationState {
    Normal,
    Detached,
    TerminatedByNull,
    Ambiguous,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct BlockAssociation {
    pub block_index: usize,
    pub state: AssociationState,
    pub expression_range: Option<TextRange>,
    pub intervening_host_range: TextRange,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BoundaryAffinity {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct RoxygenStatus {
    /// Scanning or tree construction met a configured resource limit.
    pub limited: bool,
    /// Some roxygen comments could not be discovered from the host token stream.
    pub discovery_incomplete: bool,
    /// The diagnostic cap suppressed at least one diagnostic.
    pub diagnostics_truncated: bool,
}

impl RoxygenStatus {
    pub const fn is_truncated(self) -> bool {
        self.limited || self.discovery_incomplete
    }
}

#[derive(Clone, Debug)]
pub struct EmbeddedRParse {
    pub block_index: usize,
    pub projected_range: TextRange,
    pub host_range: TextRange,
    snapshot: ParseSnapshot,
    mappings: Arc<[EmbeddedMapping]>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct EmbeddedMapping {
    pub embedded_range: TextRange,
    pub projected_range: TextRange,
    pub host_range: TextRange,
}

impl EmbeddedRParse {
    pub fn snapshot(&self) -> &ParseSnapshot {
        &self.snapshot
    }
    pub fn mappings(&self) -> &[EmbeddedMapping] {
        &self.mappings
    }
    pub fn host_point(&self, point: TextSize, affinity: BoundaryAffinity) -> Option<TextSize> {
        let candidate = match affinity {
            BoundaryAffinity::Left => self.mappings.iter().rev().find(|mapping| {
                mapping.embedded_range.start() < point && point <= mapping.embedded_range.end()
            }),
            BoundaryAffinity::Right => self.mappings.iter().find(|mapping| {
                mapping.embedded_range.start() <= point && point < mapping.embedded_range.end()
            }),
        }?;
        Some(candidate.host_range.start() + (point - candidate.embedded_range.start()))
    }
    pub fn host_range(&self, range: TextRange) -> Option<TextRange> {
        Some(TextRange::new(
            self.host_point(range.start(), BoundaryAffinity::Right)?,
            self.host_point(range.end(), BoundaryAffinity::Left)?,
        ))
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RoxygenDiagnostic {
    pub code: DiagnosticCode,
    pub severity: Severity,
    pub range: TextRange,
    pub message: String,
}

/// Immutable sidecar output. The green tree and all metadata are thread-safe.
#[derive(Clone, Debug)]
pub struct RoxygenParse {
    document: DocumentId,
    profile: SyntaxProfile,
    projected: Arc<str>,
    green: GreenNode,
    tokens: Arc<[ProjectedToken]>,
    blocks: Arc<[RoxygenBlock]>,
    diagnostics: Arc<[RoxygenDiagnostic]>,
    config: RoxygenConfig,
    associations: Arc<[BlockAssociation]>,
    embedded: Arc<[EmbeddedRParse]>,
    status: RoxygenStatus,
}

impl RoxygenParse {
    pub fn document(&self) -> DocumentId {
        self.document
    }
    pub fn profile(&self) -> SyntaxProfile {
        self.profile
    }
    pub fn projected_source(&self) -> &str {
        &self.projected
    }
    pub fn green(&self) -> &GreenNode {
        &self.green
    }
    pub fn root(&self) -> RoxygenNode {
        RoxygenNode::new_root(self.green.clone())
    }
    pub fn tokens(&self) -> &[ProjectedToken] {
        &self.tokens
    }
    pub fn blocks(&self) -> &[RoxygenBlock] {
        &self.blocks
    }
    pub fn diagnostics(&self) -> &[RoxygenDiagnostic] {
        &self.diagnostics
    }
    pub fn config(&self) -> &RoxygenConfig {
        &self.config
    }
    pub fn associations(&self) -> &[BlockAssociation] {
        &self.associations
    }
    pub fn embedded_parses(&self) -> &[EmbeddedRParse] {
        &self.embedded
    }
    pub fn status(&self) -> RoxygenStatus {
        self.status
    }
    pub fn is_limited(&self) -> bool {
        self.status.limited
    }
    pub fn is_truncated(&self) -> bool {
        self.status.is_truncated()
    }
    pub fn is_valid(&self) -> bool {
        !self
            .diagnostics
            .iter()
            .any(|item| item.severity == Severity::Error)
    }

    /// Maps a projected byte offset using explicit ownership at token boundaries.
    pub fn host_point(&self, offset: TextSize, affinity: BoundaryAffinity) -> Option<TextSize> {
        map_point(&self.tokens, offset, affinity, false)
    }

    pub fn projected_point(
        &self,
        offset: TextSize,
        affinity: BoundaryAffinity,
    ) -> Option<TextSize> {
        map_point(&self.tokens, offset, affinity, true)
    }

    pub fn host_range(&self, range: TextRange) -> Option<TextRange> {
        self.host_range_with_affinity(range, BoundaryAffinity::Right, BoundaryAffinity::Left)
    }

    pub fn host_range_with_affinity(
        &self,
        range: TextRange,
        start_affinity: BoundaryAffinity,
        end_affinity: BoundaryAffinity,
    ) -> Option<TextRange> {
        Some(TextRange::new(
            self.host_point(range.start(), start_affinity)?,
            self.host_point(range.end(), end_affinity)?,
        ))
    }

    pub fn projected_range(&self, range: TextRange) -> Option<TextRange> {
        self.projected_range_with_affinity(range, BoundaryAffinity::Right, BoundaryAffinity::Left)
    }

    pub fn projected_range_with_affinity(
        &self,
        range: TextRange,
        start_affinity: BoundaryAffinity,
        end_affinity: BoundaryAffinity,
    ) -> Option<TextRange> {
        Some(TextRange::new(
            self.projected_point(range.start(), start_affinity)?,
            self.projected_point(range.end(), end_affinity)?,
        ))
    }

    /// Compatibility shorthand; interior boundaries have right affinity.
    pub fn host_offset(&self, offset: TextSize) -> Option<TextSize> {
        self.host_point(offset, BoundaryAffinity::Right)
    }
}

fn map_point(
    tokens: &[ProjectedToken],
    point: TextSize,
    affinity: BoundaryAffinity,
    reverse: bool,
) -> Option<TextSize> {
    let ranges = |token: &ProjectedToken| {
        if reverse {
            (token.host_range, token.projected_range)
        } else {
            (token.projected_range, token.host_range)
        }
    };
    let candidate = match affinity {
        BoundaryAffinity::Left => tokens.iter().rev().find(|token| {
            let (from, _) = ranges(token);
            from.start() < point && point <= from.end()
        }),
        BoundaryAffinity::Right => tokens.iter().find(|token| {
            let (from, _) = ranges(token);
            from.start() <= point && point < from.end()
        }),
    };
    if let Some(token) = candidate {
        let (from, to) = ranges(token);
        return Some(to.start() + (point - from.start()));
    }
    match affinity {
        BoundaryAffinity::Left => tokens.iter().rev().find_map(|token| {
            let (from, to) = ranges(token);
            (from.end() == point).then_some(to.end())
        }),
        BoundaryAffinity::Right => tokens.iter().find_map(|token| {
            let (from, to) = ranges(token);
            (from.start() == point).then_some(to.start())
        }),
    }
}

/// Lexes decoded R source and constructs its roxygen sidecar deterministically.
pub fn parse_source(source: &str, config: &RoxygenConfig) -> RoxygenParse {
    let host = r_parser::parse_source(
        source,
        &r_parser::ParserConfig {
            document: config.document,
            compatibility: config.compatibility,
            syntax_profile: config.profile,
            ..r_parser::ParserConfig::default()
        },
    );
    let host_tokens = host
        .snapshot()
        .root()
        .descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .map(|token| HostToken {
            kind: token.kind(),
            range: token.text_range(),
        })
        .collect::<Vec<_>>();
    build_sidecar(
        source,
        &host_tokens,
        config.document,
        config.profile,
        config,
        Some(host.snapshot()),
        host.resource_limited(),
    )
}

/// Constructs a sidecar from an existing host snapshot using default limits.
pub fn parse(host: &ParseSnapshot) -> RoxygenParse {
    parse_sidecar(
        host,
        &RoxygenConfig {
            document: host.document(),
            profile: host.profile(),
            ..RoxygenConfig::default()
        },
    )
}

/// Constructs an independent sidecar from an existing lossless host parse.
pub fn parse_sidecar(host: &ParseSnapshot, config: &RoxygenConfig) -> RoxygenParse {
    let host_tokens: Vec<_> = host
        .root()
        .descendants_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .map(|token| HostToken {
            kind: token.kind(),
            range: token.text_range(),
        })
        .collect();
    let effective = RoxygenConfig {
        document: host.document(),
        profile: host.profile(),
        ..config.clone()
    };
    build_sidecar(
        host.source(),
        &host_tokens,
        host.document(),
        host.profile(),
        &effective,
        Some(host),
        false,
    )
}

#[derive(Clone, Copy)]
struct HostToken {
    kind: SyntaxKind,
    range: TextRange,
}

fn build_sidecar(
    source: &str,
    host_tokens: &[HostToken],
    document: DocumentId,
    profile: SyntaxProfile,
    config: &RoxygenConfig,
    host: Option<&ParseSnapshot>,
    upstream_discovery_incomplete: bool,
) -> RoxygenParse {
    let discovery_incomplete = upstream_discovery_incomplete
        || host.is_some_and(|snapshot| {
            snapshot.root().text_range().end() != TextSize::of(snapshot.source())
        });
    let (runs, scan_limited) = discover_runs(host_tokens, config.limits);
    let mut projected = String::new();
    let mut tokens = Vec::new();
    let mut blocks = Vec::new();
    let mut diagnostics = Vec::new();
    let mut diagnostics_truncated = false;
    let mut line_ranges = Vec::new();
    let mut limited = scan_limited;
    let mut state = PayloadState::default();

    for run in runs {
        let projected_start = TextSize::of(projected.as_str());
        let line_start = line_ranges.len();
        let run_line_count = run.len();
        for (line_index, &host_index) in run.iter().enumerate() {
            let host_token = &host_tokens[host_index];
            let comment = source_text(source, host_token.range);
            let first = tokens.len();
            push_token(
                &mut projected,
                &mut tokens,
                RoxygenKind::LINE_PREFIX,
                &comment[..2],
                TextRange::new(
                    host_token.range.start(),
                    host_token.range.start() + TextSize::from(2),
                ),
            );
            let payload = &comment[2..];
            let mode = state.mode(payload, &config.registry);
            let before = tokens.len();
            lex_payload(
                payload,
                host_token.range.start() + TextSize::from(2),
                &mut projected,
                &mut tokens,
                &mut diagnostics,
                &mut diagnostics_truncated,
                config.limits,
                &config.registry,
                mode,
            );
            limited |= tokens[before..]
                .iter()
                .any(|token| token.kind == RoxygenKind::ERROR_TOKEN)
                && tokens.len() >= config.limits.max_tokens;
            line_ranges.push(first..tokens.len());

            if line_index + 1 < run_line_count {
                let newline = &host_tokens[host_index + 1];
                push_token(
                    &mut projected,
                    &mut tokens,
                    RoxygenKind::NEWLINE,
                    source_text(source, newline.range),
                    newline.range,
                );
            }
        }
        let first_host = host_tokens[run[0]].range.start();
        let last_host = host_tokens[*run.last().expect("run is non-empty")]
            .range
            .end();
        blocks.push(RoxygenBlock {
            host_range: TextRange::new(first_host, last_host),
            projected_range: TextRange::new(projected_start, TextSize::of(projected.as_str())),
            line_count: line_ranges.len() - line_start,
        });
    }

    if limited {
        diagnostics_truncated |= diagnostic(
            &mut diagnostics,
            config.limits,
            RESOURCE_LIMIT,
            TextRange::empty(TextSize::of(source)),
            "roxygen sidecar resource limit reached",
        );
    }

    let green = TreeParser::new(
        &projected,
        &tokens,
        &line_ranges,
        &blocks,
        &mut diagnostics,
        &mut diagnostics_truncated,
        config.limits,
        &config.registry,
        &mut limited,
    )
    .finish();
    let associations = host.map_or_else(Vec::new, |snapshot| associate(snapshot, &blocks));
    let embedded = if config.parse_examples {
        embedded_examples(
            &projected,
            &tokens,
            &blocks,
            document,
            profile,
            config.compatibility,
            &config.registry,
        )
    } else {
        Vec::new()
    };
    RoxygenParse {
        document,
        profile,
        projected: projected.into(),
        green,
        tokens: tokens.into(),
        blocks: blocks.into(),
        diagnostics: diagnostics.into(),
        config: config.clone(),
        associations: associations.into(),
        embedded: embedded.into(),
        status: RoxygenStatus {
            limited,
            discovery_incomplete,
            diagnostics_truncated,
        },
    }
}

fn discover_runs(tokens: &[HostToken], limits: RoxygenLimits) -> (Vec<Vec<usize>>, bool) {
    let mut runs = Vec::new();
    let mut index = 0;
    let mut lines = 0;
    let mut limited = false;
    while index < tokens.len() {
        if tokens[index].kind != r_syntax::SyntaxKind::ROXYGEN_COMMENT {
            index += 1;
            continue;
        }
        if runs.len() >= limits.max_blocks || lines >= limits.max_lines {
            limited = true;
            break;
        }
        let mut run = vec![index];
        lines += 1;
        while let Some(newline) = tokens.get(index + 1) {
            if newline.kind != SyntaxKind::NEWLINE {
                break;
            }
            let mut next = index + 2;
            if tokens
                .get(next)
                .is_some_and(|token| token.kind == SyntaxKind::WHITESPACE)
            {
                next += 1;
            }
            if !tokens
                .get(next)
                .is_some_and(|token| token.kind == SyntaxKind::ROXYGEN_COMMENT)
            {
                break;
            }
            if lines >= limits.max_lines {
                limited = true;
                break;
            }
            index = next;
            run.push(index);
            lines += 1;
        }
        runs.push(run);
        index += 1;
    }
    (runs, limited)
}

fn associate(host: &ParseSnapshot, blocks: &[RoxygenBlock]) -> Vec<BlockAssociation> {
    let root = host.root();
    let expressions = root
        .children()
        .find(|node| node.kind() == SyntaxKind::EXPRESSION_LIST)
        .into_iter()
        .flat_map(|list| list.children())
        .filter(|node| {
            node.kind().is_node()
                && !matches!(
                    node.kind(),
                    SyntaxKind::ERROR | SyntaxKind::MISSING | SyntaxKind::EXPRESSION_LIST
                )
        })
        .collect::<Vec<_>>();
    blocks
        .iter()
        .enumerate()
        .map(|(block_index, block)| {
            let expression = expressions
                .iter()
                .find(|node| node.text_range().start() >= block.host_range.end());
            let expression_range = expression.map(|node| node.text_range());
            let next_block_before_expression = blocks.get(block_index + 1).is_some_and(|next| {
                expression_range.map_or(true, |range| next.host_range.start() < range.start())
            });
            let state = if next_block_before_expression {
                AssociationState::Ambiguous
            } else if let Some(node) = expression {
                if node
                    .descendants_with_tokens()
                    .filter_map(NodeOrToken::into_token)
                    .filter(|token| !token.kind().is_trivia())
                    .all(|token| token.kind() == SyntaxKind::NULL_KW)
                {
                    AssociationState::TerminatedByNull
                } else {
                    AssociationState::Normal
                }
            } else {
                AssociationState::Detached
            };
            let intervening_end =
                expression_range.map_or_else(|| TextSize::of(host.source()), |range| range.start());
            BlockAssociation {
                block_index,
                state,
                expression_range,
                intervening_host_range: TextRange::new(block.host_range.end(), intervening_end),
            }
        })
        .collect()
}

fn embedded_examples(
    projected: &str,
    tokens: &[ProjectedToken],
    blocks: &[RoxygenBlock],
    document: DocumentId,
    profile: SyntaxProfile,
    compatibility: CompatibilityProfile,
    registry: &TagRegistry,
) -> Vec<EmbeddedRParse> {
    let mut output = Vec::new();
    for (block_index, block) in blocks.iter().enumerate() {
        let block_tokens = tokens.iter().filter(|token| {
            token.projected_range.start() >= block.projected_range.start()
                && token.projected_range.end() <= block.projected_range.end()
        });
        let mut active = false;
        let mut source = String::new();
        let mut first = None::<&ProjectedToken>;
        let mut last = None::<&ProjectedToken>;
        let mut mappings = Vec::new();
        for token in block_tokens {
            let text = source_text(projected, token.projected_range);
            if token.kind == RoxygenKind::TAG_NAME {
                if active {
                    break;
                }
                active = registry
                    .get(text)
                    .is_some_and(|definition| definition.body_mode() == BodyMode::Code);
                continue;
            }
            if active && matches!(token.kind, RoxygenKind::CODE_BLOCK | RoxygenKind::NEWLINE) {
                first.get_or_insert(token);
                last = Some(token);
                let embedded_start = TextSize::of(source.as_str());
                source.push_str(text);
                mappings.push(EmbeddedMapping {
                    embedded_range: TextRange::new(embedded_start, TextSize::of(source.as_str())),
                    projected_range: token.projected_range,
                    host_range: token.host_range,
                });
            }
        }
        if let (Some(first), Some(last)) = (first, last) {
            let parsed = r_parser::parse_source(
                &source,
                &r_parser::ParserConfig {
                    document,
                    compatibility,
                    syntax_profile: profile,
                    ..r_parser::ParserConfig::default()
                },
            );
            output.push(EmbeddedRParse {
                block_index,
                projected_range: TextRange::new(
                    first.projected_range.start(),
                    last.projected_range.end(),
                ),
                host_range: TextRange::new(first.host_range.start(), last.host_range.end()),
                snapshot: parsed.into_snapshot(),
                mappings: mappings.into(),
            });
        }
    }
    output
}

fn source_text(source: &str, range: TextRange) -> &str {
    &source[usize::from(range.start())..usize::from(range.end())]
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PayloadMode {
    #[default]
    Normal,
    Code,
}

#[derive(Default)]
struct PayloadState {
    examples: bool,
    fence: Option<&'static str>,
}

impl PayloadState {
    fn mode(&mut self, payload: &str, registry: &TagRegistry) -> PayloadMode {
        let trimmed = payload.trim_start_matches(is_space);
        if let Some(fence) = self.fence {
            if trimmed.starts_with(fence) {
                self.fence = None;
            }
            return PayloadMode::Code;
        }
        if trimmed.starts_with("```") {
            self.fence = Some("```");
            return PayloadMode::Code;
        }
        if trimmed.starts_with("~~~") {
            self.fence = Some("~~~");
            return PayloadMode::Code;
        }
        let tag = line_tag(payload);
        if self.examples {
            if tag.map_or(true, |name| registry.get(name).is_none()) {
                return PayloadMode::Code;
            }
            self.examples = false;
        }
        if tag.is_some_and(|name| {
            registry
                .get(name)
                .is_some_and(|definition| definition.body_mode() == BodyMode::Code)
        }) {
            self.examples = true;
        }
        PayloadMode::Normal
    }
}

fn line_tag(text: &str) -> Option<&str> {
    let trimmed = text.trim_start_matches(is_space);
    let rest = trimmed.strip_prefix('@')?;
    let length = take_while(rest, is_tag_char);
    (length > 0).then_some(&rest[..length])
}

#[allow(clippy::too_many_arguments)]
fn lex_payload(
    text: &str,
    host_start: TextSize,
    projected: &mut String,
    tokens: &mut Vec<ProjectedToken>,
    diagnostics: &mut Vec<RoxygenDiagnostic>,
    diagnostics_truncated: &mut bool,
    limits: RoxygenLimits,
    registry: &TagRegistry,
    mode: PayloadMode,
) {
    let mut offset = 0;
    let tag_start = text.len() - text.trim_start_matches(is_space).len();
    let tag_line = (mode == PayloadMode::Normal)
        .then(|| line_tag(text))
        .flatten();
    let tag_body = if tag_line.is_some() {
        let name_start = tag_start + 1;
        let name_end = name_start
            + text[name_start..]
                .find(|ch: char| !is_tag_char(ch))
                .unwrap_or(text.len() - name_start);
        Some((name_end, &text[name_start..name_end]))
    } else {
        None
    };

    while offset < text.len() && tokens.len() < limits.max_tokens {
        let rest = &text[offset..];
        let (kind, length, unterminated) = if mode == PayloadMode::Code {
            (RoxygenKind::CODE_BLOCK, rest.len(), false)
        } else if rest.starts_with(char::is_whitespace) {
            (RoxygenKind::WHITESPACE, take_while(rest, is_space), false)
        } else if tag_line.is_some() && offset == tag_start && rest.starts_with('@') {
            (RoxygenKind::TAG_MARK, 1, false)
        } else if tag_body.is_some_and(|(end, _)| offset < end && offset > 0) {
            (RoxygenKind::TAG_NAME, take_while(rest, is_tag_char), false)
        } else if tag_body.is_some_and(|(end, name)| {
            offset >= end
                && registry
                    .get(name)
                    .is_some_and(|definition| definition.body_mode() == BodyMode::Code)
        }) || rest.starts_with("```")
            || rest.starts_with("~~~")
        {
            (RoxygenKind::CODE_BLOCK, rest.len(), false)
        } else if let Some(stripped) = rest.strip_prefix('`') {
            match stripped.find('`') {
                Some(end) => (RoxygenKind::CODE_SPAN, end + 2, false),
                None => (RoxygenKind::ERROR_TOKEN, rest.len(), true),
            }
        } else {
            let ch = rest.chars().next().expect("non-empty payload");
            let punctuation = match ch {
                ',' => Some(RoxygenKind::COMMA),
                ':' => Some(RoxygenKind::COLON),
                '(' => Some(RoxygenKind::L_PAREN),
                ')' => Some(RoxygenKind::R_PAREN),
                '[' => Some(RoxygenKind::L_BRACKET),
                ']' => Some(RoxygenKind::R_BRACKET),
                _ => None,
            };
            if let Some(kind) = punctuation {
                (kind, ch.len_utf8(), false)
            } else if is_identifier_char(ch) {
                (
                    RoxygenKind::IDENTIFIER,
                    take_while(rest, is_identifier_char),
                    false,
                )
            } else {
                (RoxygenKind::TEXT, take_while(rest, is_text_char), false)
            }
        };
        let length = length.max(rest.chars().next().map_or(0, char::len_utf8));
        let range = TextRange::new(
            host_start + size(offset),
            host_start + size(offset + length),
        );
        push_token(
            projected,
            tokens,
            kind,
            &text[offset..offset + length],
            range,
        );
        if unterminated {
            *diagnostics_truncated |= diagnostic(
                diagnostics,
                limits,
                UNTERMINATED_FRAGMENT,
                range,
                "unterminated inline code span",
            );
        }
        offset += length;
    }
    if offset < text.len() {
        let range = TextRange::new(host_start + size(offset), host_start + size(text.len()));
        push_token(
            projected,
            tokens,
            RoxygenKind::ERROR_TOKEN,
            &text[offset..],
            range,
        );
        *diagnostics_truncated |= diagnostic(
            diagnostics,
            limits,
            RESOURCE_LIMIT,
            range,
            "roxygen token limit reached",
        );
    }
}

fn push_token(
    projected: &mut String,
    tokens: &mut Vec<ProjectedToken>,
    kind: RoxygenKind,
    text: &str,
    host_range: TextRange,
) {
    let start = TextSize::of(projected.as_str());
    projected.push_str(text);
    tokens.push(ProjectedToken {
        kind,
        projected_range: TextRange::new(start, TextSize::of(projected.as_str())),
        host_range,
    });
}

fn take_while(text: &str, predicate: impl Fn(char) -> bool) -> usize {
    text.char_indices()
        .find_map(|(index, ch)| (!predicate(ch)).then_some(index))
        .unwrap_or(text.len())
}
fn is_space(ch: char) -> bool {
    ch != '\n' && ch != '\r' && ch.is_whitespace()
}
fn is_tag_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-')
}
fn is_identifier_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '.' | '-')
}
fn is_text_char(ch: char) -> bool {
    !ch.is_whitespace()
        && !is_identifier_char(ch)
        && !matches!(ch, ',' | ':' | '(' | ')' | '[' | ']' | '`')
}
fn size(value: usize) -> TextSize {
    TextSize::from(u32::try_from(value).unwrap_or(u32::MAX))
}

fn diagnostic(
    diagnostics: &mut Vec<RoxygenDiagnostic>,
    limits: RoxygenLimits,
    code: DiagnosticCode,
    range: TextRange,
    message: impl Into<String>,
) -> bool {
    if diagnostics.len() < limits.max_diagnostics {
        diagnostics.push(RoxygenDiagnostic {
            code,
            severity: Severity::Error,
            range,
            message: message.into(),
        });
        false
    } else {
        true
    }
}

struct TreeParser<'a> {
    projected: &'a str,
    tokens: &'a [ProjectedToken],
    lines: &'a [Range<usize>],
    blocks: &'a [RoxygenBlock],
    diagnostics: &'a mut Vec<RoxygenDiagnostic>,
    diagnostics_truncated: &'a mut bool,
    limits: RoxygenLimits,
    registry: &'a TagRegistry,
    limited: &'a mut bool,
    builder: GreenNodeBuilder<'static>,
    pos: usize,
    line: usize,
    depth: usize,
}

impl<'a> TreeParser<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        projected: &'a str,
        tokens: &'a [ProjectedToken],
        lines: &'a [Range<usize>],
        blocks: &'a [RoxygenBlock],
        diagnostics: &'a mut Vec<RoxygenDiagnostic>,
        diagnostics_truncated: &'a mut bool,
        limits: RoxygenLimits,
        registry: &'a TagRegistry,
        limited: &'a mut bool,
    ) -> Self {
        Self {
            projected,
            tokens,
            lines,
            blocks,
            diagnostics,
            diagnostics_truncated,
            limits,
            registry,
            limited,
            builder: GreenNodeBuilder::new(),
            pos: 0,
            line: 0,
            depth: 0,
        }
    }

    fn finish(mut self) -> GreenNode {
        self.start(RoxygenKind::SIDECAR);
        for block in self.blocks {
            self.start(RoxygenKind::ROXYGEN_BLOCK);
            let target = self.line + block.line_count;
            while self.line < target {
                self.parse_line();
                if self.line + 1 < target {
                    self.bump();
                }
                self.line += 1;
            }
            self.finish_node();
        }
        while self.pos < self.tokens.len() {
            self.bump();
        }
        self.finish_node();
        self.builder.finish()
    }

    fn parse_line(&mut self) {
        let end = self.lines[self.line].end;
        self.start(RoxygenKind::ROXYGEN_LINE);
        self.bump();
        if self.at(RoxygenKind::WHITESPACE) {
            self.bump();
        }
        if self.pos < end {
            if self.at(RoxygenKind::TAG_MARK) {
                self.parse_tag(end);
            } else {
                self.parse_description(end);
            }
        }
        self.finish_node();
    }

    fn parse_tag(&mut self, end: usize) {
        self.start(RoxygenKind::TAG);
        self.bump();
        let name = if self.at(RoxygenKind::TAG_NAME) {
            let value = self.text().to_owned();
            self.bump();
            value
        } else {
            self.missing("expected a tag name");
            String::new()
        };
        if self.at(RoxygenKind::WHITESPACE) {
            self.bump();
        }
        if self.pos < end {
            self.start(RoxygenKind::TAG_BODY);
            match self.registry.get(&name).map(TagDefinition::body_mode) {
                Some(BodyMode::Parameter) => self.parse_param(end),
                Some(BodyMode::Section) => self.parse_section(end),
                Some(BodyMode::Code) => self.parse_code(end),
                Some(BodyMode::Generic) => self.parse_generic(end),
                None => self.parse_opaque(end),
            }
            self.finish_node();
        }
        self.finish_node();
    }

    fn parse_param(&mut self, end: usize) {
        self.start(RoxygenKind::PARAM_BODY);
        self.start(RoxygenKind::NAME_LIST);
        if !self.at(RoxygenKind::IDENTIFIER) {
            self.missing("expected a parameter name");
        } else {
            self.bump();
            while self.pos < end && self.at(RoxygenKind::COMMA) {
                self.bump();
                if self.at(RoxygenKind::WHITESPACE) {
                    self.bump();
                }
                if self.at(RoxygenKind::IDENTIFIER) {
                    self.bump();
                } else {
                    self.missing("expected a parameter name after ','");
                }
            }
        }
        self.finish_node();
        if self.pos < end {
            if self.at(RoxygenKind::WHITESPACE) {
                self.bump();
            } else {
                self.unexpected("expected whitespace after parameter names");
            }
            if self.pos < end {
                self.parse_description(end);
            }
        }
        self.finish_node();
    }

    fn parse_section(&mut self, end: usize) {
        self.start(RoxygenKind::SECTION_BODY);
        while self.pos < end && !self.at(RoxygenKind::COLON) {
            self.parse_inline(end);
        }
        if self.at(RoxygenKind::COLON) {
            self.punctuation();
        } else {
            self.missing("expected ':' after section title");
        }
        if self.at(RoxygenKind::WHITESPACE) {
            self.bump();
        }
        if self.pos < end {
            self.parse_description(end);
        }
        self.finish_node();
    }

    fn parse_code(&mut self, end: usize) {
        self.start(RoxygenKind::CODE_BODY);
        while self.pos < end {
            self.bump();
        }
        self.finish_node();
    }

    fn parse_generic(&mut self, end: usize) {
        self.start(RoxygenKind::GENERIC_BODY);
        while self.pos < end {
            if self.at(RoxygenKind::WHITESPACE) {
                self.bump();
            } else {
                self.parse_inline(end);
            }
        }
        self.finish_node();
    }

    fn parse_opaque(&mut self, end: usize) {
        self.start(RoxygenKind::OPAQUE_BODY);
        while self.pos < end {
            self.bump();
        }
        self.finish_node();
    }

    fn parse_description(&mut self, end: usize) {
        self.start(RoxygenKind::DESCRIPTION);
        while self.pos < end {
            if self.at(RoxygenKind::WHITESPACE) {
                self.bump();
            } else {
                self.parse_inline(end);
            }
        }
        self.finish_node();
    }

    fn parse_inline(&mut self, end: usize) {
        if self.at(RoxygenKind::L_BRACKET) {
            if self.depth >= self.limits.max_depth {
                *self.limited = true;
                *self.diagnostics_truncated |= diagnostic(
                    self.diagnostics,
                    self.limits,
                    RESOURCE_LIMIT,
                    self.tokens[self.pos].host_range,
                    "roxygen nesting limit reached",
                );
                self.start(RoxygenKind::ERROR);
                while self.pos < end {
                    self.bump();
                }
                self.finish_node();
                return;
            }
            self.depth += 1;
            self.start(RoxygenKind::LINK);
            self.bump();
            while self.pos < end && !self.at(RoxygenKind::R_BRACKET) {
                if self.at(RoxygenKind::WHITESPACE) {
                    self.bump();
                } else {
                    self.parse_inline(end);
                }
            }
            if self.at(RoxygenKind::R_BRACKET) {
                self.bump();
            } else {
                self.missing("expected ']' to close link");
            }
            self.finish_node();
            self.depth -= 1;
        } else {
            self.parse_inline_atom();
        }
    }

    fn parse_inline_atom(&mut self) {
        if matches!(
            self.kind(),
            Some(
                RoxygenKind::COMMA
                    | RoxygenKind::COLON
                    | RoxygenKind::L_PAREN
                    | RoxygenKind::R_PAREN
            )
        ) {
            self.punctuation();
        } else if self.at(RoxygenKind::ERROR_TOKEN) || self.at(RoxygenKind::R_BRACKET) {
            self.start(RoxygenKind::ERROR);
            self.unexpected("unexpected or malformed roxygen token");
            self.finish_node();
        } else {
            self.start(RoxygenKind::INLINE);
            self.bump();
            self.finish_node();
        }
    }

    fn punctuation(&mut self) {
        self.start(RoxygenKind::PUNCTUATION);
        self.bump();
        self.finish_node();
    }
    fn unexpected(&mut self, message: &str) {
        let range = self.tokens.get(self.pos).map_or_else(
            || {
                TextRange::empty(
                    self.tokens
                        .last()
                        .map_or(TextSize::from(0), |token| token.host_range.end()),
                )
            },
            |token| token.host_range,
        );
        *self.diagnostics_truncated |= diagnostic(
            self.diagnostics,
            self.limits,
            UNEXPECTED_TOKEN,
            range,
            message,
        );
        if self.pos < self.tokens.len() {
            self.bump();
        }
    }
    fn missing(&mut self, message: &str) {
        let at = self.tokens.get(self.pos).map_or_else(
            || {
                self.tokens
                    .last()
                    .map_or(TextSize::from(0), |token| token.host_range.end())
            },
            |token| token.host_range.start(),
        );
        *self.diagnostics_truncated |= diagnostic(
            self.diagnostics,
            self.limits,
            EXPECTED_TOKEN,
            TextRange::empty(at),
            message,
        );
        self.builder
            .token(rowan::SyntaxKind(RoxygenKind::MISSING.as_u16()), "");
    }
    fn at(&self, kind: RoxygenKind) -> bool {
        self.kind() == Some(kind)
    }
    fn kind(&self) -> Option<RoxygenKind> {
        self.tokens.get(self.pos).map(|token| token.kind)
    }
    fn text(&self) -> &str {
        let range = self.tokens[self.pos].projected_range;
        &self.projected[usize::from(range.start())..usize::from(range.end())]
    }
    fn bump(&mut self) {
        let token = &self.tokens[self.pos];
        let range = token.projected_range;
        let text = &self.projected[usize::from(range.start())..usize::from(range.end())];
        self.builder
            .token(rowan::SyntaxKind(token.kind.as_u16()), text);
        self.pos += 1;
    }
    fn start(&mut self, kind: RoxygenKind) {
        self.builder.start_node(rowan::SyntaxKind(kind.as_u16()));
    }
    fn finish_node(&mut self) {
        self.builder.finish_node();
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use r_syntax::{Completeness, ParseSnapshot};
    use rowan::NodeOrToken;

    use super::*;

    fn sidecar(source: &str) -> RoxygenParse {
        parse_source(source, &RoxygenConfig::default())
    }

    fn host_snapshot(source: &str) -> ParseSnapshot {
        let lexed = lex(source, &LexerConfig::default());
        let mut builder = GreenNodeBuilder::new();
        builder.start_node(rowan::SyntaxKind(SyntaxKind::SOURCE_FILE.as_u16()));
        for token in lexed.tokens {
            builder.token(
                rowan::SyntaxKind(token.kind.as_u16()),
                source_text(source, token.range),
            );
        }
        builder.finish_node();
        ParseSnapshot::new(
            DocumentId(42),
            Arc::<str>::from(source),
            builder.finish(),
            [],
            Completeness::Complete,
            SyntaxProfile::default(),
        )
    }

    #[test]
    fn discovers_only_adjacent_roxygen_runs_without_touching_host() {
        let source = "# ordinary\n#' title\n  #' details\nx <- 1\n  #' indented\n";
        let host = host_snapshot(source);
        let before = host.root().text().to_string();
        let parsed = parse(&host);
        assert_eq!(parsed.blocks().len(), 2);
        assert_eq!(parsed.blocks()[0].line_count, 2);
        assert_eq!(parsed.projected_source(), "#' title\n#' details#' indented");
        assert_eq!(host.root().text().to_string(), before);
    }

    #[test]
    fn closes_the_structural_grammar_losslessly() {
        let source = "#' A [link] and `code`.\n#' @param x, y values\n#' @section Details: More text\n#' @examples f(x)\n";
        let parsed = sidecar(source);
        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.root().text().to_string(), parsed.projected_source());
        let kinds: Vec<_> = parsed
            .root()
            .descendants()
            .map(|node| node.kind())
            .collect();
        for kind in [
            RoxygenKind::LINK,
            RoxygenKind::PARAM_BODY,
            RoxygenKind::NAME_LIST,
            RoxygenKind::SECTION_BODY,
            RoxygenKind::CODE_BODY,
        ] {
            assert!(kinds.contains(&kind), "missing {kind:?}");
        }
        assert!(parsed
            .root()
            .descendants_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .any(|token| token.kind() == RoxygenKind::CODE_SPAN));
        let tree_tokens: Vec<_> = parsed
            .root()
            .descendants_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .filter(|token| token.kind() != RoxygenKind::MISSING)
            .map(|token| token.text().to_string())
            .collect();
        let projected_tokens: Vec<_> = parsed
            .tokens()
            .iter()
            .map(|token| {
                let range = token.projected_range;
                parsed.projected_source()[usize::from(range.start())..usize::from(range.end())]
                    .to_owned()
            })
            .collect();
        assert_eq!(tree_tokens, projected_tokens);
    }

    #[test]
    fn diagnostics_and_missing_tokens_are_sidecar_local() {
        let source = "#' @\n#' @param , bad\n#' broken [link\n#' `open\nx <- 1\n";
        let host = host_snapshot(source);
        let host_diagnostics = host.diagnostics().len();
        let parsed = parse(&host);
        assert!(!parsed.is_valid());
        assert!(parsed
            .diagnostics()
            .iter()
            .all(|item| item.code.as_str().starts_with("R-ROXYGEN-")));
        assert!(parsed
            .root()
            .descendants_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .any(|token| token.kind() == RoxygenKind::MISSING && token.text().is_empty()));
        assert_eq!(host.diagnostics().len(), host_diagnostics);
        assert_eq!(host.root().text().to_string(), source);
    }

    #[test]
    fn preserves_crlf_and_maps_projection_to_host() {
        let parsed = sidecar("  #' one\r\n#' two\r\n");
        assert_eq!(parsed.projected_source(), "#' one\r\n#' two");
        let newline = parsed
            .tokens()
            .iter()
            .find(|token| token.kind == RoxygenKind::NEWLINE)
            .unwrap();
        assert_eq!(u32::from(newline.host_range.len()), 2);
        assert_eq!(
            parsed.host_offset(newline.projected_range.start()),
            Some(newline.host_range.start())
        );
    }

    #[test]
    fn blank_and_unknown_tags_remain_lossless() {
        let parsed = sidecar("#'\n#'   \n#' @unknown (x): [y]\n");
        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.root().text().to_string(), parsed.projected_source());
        assert!(parsed
            .root()
            .descendants()
            .any(|node| node.kind() == RoxygenKind::OPAQUE_BODY));
    }

    #[test]
    fn snapshots_are_deterministic_and_thread_safe() {
        let source = Arc::<str>::from("#' @param x value\n#' Text [topic]\nf <- function(x) x\n");
        let results: Vec<_> = (0..8)
            .map(|_| {
                let source = Arc::clone(&source);
                thread::spawn(move || {
                    let parsed = sidecar(&source);
                    (
                        parsed.projected_source().to_owned(),
                        format!("{:?}", parsed.green()),
                    )
                })
            })
            .map(|handle| handle.join().unwrap())
            .collect();
        assert!(results.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn token_limit_returns_a_lossless_error_tail() {
        let host = host_snapshot("#' one two three\n");
        let parsed = parse_sidecar(
            &host,
            &RoxygenConfig {
                limits: RoxygenLimits {
                    max_tokens: 2,
                    ..RoxygenLimits::DEFAULT
                },
                ..RoxygenConfig::default()
            },
        );
        assert_eq!(parsed.root().text().to_string(), parsed.projected_source());
        assert!(parsed
            .diagnostics()
            .iter()
            .any(|item| item.code == RESOURCE_LIMIT));
        assert!(parsed.is_limited());
    }

    #[test]
    fn registry_is_pinned_immutable_and_drives_dispatch() {
        let base = TagRegistry::default();
        assert_eq!(base.version(), Version::new(8, 1, 0));
        assert!(base.get("widget").is_none());
        let extended = base.extended(TagDefinition::new("widget", BodyMode::Parameter));
        assert!(base.get("widget").is_none());
        let parsed = parse_source(
            "#' @widget x description\n",
            &RoxygenConfig {
                registry: extended.clone(),
                ..RoxygenConfig::default()
            },
        );
        assert_eq!(parsed.config().registry, extended);
        assert!(parsed
            .root()
            .descendants()
            .any(|node| node.kind() == RoxygenKind::PARAM_BODY));
    }

    #[test]
    fn tag_mark_only_occurs_at_the_line_tag_position() {
        let parsed = sidecar("#' text @param x and mail@host\n#'   @param y value\n");
        let marks = parsed
            .tokens()
            .iter()
            .filter(|token| token.kind == RoxygenKind::TAG_MARK)
            .count();
        assert_eq!(marks, 1);
    }

    #[test]
    fn examples_and_fences_suppress_false_tags_across_lines() {
        let parsed = sidecar(
            "#' @examples f(\n#' @not_a_registered_tag\n#' )\n#' @details done\n#' ```r\n#' @param not_a_tag\n#' ```\n#' @param real value\n",
        );
        let names = parsed
            .tokens()
            .iter()
            .filter(|token| token.kind == RoxygenKind::TAG_NAME)
            .map(|token| source_text(parsed.projected_source(), token.projected_range))
            .collect::<Vec<_>>();
        assert_eq!(names, ["examples", "details", "param"]);
        assert!(parsed
            .tokens()
            .iter()
            .any(|token| token.kind == RoxygenKind::CODE_BLOCK));
    }

    #[test]
    fn associates_blocks_using_top_level_host_expressions() {
        let parsed = sidecar(
            "#' normal\nx <- 1\n#' stop\nNULL\n#' first\n# ordinary\n#' second\ny <- 2\n#' detached\n",
        );
        let states = parsed
            .associations()
            .iter()
            .map(|association| association.state)
            .collect::<Vec<_>>();
        assert_eq!(
            states,
            [
                AssociationState::Normal,
                AssociationState::TerminatedByNull,
                AssociationState::Ambiguous,
                AssociationState::Normal,
                AssociationState::Detached,
            ]
        );
        assert!(parsed
            .associations()
            .iter()
            .all(|item| item.intervening_host_range.start()
                == parsed.blocks()[item.block_index].host_range.end()));
    }

    #[test]
    fn maps_points_and_ranges_in_both_directions_with_affinity() {
        let parsed = sidecar("  #' one\n#' two\n");
        let first_end = parsed.tokens()[0].projected_range.end();
        assert_eq!(
            parsed.host_point(first_end, BoundaryAffinity::Left),
            Some(parsed.tokens()[0].host_range.end())
        );
        assert_eq!(
            parsed.host_point(first_end, BoundaryAffinity::Right),
            Some(parsed.tokens()[1].host_range.start())
        );
        let range = parsed.tokens()[1].projected_range;
        let host = parsed.host_range(range).unwrap();
        assert_eq!(parsed.projected_range(host), Some(range));
        assert_eq!(
            parsed.projected_point(TextSize::from(0), BoundaryAffinity::Right),
            None
        );
    }

    #[test]
    fn limits_are_visible_even_when_diagnostics_are_capped() {
        let parsed = parse_source(
            "#' one\n#' two\n",
            &RoxygenConfig {
                limits: RoxygenLimits {
                    max_lines: 1,
                    max_diagnostics: 0,
                    ..RoxygenLimits::DEFAULT
                },
                ..RoxygenConfig::default()
            },
        );
        assert!(parsed.status().limited);
        assert!(parsed.status().diagnostics_truncated);
        assert!(parsed.diagnostics().is_empty());
    }

    #[test]
    fn host_discovery_incompleteness_is_propagated() {
        let source = "#' found\n#' undiscoverable\n";
        let lexed = lex("#' found", &LexerConfig::default());
        let mut builder = GreenNodeBuilder::new();
        builder.start_node(rowan::SyntaxKind(SyntaxKind::SOURCE_FILE.as_u16()));
        for token in lexed.tokens {
            builder.token(
                rowan::SyntaxKind(token.kind.as_u16()),
                source_text(source, token.range),
            );
        }
        builder.finish_node();
        let host = ParseSnapshot::new(
            DocumentId(1),
            Arc::<str>::from(source),
            builder.finish(),
            [],
            Completeness::Invalid,
            SyntaxProfile::default(),
        );
        assert!(parse(&host).status().discovery_incomplete);
    }

    #[test]
    fn embedded_examples_are_parsed_without_execution() {
        let parsed = parse_source(
            "#' @examples\n#' x <- function(a) a + 1\n#' x(2)\nf <- function() NULL\n",
            &RoxygenConfig {
                parse_examples: true,
                ..RoxygenConfig::default()
            },
        );
        let embedded = &parsed.embedded_parses()[0];
        assert!(embedded.host_range.start() < embedded.host_range.end());
        assert_eq!(embedded.snapshot().document(), parsed.document());
        assert!(embedded.snapshot().source().contains("function"));
        assert_eq!(
            embedded.host_range(embedded.snapshot().root().text_range()),
            Some(embedded.host_range)
        );
    }

    #[test]
    fn parse_outputs_are_send_sync_and_support_shared_queries() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TagRegistry>();
        assert_send_sync::<RoxygenParse>();
        let parsed = Arc::new(sidecar("#' @param x value\nf <- function(x) x\n"));
        let handles = (0..8)
            .map(|_| {
                let parsed = Arc::clone(&parsed);
                thread::spawn(move || {
                    (
                        parsed.root().text().to_string(),
                        parsed.associations()[0].clone(),
                        parsed.host_range(parsed.blocks()[0].projected_range),
                    )
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert!(results.windows(2).all(|pair| pair[0] == pair[1]));
    }
}
