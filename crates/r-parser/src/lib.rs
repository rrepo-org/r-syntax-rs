//! Lossless, error-tolerant parsing of R source.
#![forbid(unsafe_code)]

use std::{collections::BTreeSet, ops::Deref, sync::Arc};

use r_lexer::{lex, LexerConfig, LexerLimits, LexicalDiagnosticKind, Token};
use r_source::{CompatibilityProfile, SourceText};
use r_syntax::{
    Completeness, Diagnostic, DiagnosticCode, DocumentId, GreenNodeBuilder, ParseSnapshot,
    Recovery, RecoveryKind, Severity, SyntaxKind, SyntaxProfile, TextRange, TextSize,
};

const DUPLICATE_FORMAL: DiagnosticCode = DiagnosticCode::new("R-PARSE-004");
const INVALID_OPERAND: DiagnosticCode = DiagnosticCode::new("R-PARSE-005");
const INVALID_PIPE: DiagnosticCode = DiagnosticCode::new("R-PARSE-006");
const RESOURCE_LIMIT: DiagnosticCode = DiagnosticCode::new("R-PARSE-007");
const DISABLED_SYNTAX: DiagnosticCode = DiagnosticCode::new("R-PARSE-008");
const INVALID_CONTEXT: DiagnosticCode = DiagnosticCode::new("R-PARSE-009");

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ParserLimits {
    pub max_depth: usize,
    pub max_events: usize,
    pub max_diagnostics: usize,
    pub max_recovery_tokens: usize,
}

impl ParserLimits {
    pub const DEFAULT: Self = Self {
        max_depth: 512,
        max_events: 24_000_000,
        max_diagnostics: 10_000,
        max_recovery_tokens: 256,
    };
    pub const UNLIMITED: Self = Self {
        max_depth: usize::MAX,
        max_events: usize::MAX,
        max_diagnostics: usize::MAX,
        max_recovery_tokens: usize::MAX,
    };
}

impl Default for ParserLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// All choices affecting parsing are immutable input data. No process state is read.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ParserConfig {
    pub document: DocumentId,
    pub compatibility: CompatibilityProfile,
    pub syntax_profile: SyntaxProfile,
    pub lexer_limits: LexerLimits,
    pub limits: ParserLimits,
    /// Enables the experimental `=>` infix syntax.
    pub enable_pipe_bind: bool,
}

impl ParserConfig {
    pub const DEFAULT: Self = Self {
        document: DocumentId(0),
        compatibility: CompatibilityProfile::R_4_6_1,
        syntax_profile: SyntaxProfile::R_4_6_1_ROXYGEN2_8_1_0,
        lexer_limits: LexerLimits::DEFAULT,
        limits: ParserLimits::DEFAULT,
        enable_pipe_bind: false,
    };
}

impl Default for ParserConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ParseStatus {
    Empty,
    Complete,
    Incomplete,
    Invalid,
}

#[derive(Clone, Debug)]
pub struct Parse {
    snapshot: ParseSnapshot,
    tokens: Arc<[Token]>,
    status: ParseStatus,
    diagnostics_truncated: bool,
    resource_limited: bool,
}

impl Parse {
    pub fn snapshot(&self) -> &ParseSnapshot {
        &self.snapshot
    }
    pub fn into_snapshot(self) -> ParseSnapshot {
        self.snapshot
    }
    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }
    pub fn status(&self) -> ParseStatus {
        self.status
    }
    pub fn needs_more_input(&self) -> bool {
        self.status == ParseStatus::Incomplete
    }
    pub fn diagnostics_truncated(&self) -> bool {
        self.diagnostics_truncated
    }
    pub fn resource_limited(&self) -> bool {
        self.resource_limited
    }
}

impl Deref for Parse {
    type Target = ParseSnapshot;
    fn deref(&self) -> &Self::Target {
        &self.snapshot
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Entry {
    Source,
    Expression,
    Interactive,
    Argument,
    Formal,
    Parenthesized,
}

pub fn parse_source(source: &str, config: &ParserConfig) -> Parse {
    parse(source, None, config, Entry::Source)
}

pub fn parse_source_text(source: &SourceText, config: &ParserConfig) -> Parse {
    parse(source.text(), Some(source), config, Entry::Source)
}

pub fn parse_expression(source: &str, config: &ParserConfig) -> Parse {
    parse(source, None, config, Entry::Expression)
}

pub fn parse_expression_source_text(source: &SourceText, config: &ParserConfig) -> Parse {
    parse(source.text(), Some(source), config, Entry::Expression)
}

pub fn parse_interactive(source: &str, config: &ParserConfig) -> Parse {
    parse(source, None, config, Entry::Interactive)
}

pub fn parse_interactive_source_text(source: &SourceText, config: &ParserConfig) -> Parse {
    parse(source.text(), Some(source), config, Entry::Interactive)
}

pub fn parse_argument(source: &str, config: &ParserConfig) -> Parse {
    parse(source, None, config, Entry::Argument)
}

pub fn parse_formal(source: &str, config: &ParserConfig) -> Parse {
    parse(source, None, config, Entry::Formal)
}

pub fn parse_parenthesized(source: &str, config: &ParserConfig) -> Parse {
    parse(source, None, config, Entry::Parenthesized)
}

pub fn needs_more_input(source: &str, config: &ParserConfig) -> bool {
    parse_interactive(source, config).needs_more_input()
}

pub fn needs_more_input_source_text(source: &SourceText, config: &ParserConfig) -> bool {
    parse_interactive_source_text(source, config).needs_more_input()
}

fn parse(
    source: &str,
    source_text: Option<&SourceText>,
    config: &ParserConfig,
    entry: Entry,
) -> Parse {
    let lexer_config = LexerConfig {
        compatibility: config.compatibility,
        limits: config.lexer_limits,
    };
    let lexed = lex(source, &lexer_config);
    let lexical_incomplete = lexed.diagnostics.iter().any(|item| {
        item.kind == LexicalDiagnosticKind::UnterminatedLiteral
            && item.diagnostic.range.end() == TextSize::of(source)
    });
    let lexical_had_errors = lexed.had_errors;
    let lexer_limited = lexed.tokens_truncated;
    let mut diagnostics_truncated = lexed.diagnostics_truncated;
    let tokens: Arc<[Token]> = lexed.tokens.into();
    let mut parser = Parser::new(source, &tokens, config);
    parser.run(entry);
    let empty = tokens.iter().all(|token| token.kind.is_trivia());
    let (
        events,
        mut diagnostics,
        parser_incomplete,
        parser_had_errors,
        parser_limited,
        parser_diagnostics_truncated,
        event_fallback,
    ) = parser.finish();
    diagnostics_truncated |= parser_diagnostics_truncated;
    let incomplete = parser_incomplete || lexical_incomplete;
    diagnostics.extend(lexed.diagnostics.into_iter().map(|item| item.diagnostic));
    let source_had_errors = source_text.is_some_and(|source| !source.issues().is_empty());
    if let Some(source) = source_text {
        diagnostics.extend(source.issues().iter().map(|issue| {
            Diagnostic::new(
                DiagnosticCode::INVALID_UTF8,
                Severity::Error,
                TextRange::new(
                    TextSize::from(issue.decoded_range.start as u32),
                    TextSize::from(issue.decoded_range.end as u32),
                ),
                "invalid UTF-8 was replaced while decoding source",
            )
        }));
    }
    diagnostics.sort_by(|a, b| {
        (a.range.start(), a.range.end(), a.code.as_str(), &a.message).cmp(&(
            b.range.start(),
            b.range.end(),
            b.code.as_str(),
            &b.message,
        ))
    });
    if diagnostics.len() > config.limits.max_diagnostics {
        diagnostics.truncate(config.limits.max_diagnostics);
        diagnostics_truncated = true;
    }
    let green = Sink::new(source, &tokens, events, event_fallback).finish();
    let had_errors = lexical_had_errors || parser_had_errors || source_had_errors;
    let resource_limited = lexer_limited || parser_limited;
    let status = if empty && diagnostics.is_empty() {
        if had_errors || resource_limited {
            ParseStatus::Invalid
        } else {
            ParseStatus::Empty
        }
    } else if incomplete {
        ParseStatus::Incomplete
    } else if had_errors || resource_limited {
        ParseStatus::Invalid
    } else {
        ParseStatus::Complete
    };
    let completeness = match status {
        ParseStatus::Incomplete => Completeness::Incomplete,
        ParseStatus::Invalid => Completeness::Invalid,
        ParseStatus::Empty | ParseStatus::Complete => Completeness::Complete,
    };
    let snapshot = if let Some(source_text) = source_text {
        ParseSnapshot::new_with_source_text(
            config.document,
            source_text.clone(),
            green,
            diagnostics,
            completeness,
            config.syntax_profile,
        )
    } else {
        ParseSnapshot::new(
            config.document,
            Arc::<str>::from(source),
            green,
            diagnostics,
            completeness,
            config.syntax_profile,
        )
    };
    Parse {
        snapshot,
        tokens,
        status,
        diagnostics_truncated,
        resource_limited,
    }
}

#[derive(Debug)]
enum Event {
    Start {
        kind: SyntaxKind,
        forward_parent: Option<usize>,
    },
    Finish,
    TokenRange {
        start: usize,
        end: usize,
    },
    Synthetic(SyntaxKind),
    Tombstone,
}

#[derive(Clone, Copy, Debug)]
struct Marker(Option<usize>);

#[derive(Clone, Copy, Debug)]
struct CompletedMarker {
    pos: Option<usize>,
    kind: SyntaxKind,
}

#[derive(Clone, Copy)]
struct ExprContext {
    soft_newline: bool,
    allow_eq: bool,
    in_pipe_rhs: bool,
    placeholder_allowed: bool,
}

#[derive(Default)]
struct FormalState {
    names: BTreeSet<String>,
    saw_ellipsis: bool,
}

impl ExprContext {
    const TOP: Self = Self {
        soft_newline: false,
        allow_eq: true,
        in_pipe_rhs: false,
        placeholder_allowed: false,
    };
    const NESTED: Self = Self {
        soft_newline: true,
        allow_eq: true,
        in_pipe_rhs: false,
        placeholder_allowed: false,
    };
}

struct Parser<'a> {
    source: &'a str,
    tokens: &'a [Token],
    config: &'a ParserConfig,
    pos: usize,
    events: Vec<Event>,
    diagnostics: Vec<Diagnostic>,
    depth: usize,
    incomplete: bool,
    limited: bool,
    had_errors: bool,
    diagnostics_truncated: bool,
    event_fallback: bool,
    loop_depth: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str, tokens: &'a [Token], config: &'a ParserConfig) -> Self {
        Self {
            source,
            tokens,
            config,
            pos: 0,
            events: Vec::new(),
            diagnostics: Vec::new(),
            depth: 0,
            incomplete: false,
            limited: false,
            had_errors: false,
            diagnostics_truncated: false,
            event_fallback: false,
            loop_depth: 0,
        }
    }

    fn run(&mut self, entry: Entry) {
        let root = self.start();
        match entry {
            Entry::Source | Entry::Interactive => self.expression_list(None),
            Entry::Expression => {
                if self.parse_expr(0, ExprContext::TOP).is_none() {
                    let incomplete = self.eof();
                    self.expected_expression(incomplete);
                }
                self.consume_remainder();
            }
            Entry::Argument => {
                let list = self.start();
                self.argument(SyntaxKind::EOF, false, false);
                list.complete(self, SyntaxKind::ARGUMENT_LIST);
                self.consume_remainder();
            }
            Entry::Formal => {
                let list = self.start();
                self.formal(&mut FormalState::default());
                list.complete(self, SyntaxKind::PARAMETER_LIST);
                self.consume_remainder();
            }
            Entry::Parenthesized => {
                if self.at(SyntaxKind::L_PAREN, false) {
                    self.paren(ExprContext::NESTED);
                } else {
                    self.expected(SyntaxKind::L_PAREN, false);
                    self.parse_expr(0, ExprContext::NESTED);
                }
                self.consume_remainder();
            }
        }
        self.drain_tokens();
        root.complete(self, SyntaxKind::SOURCE_FILE);
    }

    fn finish(self) -> (Vec<Event>, Vec<Diagnostic>, bool, bool, bool, bool, bool) {
        (
            self.events,
            self.diagnostics,
            self.incomplete,
            self.had_errors,
            self.limited,
            self.diagnostics_truncated,
            self.event_fallback,
        )
    }

    fn start(&mut self) -> Marker {
        if self.event_fallback {
            return Marker(None);
        }
        let pos = self.events.len();
        if !self.push_event(Event::Tombstone) {
            return Marker(None);
        }
        Marker(Some(pos))
    }

    fn push_event(&mut self, event: Event) -> bool {
        if self.event_fallback {
            return false;
        }
        if self.events.len() >= self.config.limits.max_events {
            self.events.clear();
            self.event_fallback = true;
            self.limit("parser event limit exceeded");
            return false;
        }
        self.events.push(event);
        true
    }

    fn expression_list(&mut self, close: Option<SyntaxKind>) {
        let list = self.start();
        loop {
            self.eat_separators();
            if self.eof() || close.is_some_and(|kind| self.at(kind, false)) {
                break;
            }
            let before = self.pos;
            self.parse_expr(0, ExprContext::TOP);
            if self.pos == before {
                self.recover_one("expected an expression");
            }
            if !self.at_any(&[SyntaxKind::NEWLINE, SyntaxKind::SEMICOLON], false)
                && !self.eof()
                && !close.is_some_and(|kind| self.at(kind, false))
            {
                self.recover_one("expected a newline or ';'");
            }
        }
        list.complete(self, SyntaxKind::EXPRESSION_LIST);
    }

    fn parse_expr(&mut self, min_bp: u8, ctx: ExprContext) -> Option<CompletedMarker> {
        if self.depth >= self.config.limits.max_depth {
            self.limit("parser nesting limit exceeded");
            return self.recover_one("expression nesting is too deep");
        }
        self.depth += 1;
        self.prepare(ctx.soft_newline);
        let Some(mut lhs) = self.prefix(ctx) else {
            self.depth -= 1;
            return None;
        };
        loop {
            self.prepare(ctx.soft_newline);
            if let Some((postfix_bp, kind)) = self.postfix_kind() {
                if postfix_bp < min_bp {
                    break;
                }
                lhs = self.postfix(lhs, kind, ctx);
                continue;
            }
            let token = self.current_raw();
            let Some((left_bp, right_bp, node_kind)) = token.and_then(|kind| self.infix(kind, ctx))
            else {
                break;
            };
            if left_bp < min_bp {
                break;
            }
            let operator_pos = self.pos;
            let parent = lhs.precede(self);
            let native_pipe = self.tokens[operator_pos].kind == SyntaxKind::NATIVE_PIPE;
            self.bump_raw();
            self.prepare(true);
            let rhs_start = self.pos;
            let rhs_ctx = ExprContext {
                in_pipe_rhs: native_pipe,
                placeholder_allowed: native_pipe,
                ..ctx
            };
            let rhs = self.parse_expr(right_bp, rhs_ctx);
            if rhs.is_none() {
                self.expected_expression(true);
            }
            lhs = parent.complete(self, node_kind);
            self.validate_binary(operator_pos, rhs_start, rhs, native_pipe);
        }
        self.depth -= 1;
        Some(lhs)
    }

    fn prefix(&mut self, ctx: ExprContext) -> Option<CompletedMarker> {
        let kind = self.current_raw()?;
        if kind.is_literal_token() {
            let marker = self.start();
            self.bump_raw();
            return Some(marker.complete(self, SyntaxKind::LITERAL_EXPR));
        }
        match kind {
            SyntaxKind::IDENTIFIER | SyntaxKind::DOT_DOT_I | SyntaxKind::ELLIPSIS => {
                let marker = self.start();
                self.bump_raw();
                Some(marker.complete(self, SyntaxKind::IDENTIFIER_EXPR))
            }
            SyntaxKind::UNDERSCORE => {
                let marker = self.start();
                let range = self.tokens[self.pos].range;
                self.bump_raw();
                if !ctx.placeholder_allowed {
                    self.error(
                        INVALID_PIPE,
                        range,
                        "placeholder '_' is only valid in a native pipe RHS",
                    );
                }
                Some(marker.complete(self, SyntaxKind::IDENTIFIER_EXPR))
            }
            SyntaxKind::L_PAREN => Some(self.paren(ctx)),
            SyntaxKind::L_BRACE => Some(self.braced()),
            SyntaxKind::FUNCTION_KW | SyntaxKind::BACKSLASH => Some(self.function()),
            SyntaxKind::IF_KW => Some(self.if_expr()),
            SyntaxKind::WHILE_KW => Some(self.while_expr()),
            SyntaxKind::FOR_KW => Some(self.for_expr()),
            SyntaxKind::REPEAT_KW => Some(self.simple_control(SyntaxKind::REPEAT_EXPR, true)),
            SyntaxKind::NEXT_KW => Some(self.simple_control(SyntaxKind::NEXT_EXPR, false)),
            SyntaxKind::BREAK_KW => Some(self.simple_control(SyntaxKind::BREAK_EXPR, false)),
            SyntaxKind::PLUS
            | SyntaxKind::MINUS
            | SyntaxKind::BANG
            | SyntaxKind::TILDE
            | SyntaxKind::QUESTION => {
                let marker = self.start();
                let node = if kind == SyntaxKind::TILDE {
                    SyntaxKind::FORMULA_EXPR
                } else if kind == SyntaxKind::QUESTION {
                    SyntaxKind::HELP_EXPR
                } else {
                    SyntaxKind::UNARY_EXPR
                };
                self.bump_raw();
                self.prepare(true);
                if self.parse_expr(prefix_bp(kind), ctx).is_none() {
                    self.expected_expression(true);
                }
                Some(marker.complete(self, node))
            }
            SyntaxKind::ERROR_TOKEN => {
                let marker = self.start();
                self.bump_raw();
                Some(marker.complete(self, SyntaxKind::ERROR))
            }
            _ => {
                self.expected_expression(false);
                None
            }
        }
    }

    fn paren(&mut self, ctx: ExprContext) -> CompletedMarker {
        let marker = self.start();
        self.bump(SyntaxKind::L_PAREN, false);
        if !self.at(SyntaxKind::R_PAREN, true) {
            self.parse_expr(
                0,
                ExprContext {
                    soft_newline: true,
                    ..ctx
                },
            );
        } else {
            self.expected_expression(false);
        }
        self.expect_close(SyntaxKind::R_PAREN);
        marker.complete(self, SyntaxKind::PAREN_EXPR)
    }

    fn braced(&mut self) -> CompletedMarker {
        let marker = self.start();
        self.bump(SyntaxKind::L_BRACE, false);
        self.expression_list(Some(SyntaxKind::R_BRACE));
        self.expect_close(SyntaxKind::R_BRACE);
        marker.complete(self, SyntaxKind::BRACED_EXPR)
    }

    fn function(&mut self) -> CompletedMarker {
        let marker = self.start();
        self.bump_raw();
        self.prepare(true);
        let outer_loop_depth = self.loop_depth;
        self.loop_depth = 0;
        self.formal_list();
        if self.parse_expr(0, ExprContext::TOP).is_none() {
            self.expected_expression(true);
        }
        self.loop_depth = outer_loop_depth;
        marker.complete(self, SyntaxKind::FUNCTION_EXPR)
    }

    fn formal_list(&mut self) {
        let list = self.start();
        if !self.bump(SyntaxKind::L_PAREN, true) {
            self.expected(SyntaxKind::L_PAREN, true);
        }
        let mut state = FormalState::default();
        loop {
            self.prepare(true);
            if self.eof() || self.at(SyntaxKind::R_PAREN, true) {
                break;
            }
            self.formal(&mut state);
            if !self.bump(SyntaxKind::COMMA, true) {
                break;
            }
        }
        self.expect_close(SyntaxKind::R_PAREN);
        list.complete(self, SyntaxKind::PARAMETER_LIST);
    }

    fn formal(&mut self, state: &mut FormalState) {
        let parameter = self.start();
        self.prepare(true);
        if self.at_any(
            &[
                SyntaxKind::IDENTIFIER,
                SyntaxKind::ELLIPSIS,
                SyntaxKind::DOT_DOT_I,
            ],
            true,
        ) {
            let token = &self.tokens[self.pos];
            let name = token.text(self.source).to_owned();
            let range = token.range;
            let ellipsis = token.kind == SyntaxKind::ELLIPSIS;
            self.bump_raw();
            if !state.names.insert(name.clone()) {
                self.error(
                    DUPLICATE_FORMAL,
                    range,
                    format!("duplicate formal parameter '{name}'"),
                );
            }
            let has_default = self.bump(SyntaxKind::EQ, true);
            if has_default {
                if self.at_any(&[SyntaxKind::COMMA, SyntaxKind::R_PAREN], true) || self.eof() {
                    self.missing();
                } else {
                    self.parse_expr(0, ExprContext::NESTED);
                }
            }
            if ellipsis {
                if state.saw_ellipsis {
                    self.error(
                        INVALID_CONTEXT,
                        range,
                        "'...' may occur only once in formals",
                    );
                }
                if has_default {
                    self.error(INVALID_CONTEXT, range, "'...' may not have a default");
                }
                state.saw_ellipsis = true;
            } else if state.saw_ellipsis && !has_default {
                self.error(
                    INVALID_CONTEXT,
                    range,
                    "formal parameters after '...' require defaults",
                );
            }
        } else {
            let incomplete = self.eof();
            self.expected(SyntaxKind::IDENTIFIER, incomplete);
            if !self.eof() {
                self.recover_one("invalid formal parameter");
            }
        }
        parameter.complete(self, SyntaxKind::PARAMETER);
    }

    fn if_expr(&mut self) -> CompletedMarker {
        let marker = self.start();
        self.bump_raw();
        self.condition();
        self.prepare(true);
        let then_branch = self.parse_expr(0, ExprContext::TOP);
        if then_branch.is_none() {
            self.expected_expression(true);
        }
        // R attaches `else` across a newline only when the then branch is braced.
        let braced_then = then_branch.is_some_and(|branch| branch.kind == SyntaxKind::BRACED_EXPR);
        self.prepare(braced_then);
        if self.at(SyntaxKind::ELSE_KW, false) {
            let clause = self.start();
            self.bump_raw();
            self.prepare(true);
            if self.parse_expr(0, ExprContext::TOP).is_none() {
                self.expected_expression(true);
            }
            clause.complete(self, SyntaxKind::ELSE_CLAUSE);
        }
        marker.complete(self, SyntaxKind::IF_EXPR)
    }

    fn while_expr(&mut self) -> CompletedMarker {
        let marker = self.start();
        self.bump_raw();
        self.condition();
        self.prepare(true);
        self.loop_depth += 1;
        if self.parse_expr(0, ExprContext::TOP).is_none() {
            self.expected_expression(true);
        }
        self.loop_depth -= 1;
        marker.complete(self, SyntaxKind::WHILE_EXPR)
    }

    fn for_expr(&mut self) -> CompletedMarker {
        let marker = self.start();
        self.bump_raw();
        let condition = self.start();
        self.bump_or_expected(SyntaxKind::L_PAREN, true);
        if self.at(SyntaxKind::IDENTIFIER, true) {
            self.bump_raw();
        } else {
            let incomplete = self.eof();
            self.expected(SyntaxKind::IDENTIFIER, incomplete);
        }
        self.bump_or_expected(SyntaxKind::IN_KW, true);
        if self.parse_expr(0, ExprContext::NESTED).is_none() {
            self.expected_expression(true);
        }
        self.expect_close(SyntaxKind::R_PAREN);
        condition.complete(self, SyntaxKind::CONDITION);
        self.prepare(true);
        self.loop_depth += 1;
        if self.parse_expr(0, ExprContext::TOP).is_none() {
            self.expected_expression(true);
        }
        self.loop_depth -= 1;
        marker.complete(self, SyntaxKind::FOR_EXPR)
    }

    fn condition(&mut self) {
        let condition = self.start();
        self.bump_or_expected(SyntaxKind::L_PAREN, true);
        if self.parse_expr(0, ExprContext::NESTED).is_none() {
            self.expected_expression(true);
        }
        self.expect_close(SyntaxKind::R_PAREN);
        condition.complete(self, SyntaxKind::CONDITION);
    }

    fn simple_control(&mut self, kind: SyntaxKind, body: bool) -> CompletedMarker {
        let marker = self.start();
        let range = self.range_here();
        self.bump_raw();
        if body {
            self.prepare(true);
            self.loop_depth += 1;
            if self.parse_expr(0, ExprContext::TOP).is_none() {
                self.expected_expression(true);
            }
            self.loop_depth -= 1;
        } else if self.loop_depth == 0 {
            self.error(
                INVALID_CONTEXT,
                range,
                "'break' and 'next' require an enclosing loop",
            );
        }
        marker.complete(self, kind)
    }

    fn postfix_kind(&mut self) -> Option<(u8, SyntaxKind)> {
        Some(match self.current_raw()? {
            SyntaxKind::L_PAREN => (21, SyntaxKind::CALL_EXPR),
            SyntaxKind::L_BRACKET => (21, SyntaxKind::SUBSET_EXPR),
            SyntaxKind::L_DBRACKET => (21, SyntaxKind::SUBSET2_EXPR),
            SyntaxKind::DOLLAR | SyntaxKind::AT => (19, SyntaxKind::MEMBER_EXPR),
            SyntaxKind::NS_GET | SyntaxKind::NS_GET_INTERNAL => (20, SyntaxKind::NAMESPACE_EXPR),
            _ => return None,
        })
    }

    fn postfix(
        &mut self,
        lhs: CompletedMarker,
        node_kind: SyntaxKind,
        ctx: ExprContext,
    ) -> CompletedMarker {
        let marker = lhs.precede(self);
        match node_kind {
            SyntaxKind::CALL_EXPR => self.argument_list(ctx),
            SyntaxKind::SUBSET_EXPR => self.index_list(false, ctx),
            SyntaxKind::SUBSET2_EXPR => self.index_list(true, ctx),
            SyntaxKind::MEMBER_EXPR | SyntaxKind::NAMESPACE_EXPR => {
                let op = self.pos;
                self.bump_raw();
                self.prepare(true);
                if self
                    .current_raw()
                    .is_some_and(SyntaxKind::is_name_or_string_token)
                {
                    self.bump_raw();
                } else {
                    let range = self.range_here();
                    self.error(
                        INVALID_OPERAND,
                        range,
                        "extraction and namespace RHS must be a name or string",
                    );
                    if !self.eof() {
                        self.recover_one("invalid extraction operand");
                    } else {
                        self.incomplete = true;
                    }
                }
                if node_kind == SyntaxKind::NAMESPACE_EXPR {
                    self.validate_namespace_lhs(lhs, op);
                }
            }
            _ => unreachable!(),
        }
        marker.complete(self, node_kind)
    }

    fn argument_list(&mut self, ctx: ExprContext) {
        let list = self.start();
        self.bump_raw();
        loop {
            self.prepare(true);
            if self.eof() || self.at(SyntaxKind::R_PAREN, true) {
                break;
            }
            self.argument(SyntaxKind::R_PAREN, false, ctx.in_pipe_rhs);
            if !self.bump(SyntaxKind::COMMA, true) {
                break;
            }
            if self.at(SyntaxKind::R_PAREN, true) {
                self.argument(SyntaxKind::R_PAREN, false, ctx.in_pipe_rhs);
                break;
            }
        }
        self.expect_close(SyntaxKind::R_PAREN);
        list.complete(self, SyntaxKind::ARGUMENT_LIST);
    }

    fn argument(&mut self, close: SyntaxKind, _index: bool, in_pipe_rhs: bool) -> bool {
        let argument = self.start();
        self.prepare(true);
        let missing;
        if self.at_any(&[SyntaxKind::COMMA, close], true) || self.eof() {
            self.missing();
            missing = true;
        } else if self.current_raw().is_some_and(SyntaxKind::is_tag_token)
            && self.nth_non_trivia(1, true) == Some(SyntaxKind::EQ)
        {
            self.bump_raw();
            self.bump(SyntaxKind::EQ, true);
            if self.at_any(&[SyntaxKind::COMMA, close], true) || self.eof() {
                self.missing();
                missing = true;
            } else {
                missing = self
                    .parse_expr(
                        0,
                        ExprContext {
                            in_pipe_rhs,
                            placeholder_allowed: in_pipe_rhs,
                            ..ExprContext::NESTED
                        },
                    )
                    .is_none();
            }
        } else {
            missing = self
                .parse_expr(
                    0,
                    ExprContext {
                        in_pipe_rhs,
                        placeholder_allowed: false,
                        ..ExprContext::NESTED
                    },
                )
                .is_none();
        }
        argument.complete(self, SyntaxKind::ARGUMENT);
        missing
    }

    fn index_list(&mut self, double: bool, ctx: ExprContext) {
        let list = self.start();
        self.bump_raw();
        let close = SyntaxKind::R_BRACKET;
        let mut argument_count = 0_usize;
        let mut has_missing = false;
        loop {
            self.prepare(true);
            if self.eof() || self.at(close, true) {
                break;
            }
            let recovery = double.then(|| self.start());
            let missing = self.argument(close, true, ctx.in_pipe_rhs);
            argument_count += 1;
            has_missing |= missing;
            if let Some(recovery) = recovery {
                if missing || argument_count > 1 {
                    recovery.complete(self, SyntaxKind::ERROR);
                } else {
                    recovery.abandon(self);
                }
            }
            if !self.bump(SyntaxKind::COMMA, true) {
                break;
            }
            if self.at(close, true) {
                let recovery = double.then(|| self.start());
                let missing = self.argument(close, true, ctx.in_pipe_rhs);
                argument_count += 1;
                has_missing |= missing;
                if let Some(recovery) = recovery {
                    recovery.complete(self, SyntaxKind::ERROR);
                }
                break;
            }
        }
        if double && argument_count == 0 {
            let recovery = self.start();
            has_missing = self.argument(close, true, ctx.in_pipe_rhs);
            argument_count = 1;
            recovery.complete(self, SyntaxKind::ERROR);
        }
        self.expect_close(close);
        if double {
            self.expect_close(SyntaxKind::R_BRACKET);
            if argument_count != 1 || has_missing {
                self.error(
                    INVALID_OPERAND,
                    self.range_here(),
                    "'[[' requires exactly one nonmissing top-level argument",
                );
            }
        }
        list.complete(self, SyntaxKind::INDEX_ARGUMENT_LIST);
    }

    fn infix(&self, kind: SyntaxKind, ctx: ExprContext) -> Option<(u8, u8, SyntaxKind)> {
        let (bp, right_assoc, node) = match kind {
            SyntaxKind::QUESTION => (1, false, SyntaxKind::HELP_EXPR),
            SyntaxKind::LEFT_ASSIGN | SyntaxKind::SUPER_LEFT_ASSIGN => {
                (2, true, SyntaxKind::ASSIGNMENT_EXPR)
            }
            SyntaxKind::EQ if ctx.allow_eq => (3, true, SyntaxKind::ASSIGNMENT_EXPR),
            SyntaxKind::RIGHT_ASSIGN | SyntaxKind::SUPER_RIGHT_ASSIGN => {
                (4, false, SyntaxKind::ASSIGNMENT_EXPR)
            }
            SyntaxKind::TILDE => (5, true, SyntaxKind::FORMULA_EXPR),
            SyntaxKind::PIPE2 => (6, false, SyntaxKind::BINARY_EXPR),
            SyntaxKind::PIPE => (7, false, SyntaxKind::BINARY_EXPR),
            SyntaxKind::AMP2 => (8, false, SyntaxKind::BINARY_EXPR),
            SyntaxKind::AMP => (9, false, SyntaxKind::BINARY_EXPR),
            SyntaxKind::LT
            | SyntaxKind::LE
            | SyntaxKind::GT
            | SyntaxKind::GE
            | SyntaxKind::EQ2
            | SyntaxKind::NE => (11, false, SyntaxKind::BINARY_EXPR),
            SyntaxKind::PLUS | SyntaxKind::MINUS => (12, false, SyntaxKind::BINARY_EXPR),
            SyntaxKind::STAR | SyntaxKind::SLASH => (13, false, SyntaxKind::BINARY_EXPR),
            SyntaxKind::SPECIAL | SyntaxKind::WALRUS | SyntaxKind::NATIVE_PIPE => (
                14,
                false,
                if kind == SyntaxKind::NATIVE_PIPE {
                    SyntaxKind::PIPE_EXPR
                } else {
                    SyntaxKind::BINARY_EXPR
                },
            ),
            SyntaxKind::PIPE_BIND => (15, false, SyntaxKind::PIPE_EXPR),
            SyntaxKind::COLON => (16, false, SyntaxKind::BINARY_EXPR),
            SyntaxKind::CARET => (18, true, SyntaxKind::BINARY_EXPR),
            _ => return None,
        };
        Some((bp, if right_assoc { bp } else { bp + 1 }, node))
    }

    fn validate_binary(
        &mut self,
        operator_pos: usize,
        rhs_start: usize,
        rhs: Option<CompletedMarker>,
        native_pipe: bool,
    ) {
        let token_kind = self.tokens[operator_pos].kind;
        let token_range = self.tokens[operator_pos].range;
        if token_kind == SyntaxKind::PIPE_BIND && !self.config.enable_pipe_bind {
            self.error(
                DISABLED_SYNTAX,
                token_range,
                "'=>' syntax is disabled by ParserConfig",
            );
        }
        if native_pipe {
            let Some(rhs) = rhs else { return };
            let first_rhs = self.tokens[rhs_start..self.pos]
                .iter()
                .find(|token| !token.kind.is_trivia())
                .map(|token| token.kind);
            let placeholder_chain = first_rhs == Some(SyntaxKind::UNDERSCORE)
                && matches!(
                    rhs.kind,
                    SyntaxKind::MEMBER_EXPR | SyntaxKind::SUBSET_EXPR | SyntaxKind::SUBSET2_EXPR
                );
            if rhs.kind != SyntaxKind::CALL_EXPR && !placeholder_chain {
                self.error(
                    INVALID_PIPE,
                    token_range,
                    "native pipe RHS must be a call or placeholder extraction chain",
                );
            }
            let placeholders: Vec<_> = (rhs_start..self.pos)
                .filter(|index| self.tokens[*index].kind == SyntaxKind::UNDERSCORE)
                .collect();
            if placeholders.len() > 1 {
                self.error(
                    INVALID_PIPE,
                    self.tokens[placeholders[1]].range,
                    "native pipe RHS may contain at most one placeholder",
                );
            }
        }
    }

    fn validate_namespace_lhs(&mut self, lhs: CompletedMarker, operator_pos: usize) {
        let literal_is_string = lhs.kind == SyntaxKind::LITERAL_EXPR
            && self.tokens[..operator_pos]
                .iter()
                .rev()
                .find(|token| !token.kind.is_trivia())
                .is_some_and(|token| token.kind.is_name_or_string_token());
        if lhs.kind != SyntaxKind::IDENTIFIER_EXPR && !literal_is_string {
            self.error(
                INVALID_OPERAND,
                self.tokens[operator_pos].range,
                "namespace LHS must be a package name or string",
            );
        }
    }

    fn missing(&mut self) {
        let marker = self.start();
        self.push_event(Event::Synthetic(SyntaxKind::MISSING_TOKEN));
        marker.complete(self, SyntaxKind::MISSING);
    }

    fn prepare(&mut self, soft_newline: bool) {
        loop {
            while self.pos < self.tokens.len()
                && matches!(
                    self.tokens[self.pos].kind,
                    SyntaxKind::WHITESPACE | SyntaxKind::COMMENT | SyntaxKind::ROXYGEN_COMMENT
                )
            {
                self.bump_raw();
            }
            if soft_newline && self.current_raw() == Some(SyntaxKind::NEWLINE) {
                self.bump_raw();
                continue;
            }
            break;
        }
    }

    fn eat_separators(&mut self) {
        loop {
            self.prepare(false);
            if self.at_any(&[SyntaxKind::NEWLINE, SyntaxKind::SEMICOLON], false) {
                self.bump_raw();
            } else {
                break;
            }
        }
    }

    fn bump(&mut self, kind: SyntaxKind, soft: bool) -> bool {
        self.prepare(soft);
        if self.current_raw() == Some(kind) {
            self.bump_raw();
            true
        } else {
            false
        }
    }

    fn bump_or_expected(&mut self, kind: SyntaxKind, soft: bool) {
        if !self.bump(kind, soft) {
            let incomplete = self.eof();
            self.expected(kind, incomplete);
        }
    }

    fn bump_raw(&mut self) {
        if self.pos < self.tokens.len() {
            if !self.event_fallback {
                if let Some(Event::TokenRange { end, .. }) = self.events.last_mut() {
                    if *end == self.pos {
                        *end += 1;
                    } else {
                        self.push_event(Event::TokenRange {
                            start: self.pos,
                            end: self.pos + 1,
                        });
                    }
                } else {
                    self.push_event(Event::TokenRange {
                        start: self.pos,
                        end: self.pos + 1,
                    });
                }
            }
            self.pos += 1;
        }
    }

    fn at(&mut self, kind: SyntaxKind, soft: bool) -> bool {
        self.prepare(soft);
        self.current_raw() == Some(kind)
    }

    fn at_any(&mut self, kinds: &[SyntaxKind], soft: bool) -> bool {
        self.prepare(soft);
        self.current_raw().is_some_and(|kind| kinds.contains(&kind))
    }

    fn current_raw(&self) -> Option<SyntaxKind> {
        self.tokens.get(self.pos).map(|token| token.kind)
    }

    fn nth_non_trivia(&self, nth: usize, soft: bool) -> Option<SyntaxKind> {
        let mut seen = 0;
        for token in &self.tokens[self.pos..] {
            if matches!(
                token.kind,
                SyntaxKind::WHITESPACE | SyntaxKind::COMMENT | SyntaxKind::ROXYGEN_COMMENT
            ) || (soft && token.kind == SyntaxKind::NEWLINE)
            {
                continue;
            }
            if seen == nth {
                return Some(token.kind);
            }
            seen += 1;
        }
        None
    }

    fn eof(&mut self) -> bool {
        self.prepare(false);
        self.pos >= self.tokens.len()
    }

    fn expect_close(&mut self, kind: SyntaxKind) {
        if !self.bump(kind, true) {
            let incomplete = self.eof();
            self.expected(kind, incomplete);
        }
    }

    fn expected(&mut self, kind: SyntaxKind, incomplete: bool) {
        let range = self.range_here();
        self.incomplete |= incomplete;
        let diagnostic = Diagnostic::new(
            DiagnosticCode::EXPECTED_TOKEN,
            Severity::Error,
            range,
            format!("expected {kind:?}"),
        )
        .with_recovery(Recovery {
            kind: RecoveryKind::InsertedToken,
            range,
            token: Some(kind),
        });
        self.push_diagnostic(diagnostic);
        self.missing();
    }

    fn expected_expression(&mut self, incomplete: bool) {
        let range = self.range_here();
        self.incomplete |= incomplete && self.pos >= self.tokens.len();
        self.push_diagnostic(Diagnostic::new(
            DiagnosticCode::EXPECTED_EXPRESSION,
            Severity::Error,
            range,
            "expected an expression",
        ));
        self.missing();
    }

    fn recover_one(&mut self, message: &str) -> Option<CompletedMarker> {
        if self.pos >= self.tokens.len() {
            return None;
        }
        let marker = self.start();
        let range = self.tokens[self.pos].range;
        self.bump_raw();
        let completed = marker.complete(self, SyntaxKind::ERROR);
        self.push_diagnostic(
            Diagnostic::new(
                DiagnosticCode::UNEXPECTED_TOKEN,
                Severity::Error,
                range,
                message,
            )
            .with_recovery(Recovery {
                kind: RecoveryKind::WrappedErrorNode,
                range,
                token: None,
            }),
        );
        Some(completed)
    }

    fn consume_remainder(&mut self) {
        self.prepare(false);
        let mut recovered = 0;
        while self.pos < self.tokens.len() {
            if self.current_raw().is_some_and(SyntaxKind::is_trivia) {
                self.bump_raw();
            } else if recovered < self.config.limits.max_recovery_tokens {
                self.recover_one("unexpected token after expression");
                recovered += 1;
            } else {
                let marker = self.start();
                while self.pos < self.tokens.len() {
                    self.bump_raw();
                }
                marker.complete(self, SyntaxKind::ERROR);
                self.limit("parser recovery token limit exceeded");
            }
        }
    }

    fn drain_tokens(&mut self) {
        if self.pos < self.tokens.len() {
            let marker = self.start();
            while self.pos < self.tokens.len() {
                self.bump_raw();
            }
            marker.complete(self, SyntaxKind::ERROR);
        }
    }

    fn range_here(&self) -> TextRange {
        self.tokens.get(self.pos).map_or_else(
            || {
                let end = TextSize::of(self.source);
                TextRange::empty(end)
            },
            |token| token.range,
        )
    }

    fn error(&mut self, code: DiagnosticCode, range: TextRange, message: impl Into<String>) {
        self.push_diagnostic(Diagnostic::new(code, Severity::Error, range, message));
    }

    fn push_diagnostic(&mut self, diagnostic: Diagnostic) {
        if diagnostic.severity == Severity::Error {
            self.had_errors = true;
        }
        if self.diagnostics.len() < self.config.limits.max_diagnostics {
            self.diagnostics.push(diagnostic);
        } else {
            self.diagnostics_truncated = true;
        }
    }

    fn limit(&mut self, message: &str) {
        if !self.limited {
            self.limited = true;
            self.error(RESOURCE_LIMIT, self.range_here(), message);
        }
    }
}

impl Marker {
    fn abandon(self, parser: &mut Parser<'_>) {
        if let Some(pos) = self.0 {
            if !parser.event_fallback {
                parser.events[pos] = Event::Tombstone;
            }
        }
    }

    fn complete(self, parser: &mut Parser<'_>, kind: SyntaxKind) -> CompletedMarker {
        let Some(pos) = self.0 else {
            return CompletedMarker { pos: None, kind };
        };
        if parser.event_fallback {
            return CompletedMarker { pos: None, kind };
        }
        parser.events[pos] = Event::Start {
            kind,
            forward_parent: None,
        };
        if !parser.push_event(Event::Finish) {
            return CompletedMarker { pos: None, kind };
        }
        CompletedMarker {
            pos: Some(pos),
            kind,
        }
    }
}

impl CompletedMarker {
    fn precede(self, parser: &mut Parser<'_>) -> Marker {
        let marker = parser.start();
        let (Some(pos), Some(marker_pos)) = (self.pos, marker.0) else {
            return Marker(None);
        };
        if parser.event_fallback {
            return Marker(None);
        }
        let Event::Start { forward_parent, .. } = &mut parser.events[pos] else {
            unreachable!()
        };
        *forward_parent = Some(marker_pos - pos);
        marker
    }
}

fn prefix_bp(kind: SyntaxKind) -> u8 {
    match kind {
        SyntaxKind::QUESTION => 1,
        SyntaxKind::TILDE => 5,
        SyntaxKind::BANG => 10,
        SyntaxKind::PLUS | SyntaxKind::MINUS => 17,
        _ => 17,
    }
}

struct Sink<'a> {
    source: &'a str,
    tokens: &'a [Token],
    events: Vec<Event>,
    builder: GreenNodeBuilder<'static>,
    fallback: bool,
}

impl<'a> Sink<'a> {
    fn new(source: &'a str, tokens: &'a [Token], events: Vec<Event>, fallback: bool) -> Self {
        Self {
            source,
            tokens,
            events,
            builder: GreenNodeBuilder::new(),
            fallback,
        }
    }

    fn finish(mut self) -> r_syntax::GreenNode {
        if self.fallback {
            self.builder
                .start_node(rowan::SyntaxKind(SyntaxKind::SOURCE_FILE.as_u16()));
            self.builder
                .start_node(rowan::SyntaxKind(SyntaxKind::ERROR.as_u16()));
            if !self.source.is_empty() {
                self.builder.token(
                    rowan::SyntaxKind(SyntaxKind::ERROR_TOKEN.as_u16()),
                    self.source,
                );
            }
            self.builder.finish_node();
            self.builder.finish_node();
            return self.builder.finish();
        }
        for index in 0..self.events.len() {
            match std::mem::replace(&mut self.events[index], Event::Tombstone) {
                Event::Start {
                    kind,
                    forward_parent,
                } => {
                    let mut kinds = vec![kind];
                    let mut next = forward_parent.map(|distance| index + distance);
                    while let Some(parent) = next {
                        match std::mem::replace(&mut self.events[parent], Event::Tombstone) {
                            Event::Start {
                                kind,
                                forward_parent,
                            } => {
                                kinds.push(kind);
                                next = forward_parent.map(|distance| parent + distance);
                            }
                            _ => unreachable!("forward parent must point to a start event"),
                        }
                    }
                    for kind in kinds.into_iter().rev() {
                        self.builder.start_node(rowan::SyntaxKind(kind.as_u16()));
                    }
                }
                Event::Finish => self.builder.finish_node(),
                Event::TokenRange { start, end } => {
                    for token in &self.tokens[start..end] {
                        self.builder.token(
                            rowan::SyntaxKind(token.kind.as_u16()),
                            token.text(self.source),
                        );
                    }
                }
                Event::Synthetic(kind) => self.builder.token(rowan::SyntaxKind(kind.as_u16()), ""),
                Event::Tombstone => {}
            }
        }
        self.builder.finish()
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, thread};

    use r_lexer::TokenMetadata;
    use r_source::{DecodeMode, EncodingProfile, SourceEncoding, SourceLimits, SourceText};
    use r_syntax::{validate_snapshot, NodeOrToken, RowanAstNode, SyntaxKind};

    use super::*;

    fn parse_ok(source: &str) -> Parse {
        let parsed = parse_source(source, &ParserConfig::default());
        assert!(
            parsed.diagnostics().is_empty(),
            "{source:?}: {:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.root().text().to_string(), source);
        validate_snapshot(&parsed).unwrap();
        parsed
    }

    fn node_kinds(parsed: &Parse) -> Vec<SyntaxKind> {
        parsed
            .root()
            .descendants()
            .map(|node| node.kind())
            .collect()
    }

    #[test]
    fn practical_grammar() {
        for source in [
            "x <- 1 + 2 * 3 ^ 4",
            "pkg::fun(x = 1, , y = a[[2]])$value",
            "function(x, y = 2, ...) if (x) y else { x + y }",
            "\\(x, y = 1) x + y",
            "for (x in xs) { if (x > 2) next; print(x) }",
            "while (ok) repeat { break }",
            "y -> x; y ->> z",
            "x %custom% y |> f(a = _)",
            "~ x + y",
            "?mean",
            "x[,, drop = FALSE]",
        ] {
            parse_ok(source);
        }
    }

    #[test]
    fn precedence_and_surface_are_preserved() {
        let parsed = parse_ok("a + b * c ** d |> f()");
        let kinds = node_kinds(&parsed);
        assert!(kinds.contains(&SyntaxKind::PIPE_EXPR));
        assert_eq!(parsed.root().text().to_string(), "a + b * c ** d |> f()");
        assert!(parsed
            .tokens()
            .iter()
            .any(|token| matches!(token.metadata, TokenMetadata::DoubleStar)));
    }

    #[test]
    fn every_lexer_token_is_emitted_once() {
        let source = "# lead\r\nf ( x, , y = 2 ) [[ 1 ]] # end\n";
        let parsed = parse_source(source, &ParserConfig::default());
        let tree_tokens: Vec<_> = parsed
            .root()
            .descendants_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .filter(|token| token.kind() != SyntaxKind::MISSING_TOKEN)
            .map(|token| (token.kind(), token.text().to_string()))
            .collect();
        let lex_tokens: Vec<_> = parsed
            .tokens()
            .iter()
            .map(|token| (token.kind, token.text(source).to_owned()))
            .collect();
        assert_eq!(tree_tokens, lex_tokens);
        assert_eq!(parsed.root().text().to_string(), source);
    }

    #[test]
    fn missing_is_not_recovery() {
        let parsed = parse_ok("f(, x = , 3)[,]");
        assert!(
            node_kinds(&parsed)
                .iter()
                .filter(|kind| **kind == SyntaxKind::MISSING)
                .count()
                >= 4
        );
        assert!(!node_kinds(&parsed).contains(&SyntaxKind::ERROR));
    }

    #[test]
    fn completeness_is_independent_of_empty_and_invalid() {
        assert_eq!(
            parse_source(" # hi\n", &ParserConfig::default()).status(),
            ParseStatus::Empty
        );
        assert_eq!(
            parse_source("x +", &ParserConfig::default()).status(),
            ParseStatus::Incomplete
        );
        assert!(needs_more_input("if (x", &ParserConfig::default()));
        assert_eq!(
            parse_source("x + * y", &ParserConfig::default()).status(),
            ParseStatus::Invalid
        );
        assert_eq!(
            parse_source("x + y", &ParserConfig::default()).status(),
            ParseStatus::Complete
        );
    }

    #[test]
    fn validates_contextual_restrictions() {
        assert!(!parse_source("1::x", &ParserConfig::default())
            .diagnostics()
            .is_empty());
        assert!(!parse_source("x |> 1", &ParserConfig::default())
            .diagnostics()
            .is_empty());
        assert!(!parse_source("_", &ParserConfig::default())
            .diagnostics()
            .is_empty());
        assert!(!parse_source("x => f", &ParserConfig::default())
            .diagnostics()
            .is_empty());
        let enabled = ParserConfig {
            enable_pipe_bind: true,
            ..ParserConfig::default()
        };
        parse_ok_with("x => f", &enabled);
    }

    fn parse_ok_with(source: &str, config: &ParserConfig) {
        let parsed = parse_source(source, config);
        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        validate_snapshot(&parsed).unwrap();
    }

    #[test]
    fn duplicate_formals_are_deterministic() {
        let parsed = parse_source("function(x, x = 2) x", &ParserConfig::default());
        assert_eq!(parsed.diagnostics()[0].code, DUPLICATE_FORMAL);
    }

    #[test]
    fn parse_snapshots_are_thread_safe_and_deterministic() {
        let source = Arc::<str>::from("function(x) { x |> sum(na.rm = TRUE) }");
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let source = Arc::clone(&source);
                thread::spawn(move || {
                    let parsed = parse_source(&source, &ParserConfig::default());
                    (
                        parsed.root().text().to_string(),
                        r_syntax::fingerprint(&parsed.root()),
                    )
                })
            })
            .collect();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert!(results.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn parses_and_retains_latin1_source_with_maps() {
        let source = SourceText::from_bytes(
            Arc::<[u8]>::from(&b"#line 40 \"generated.R\"\nx <- \"caf\xe9\"\n"[..]),
            Some(Arc::from("input.R")),
            CompatibilityProfile::default(),
            EncodingProfile::LATIN1,
            SourceLimits::UNLIMITED,
        )
        .unwrap();
        let parsed = parse_source_text(&source, &ParserConfig::default());

        assert_eq!(parsed.status(), ParseStatus::Complete);
        assert_eq!(parsed.root().text().to_string(), source.text());
        let retained = parsed.source_text().unwrap();
        assert_eq!(retained.original_bytes(), source.original_bytes());
        assert_eq!(retained.offset_map(), source.offset_map());
        assert_eq!(retained.line_index(), source.line_index());
        assert_eq!(retained.logical_map().directives().len(), 1);
        let location = retained.logical_map().location(1, 0).unwrap();
        assert_eq!(location.line, 40);
        assert_eq!(location.source_name.as_deref(), Some("generated.R"));
        assert_send_sync::<Parse>();
    }

    #[test]
    fn recovering_decode_issues_are_source_diagnostics_in_decoded_ranges() {
        let source = SourceText::from_bytes(
            Arc::<[u8]>::from(&b"x <- \xff\n"[..]),
            None,
            CompatibilityProfile::default(),
            EncodingProfile::new(SourceEncoding::Utf8, DecodeMode::Recovering),
            SourceLimits::UNLIMITED,
        )
        .unwrap();
        let parsed = parse_source_text(&source, &ParserConfig::default());
        let source_diagnostic = parsed
            .diagnostics()
            .iter()
            .find(|diagnostic| diagnostic.code == DiagnosticCode::INVALID_UTF8)
            .unwrap();

        assert_eq!(parsed.status(), ParseStatus::Invalid);
        assert_eq!(source_diagnostic.range, TextRange::new(5.into(), 8.into()));
        assert_eq!(
            parsed.original_byte_range(source_diagnostic.range),
            Some(5..6)
        );
        assert_eq!(parsed.source_text().unwrap().issues(), source.issues());
    }

    #[test]
    fn decode_errors_survive_zero_diagnostic_caps() {
        let source = SourceText::from_bytes(
            Arc::<[u8]>::from(&b"\xff"[..]),
            None,
            CompatibilityProfile::default(),
            EncodingProfile::UTF8_RECOVERING,
            SourceLimits::UNLIMITED,
        )
        .unwrap();
        let config = ParserConfig {
            lexer_limits: LexerLimits {
                max_diagnostics: 0,
                ..LexerLimits::DEFAULT
            },
            limits: ParserLimits {
                max_diagnostics: 0,
                ..ParserLimits::DEFAULT
            },
            ..ParserConfig::default()
        };
        let parsed = parse_source_text(&source, &config);

        assert_eq!(parsed.status(), ParseStatus::Invalid);
        assert_eq!(parsed.completeness(), Completeness::Invalid);
        assert!(parsed.diagnostics().is_empty());
        assert!(parsed.diagnostics_truncated());
    }

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn bounded_recovery_still_returns_a_lossless_tree() {
        let config = ParserConfig {
            limits: ParserLimits {
                max_recovery_tokens: 2,
                ..ParserLimits::DEFAULT
            },
            ..ParserConfig::default()
        };
        let source = "1 ) ) ) )";
        let parsed = parse_source(source, &config);
        assert_eq!(parsed.root().text().to_string(), source);
        validate_snapshot(&parsed).unwrap();
    }

    fn top_expression(parsed: &Parse) -> r_syntax::SyntaxNode {
        parsed
            .root()
            .children()
            .find(|node| node.kind() == SyntaxKind::EXPRESSION_LIST)
            .unwrap()
            .children()
            .find(|node| r_syntax::Expr::can_cast(node.kind()))
            .unwrap()
    }

    fn direct_operator(node: &r_syntax::SyntaxNode) -> SyntaxKind {
        node.children_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .find(|token| !token.kind().is_trivia())
            .unwrap()
            .kind()
    }

    #[test]
    fn precedence_rows_are_distinct_and_ordered() {
        let rows = [
            "?", "<-", "=", "->", "~", "||", "|", "&&", "&", "<", "+", "*", "%x%", "=>", ":", "^",
        ];
        let enabled = ParserConfig {
            enable_pipe_bind: true,
            ..ParserConfig::default()
        };
        for pair in rows.windows(2) {
            let source = format!("a {} b {} c", pair[0], pair[1]);
            let parsed = parse_source(&source, &enabled);
            assert_eq!(
                direct_operator(&top_expression(&parsed)),
                parsed
                    .tokens()
                    .iter()
                    .find(|token| token.text(&source) == pair[0])
                    .unwrap()
                    .kind,
                "{source}"
            );
        }

        let source = "a %x% b |> f()";
        let parsed = parse_ok(source);
        assert_eq!(
            direct_operator(&top_expression(&parsed)),
            SyntaxKind::NATIVE_PIPE
        );
    }

    #[test]
    fn associativity_matches_each_documented_operator_class() {
        for (operator, right_associative) in [
            ("?", false),
            ("<-", true),
            ("=", true),
            ("->", false),
            ("~", true),
            ("||", false),
            ("|", false),
            ("&&", false),
            ("&", false),
            ("%x%", false),
            ("=>", false),
            (":", false),
            ("^", true),
        ] {
            let config = ParserConfig {
                enable_pipe_bind: true,
                ..ParserConfig::default()
            };
            let source = format!("a {operator} b {operator} c");
            let parsed = parse_source(&source, &config);
            let root = top_expression(&parsed);
            let child_expressions: Vec<_> = root
                .children()
                .filter(|node| r_syntax::Expr::can_cast(node.kind()))
                .collect();
            let nested = if right_associative {
                child_expressions.last().unwrap()
            } else {
                child_expressions.first().unwrap()
            };
            assert_eq!(nested.kind(), root.kind(), "{source}");
        }
    }

    #[test]
    fn unary_and_postfix_precedence_matches_r_order() {
        let power = parse_ok("-a^b");
        assert_eq!(top_expression(&power).kind(), SyntaxKind::UNARY_EXPR);
        let not = parse_ok("!a < b");
        assert_eq!(top_expression(&not).kind(), SyntaxKind::UNARY_EXPR);
        parse_ok("a::b$c(d)[1]");
    }

    #[test]
    fn native_pipe_validation_is_structural() {
        parse_ok("x |> f()");
        parse_ok("x |> f(value = _)");
        parse_ok("x |> f(value = (_))");
        parse_ok("x |> _$field");
        parse_ok("x |> _[[1]]");
        assert_eq!(
            parse_source("x |> rhs", &ParserConfig::default()).status(),
            ParseStatus::Invalid
        );
        assert_eq!(
            parse_source("x |> f(_)", &ParserConfig::default()).status(),
            ParseStatus::Invalid
        );
        assert_eq!(
            parse_source("x |> f((value = _))", &ParserConfig::default()).status(),
            ParseStatus::Invalid
        );
    }

    #[test]
    fn subset_tags_and_double_subset_arity_are_checked() {
        parse_ok("x[name = 1, r\"(raw)\" = 2]");
        parse_ok("x[[name = 1]]");
        for source in ["x[[]]", "x[[,]]", "x[[1, 2]]", "x[[name = ]]"] {
            let parsed = parse_source(source, &ParserConfig::default());
            assert_eq!(parsed.status(), ParseStatus::Invalid, "{source}");
            assert!(node_kinds(&parsed).contains(&SyntaxKind::ERROR), "{source}");
        }
    }

    #[test]
    fn formals_and_loop_context_follow_parser_time_rules() {
        parse_ok("function(..., x = 1, ..2 = 2) x");
        for source in [
            "function(..., x) x",
            "function(..., ...) x",
            "function(... = 1) x",
            "break",
            "next",
            "while (x) function() break",
            "while (x) function(a = break) 1",
        ] {
            assert_eq!(
                parse_source(source, &ParserConfig::default()).status(),
                ParseStatus::Invalid,
                "{source}"
            );
        }
        parse_ok("while (x) { next; break }");
    }

    #[test]
    fn missing_recovery_is_deliberate_and_zero_width() {
        let parsed = parse_source("f(", &ParserConfig::default());
        let missing: Vec<_> = parsed
            .root()
            .descendants_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .filter(|token| token.kind() == SyntaxKind::MISSING_TOKEN)
            .collect();
        assert!(!missing.is_empty());
        assert!(missing.iter().all(|token| token.text().is_empty()));
        assert_eq!(parsed.root().text().to_string(), "f(");
    }

    #[test]
    fn errors_and_limits_survive_zero_diagnostic_caps() {
        let config = ParserConfig {
            lexer_limits: LexerLimits {
                max_diagnostics: 0,
                ..LexerLimits::DEFAULT
            },
            limits: ParserLimits {
                max_diagnostics: 0,
                ..ParserLimits::DEFAULT
            },
            ..ParserConfig::default()
        };
        let lexical = parse_source("\0", &config);
        assert_eq!(lexical.status(), ParseStatus::Invalid);
        assert!(lexical.diagnostics().is_empty());
        assert!(lexical.diagnostics_truncated());

        let syntactic = parse_source("x + * y", &config);
        assert_eq!(syntactic.status(), ParseStatus::Invalid);
        assert!(syntactic.diagnostics().is_empty());
        assert!(syntactic.diagnostics_truncated());
    }

    #[test]
    fn max_events_uses_a_bounded_lossless_fallback() {
        let source = "f(a + b, c[[1]]) # retained\n";
        for max_events in 0..8 {
            let config = ParserConfig {
                limits: ParserLimits {
                    max_events,
                    max_diagnostics: 0,
                    ..ParserLimits::DEFAULT
                },
                ..ParserConfig::default()
            };
            let parsed = parse_source(source, &config);
            assert_eq!(parsed.root().text().to_string(), source);
            assert_eq!(parsed.status(), ParseStatus::Invalid);
            assert!(parsed.resource_limited());
            assert!(parsed.diagnostics_truncated());
            assert_eq!(
                parsed.root().children().next().unwrap().kind(),
                SyntaxKind::ERROR
            );
            validate_snapshot(&parsed).unwrap();
        }
    }
}
