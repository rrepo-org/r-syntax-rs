//! Stateless, lossless lexical analysis of decoded R source.
#![forbid(unsafe_code)]

use std::sync::Arc;

use r_source::{CompatibilityProfile, LineDirective};
use r_syntax::{
    Diagnostic, DiagnosticCode, Recovery, RecoveryKind, Severity, SyntaxKind, TextRange, TextSize,
};

/// Resource limits. Limits never cause source text to be dropped: the unlexed
/// remainder is represented by one `ERROR_TOKEN`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LexerLimits {
    pub max_input_bytes: usize,
    pub max_tokens: usize,
    pub max_diagnostics: usize,
    pub max_raw_delimiter_dashes: usize,
}

impl LexerLimits {
    pub const DEFAULT: Self = Self {
        max_input_bytes: u32::MAX as usize,
        max_tokens: 8_000_000,
        max_diagnostics: 10_000,
        max_raw_delimiter_dashes: 1_000,
    };
    pub const UNLIMITED: Self = Self {
        max_input_bytes: u32::MAX as usize,
        max_tokens: usize::MAX,
        max_diagnostics: usize::MAX,
        max_raw_delimiter_dashes: usize::MAX,
    };
}

impl Default for LexerLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct LexerConfig {
    pub compatibility: CompatibilityProfile,
    pub limits: LexerLimits,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LineEnding {
    Lf,
    Cr,
    CrLf,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RoxygenIndent {
    NonIndented,
    Indented,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum CommentMetadata {
    Ordinary,
    Roxygen(RoxygenIndent),
    InitialShebang,
    /// `SyntaxKind` has no directive token, so directives remain comments and
    /// carry the source-map type used by `r-source` here.
    LineDirective(LineDirective),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum TokenMetadata {
    None,
    LineEnding(LineEnding),
    Comment(CommentMetadata),
    /// The spelling `**` maps to R's exponentiation kind, just like `^`.
    DoubleStar,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Token {
    pub kind: SyntaxKind,
    pub range: TextRange,
    pub metadata: TokenMetadata,
}

impl Token {
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[u32::from(self.range.start()) as usize..u32::from(self.range.end()) as usize]
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LexicalDiagnosticKind {
    UnterminatedLiteral,
    InvalidEscape,
    InvalidString,
    InvalidNumber,
    UnknownScalar,
    InputLimit,
    TokenLimit,
    RawDelimiterLimit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LexicalDiagnostic {
    pub kind: LexicalDiagnosticKind,
    pub diagnostic: Diagnostic,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub diagnostics: Vec<LexicalDiagnostic>,
    /// True when lexical errors occurred, even if no diagnostics were retained.
    pub had_errors: bool,
    /// True if additional diagnostics were suppressed by the configured cap.
    pub diagnostics_truncated: bool,
    /// True if a limit caused a final `ERROR_TOKEN` to cover the remainder.
    pub tokens_truncated: bool,
}

impl Lexed {
    pub fn round_trip(&self, source: &str) -> String {
        self.tokens.iter().map(|token| token.text(source)).collect()
    }
}

/// Lexes known-valid, decoded UTF-8. The result does not borrow the input and
/// depends only on `source` and `config`.
pub fn lex(source: &str, config: &LexerConfig) -> Lexed {
    Lexer::new(source, config).run()
}

/// Lexes with the pinned default compatibility profile and limits.
pub fn lex_default(source: &str) -> Lexed {
    lex(source, &LexerConfig::default())
}

struct Lexer<'a> {
    source: &'a str,
    config: &'a LexerConfig,
    pos: usize,
    line_start: usize,
    physical_line: usize,
    tokens: Vec<Token>,
    diagnostics: Vec<LexicalDiagnostic>,
    had_errors: bool,
    diagnostics_truncated: bool,
    tokens_truncated: bool,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str, config: &'a LexerConfig) -> Self {
        Self {
            source,
            config,
            pos: 0,
            line_start: 0,
            physical_line: 0,
            tokens: Vec::new(),
            diagnostics: Vec::new(),
            had_errors: false,
            diagnostics_truncated: false,
            tokens_truncated: false,
        }
    }

    fn run(mut self) -> Lexed {
        if self.source.len() > u32::MAX as usize {
            // Rowan ranges cannot represent larger decoded documents.
            self.diagnostics_truncated = true;
            return Lexed {
                tokens: Vec::new(),
                diagnostics: Vec::new(),
                had_errors: true,
                diagnostics_truncated: true,
                tokens_truncated: true,
            };
        }
        if self.source.len() > self.config.limits.max_input_bytes {
            self.limit_remainder(
                LexicalDiagnosticKind::InputLimit,
                "lexer input byte limit exceeded",
            );
        }
        while self.pos < self.source.len() && !self.tokens_truncated {
            if self.tokens.len() >= self.config.limits.max_tokens.max(1).saturating_sub(1) {
                self.limit_remainder(
                    LexicalDiagnosticKind::TokenLimit,
                    "lexer token limit exceeded",
                );
                break;
            }
            let before = self.pos;
            self.next_token();
            debug_assert!(self.pos > before, "lexer must always advance");
        }
        Lexed {
            tokens: self.tokens,
            diagnostics: self.diagnostics,
            had_errors: self.had_errors,
            diagnostics_truncated: self.diagnostics_truncated,
            tokens_truncated: self.tokens_truncated,
        }
    }

    fn next_token(&mut self) {
        let start = self.pos;
        let byte = self.bytes()[start];
        match byte {
            b'\n' => {
                self.pos += 1;
                self.push(
                    start,
                    SyntaxKind::NEWLINE,
                    TokenMetadata::LineEnding(LineEnding::Lf),
                );
                self.new_line();
            }
            b'\r' => {
                self.pos += 1;
                let ending = if self.eat_byte(b'\n') {
                    LineEnding::CrLf
                } else {
                    LineEnding::Cr
                };
                self.push(
                    start,
                    SyntaxKind::NEWLINE,
                    TokenMetadata::LineEnding(ending),
                );
                self.new_line();
            }
            b'#' => self.comment(),
            b'\'' | b'"' => self.quoted(byte, SyntaxKind::STRING),
            b'`' => self.quoted(byte, SyntaxKind::IDENTIFIER),
            b'%' => self.special(),
            b'0'..=b'9' => self.number(false),
            b'.' if self.peek_byte(1).is_some_and(|b| b.is_ascii_digit()) => self.number(true),
            b'.' => self.dot_or_name(),
            b'r' | b'R' if self.raw_opener().is_some() => self.raw_string(),
            _ if self.current_char().is_some_and(is_identifier_start) => self.identifier(),
            _ if self.current_char().is_some_and(is_non_newline_whitespace) => self.whitespace(),
            _ => self.operator_or_unknown(),
        }
    }

    fn bytes(&self) -> &[u8] {
        self.source.as_bytes()
    }

    fn peek_byte(&self, ahead: usize) -> Option<u8> {
        self.bytes().get(self.pos + ahead).copied()
    }

    fn eat_byte(&mut self, byte: u8) -> bool {
        if self.peek_byte(0) == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn current_char(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    fn advance_char(&mut self) -> char {
        let ch = self.current_char().expect("cursor is not at EOF");
        self.pos += ch.len_utf8();
        ch
    }

    fn push(&mut self, start: usize, kind: SyntaxKind, metadata: TokenMetadata) {
        self.tokens.push(Token {
            kind,
            range: range(start, self.pos),
            metadata,
        });
    }

    fn new_line(&mut self) {
        self.line_start = self.pos;
        self.physical_line += 1;
    }

    fn whitespace(&mut self) {
        let start = self.pos;
        while self.current_char().is_some_and(is_non_newline_whitespace) {
            self.advance_char();
        }
        self.push(start, SyntaxKind::WHITESPACE, TokenMetadata::None);
    }

    fn comment(&mut self) {
        let start = self.pos;
        while self.peek_byte(0).is_some_and(|b| b != b'\n' && b != b'\r') {
            self.pos += 1;
        }
        let text = &self.source[start..self.pos];
        let leading_indent = self.source[self.line_start..start]
            .bytes()
            .all(|b| matches!(b, b' ' | b'\t'));
        let metadata = if start == 0 && text.starts_with("#!") {
            CommentMetadata::InitialShebang
        } else if leading_indent && text.starts_with("#'") {
            let indent = if start == self.line_start {
                RoxygenIndent::NonIndented
            } else {
                RoxygenIndent::Indented
            };
            CommentMetadata::Roxygen(indent)
        } else if leading_indent && self.config.compatibility.recognize_line_directives {
            parse_line_directive(text, self.physical_line)
                .map(CommentMetadata::LineDirective)
                .unwrap_or(CommentMetadata::Ordinary)
        } else {
            CommentMetadata::Ordinary
        };
        let kind = if matches!(metadata, CommentMetadata::Roxygen(_)) {
            SyntaxKind::ROXYGEN_COMMENT
        } else {
            SyntaxKind::COMMENT
        };
        self.push(start, kind, TokenMetadata::Comment(metadata));
    }

    fn dot_or_name(&mut self) {
        let start = self.pos;
        if self.source[start..].starts_with("...")
            && self.source[start + 3..]
                .chars()
                .next()
                .map_or(true, |ch| !is_identifier_continue(ch))
        {
            self.pos += 3;
            self.push(start, SyntaxKind::ELLIPSIS, TokenMetadata::None);
            return;
        }
        if self.source[start..].starts_with("..")
            && self.peek_byte(2).is_some_and(|b| matches!(b, b'1'..=b'9'))
        {
            self.pos += 2;
            while self.peek_byte(0).is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
            if self
                .current_char()
                .map_or(true, |ch| !is_identifier_continue(ch))
            {
                self.push(start, SyntaxKind::DOT_DOT_I, TokenMetadata::None);
                return;
            }
            self.pos = start;
        }
        self.identifier();
    }

    fn identifier(&mut self) {
        let start = self.pos;
        self.advance_char();
        while self.current_char().is_some_and(is_identifier_continue) {
            self.advance_char();
        }
        let kind =
            SyntaxKind::keyword(&self.source[start..self.pos]).unwrap_or(SyntaxKind::IDENTIFIER);
        self.push(start, kind, TokenMetadata::None);
    }

    fn quoted(&mut self, quote: u8, valid_kind: SyntaxKind) {
        let start = self.pos;
        self.pos += 1;
        let mut terminated = false;
        let mut first_newline = None;
        while self.pos < self.source.len() {
            match self.peek_byte(0) {
                Some(byte) if byte == quote => {
                    self.pos += 1;
                    terminated = true;
                    break;
                }
                Some(b'\\') => self.escape(),
                Some(b'\n' | b'\r') => {
                    first_newline.get_or_insert(self.pos);
                    self.consume_line_ending();
                }
                Some(_) => {
                    self.advance_char();
                }
                None => break,
            }
        }
        if terminated {
            self.push(start, valid_kind, TokenMetadata::None);
            if let Some(newline) = first_newline {
                self.diagnose(
                    LexicalDiagnosticKind::InvalidString,
                    newline,
                    newline + 1,
                    "unescaped newline in quoted literal",
                );
            }
        } else {
            self.push(start, SyntaxKind::ERROR_TOKEN, TokenMetadata::None);
            self.diagnose(
                LexicalDiagnosticKind::UnterminatedLiteral,
                start,
                self.pos,
                "unterminated quoted literal",
            );
        }
    }

    fn escape(&mut self) {
        let start = self.pos;
        self.pos += 1;
        let Some(byte) = self.peek_byte(0) else {
            return;
        };
        if matches!(
            byte,
            b'a' | b'b' | b'f' | b'n' | b'r' | b't' | b'v' | b'\\' | b'\'' | b'"' | b'`'
        ) {
            self.pos += 1;
            return;
        }
        if matches!(byte, b'\n' | b'\r') {
            self.consume_line_ending();
            return;
        }
        if matches!(byte, b'0'..=b'7') {
            for _ in 0..3 {
                if self.peek_byte(0).is_some_and(|b| matches!(b, b'0'..=b'7')) {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            return;
        }
        if byte == b'x' {
            self.pos += 1;
            let digits = self.take_hex(2);
            if digits == 0 {
                self.invalid_escape(start);
            }
            return;
        }
        if matches!(byte, b'u' | b'U') {
            self.pos += 1;
            let max = if byte == b'u' { 4 } else { 8 };
            let mut malformed = false;
            let digits = if self.eat_byte(b'{') {
                let digits_start = self.pos;
                let count = self.take_hex(max);
                if !self.eat_byte(b'}') {
                    malformed = true;
                }
                malformed |= !is_unicode_scalar(&self.source[digits_start..digits_start + count]);
                count
            } else {
                let digits_start = self.pos;
                let count = self.take_hex(max);
                malformed |= !is_unicode_scalar(&self.source[digits_start..digits_start + count]);
                count
            };
            if malformed
                || digits == 0
                || (self.bytes().get(start + 2) != Some(&b'{') && digits != max)
            {
                self.invalid_escape(start);
            }
            return;
        }
        self.advance_char();
        self.invalid_escape(start);
    }

    fn invalid_escape(&mut self, start: usize) {
        self.diagnose(
            LexicalDiagnosticKind::InvalidEscape,
            start,
            self.pos,
            "invalid escape sequence",
        );
    }

    fn take_hex(&mut self, max: usize) -> usize {
        let start = self.pos;
        while self.pos - start < max && self.peek_byte(0).is_some_and(|b| b.is_ascii_hexdigit()) {
            self.pos += 1;
        }
        self.pos - start
    }

    fn raw_opener(&self) -> Option<(usize, u8)> {
        if !matches!(self.peek_byte(0), Some(b'r' | b'R')) || self.peek_byte(1) != Some(b'"') {
            return None;
        }
        let mut offset = 2;
        while self.peek_byte(offset) == Some(b'-') {
            offset += 1;
        }
        matches!(self.peek_byte(offset), Some(b'(' | b'[' | b'{' | b'|'))
            .then(|| (offset - 2, self.peek_byte(offset).unwrap()))
    }

    fn raw_string(&mut self) {
        let start = self.pos;
        let (dashes, opener) = self.raw_opener().expect("checked by caller");
        self.pos += 2 + dashes + 1;
        if dashes > self.config.limits.max_raw_delimiter_dashes {
            self.diagnose(
                LexicalDiagnosticKind::RawDelimiterLimit,
                start + 2,
                start + 2 + dashes,
                "raw string delimiter exceeds configured dash limit",
            );
        }
        let close = match opener {
            b'(' => b')',
            b'[' => b']',
            b'{' => b'}',
            b'|' => b'|',
            _ => unreachable!(),
        };
        let mut terminated = false;
        while self.pos < self.source.len() {
            if self.peek_byte(0) == Some(close) {
                let after = self.pos + 1;
                if self
                    .bytes()
                    .get(after..after + dashes)
                    .is_some_and(|s| s.iter().all(|&b| b == b'-'))
                    && self.bytes().get(after + dashes) == Some(&b'"')
                {
                    self.pos = after + dashes + 1;
                    terminated = true;
                    break;
                }
            }
            if matches!(self.peek_byte(0), Some(b'\n' | b'\r')) {
                self.consume_line_ending();
            } else {
                self.advance_char();
            }
        }
        if terminated {
            self.push(start, SyntaxKind::RAW_STRING, TokenMetadata::None);
        } else {
            self.push(start, SyntaxKind::ERROR_TOKEN, TokenMetadata::None);
            self.diagnose(
                LexicalDiagnosticKind::UnterminatedLiteral,
                start,
                self.pos,
                "unterminated raw string",
            );
        }
    }

    fn number(&mut self, leading_dot: bool) {
        let start = self.pos;
        let mut invalid = false;
        let mut hex = false;
        if leading_dot {
            self.pos += 1;
            self.take_ascii_digits();
            if matches!(self.peek_byte(0), Some(b'e' | b'E')) {
                self.pos += 1;
                if matches!(self.peek_byte(0), Some(b'+' | b'-')) {
                    self.pos += 1;
                }
                let exponent = self.pos;
                self.take_ascii_digits();
                invalid |= self.pos == exponent;
            }
        } else if self.source[start..].starts_with("0x") || self.source[start..].starts_with("0X") {
            hex = true;
            self.pos += 2;
            let before = self.pos;
            self.take_hex(usize::MAX);
            if self.eat_byte(b'.') {
                self.take_hex(usize::MAX);
            }
            if self.pos == before || (self.pos == before + 1 && self.bytes()[before] == b'.') {
                invalid = true;
            }
            if matches!(self.peek_byte(0), Some(b'p' | b'P')) {
                self.pos += 1;
                if matches!(self.peek_byte(0), Some(b'+' | b'-')) {
                    self.pos += 1;
                }
                let exponent = self.pos;
                self.take_ascii_digits();
                invalid |= self.pos == exponent;
            }
        } else {
            self.take_ascii_digits();
            if self.eat_byte(b'.') {
                self.take_ascii_digits();
            }
            if matches!(self.peek_byte(0), Some(b'e' | b'E')) {
                self.pos += 1;
                if matches!(self.peek_byte(0), Some(b'+' | b'-')) {
                    self.pos += 1;
                }
                let exponent = self.pos;
                self.take_ascii_digits();
                invalid |= self.pos == exponent;
            }
        }
        let integral = numeric_value_is_integral(&self.source[start..self.pos], hex);
        let suffix = self.peek_byte(0).filter(|b| matches!(b, b'L' | b'i'));
        if suffix.is_some() {
            self.pos += 1;
        }
        if suffix == Some(b'L') && !integral {
            invalid = true;
        }
        let kind = match suffix {
            Some(b'L') => SyntaxKind::INTEGER,
            Some(b'i') => SyntaxKind::COMPLEX,
            _ => SyntaxKind::DOUBLE,
        };
        self.push(start, kind, TokenMetadata::None);
        if invalid {
            self.diagnose(
                LexicalDiagnosticKind::InvalidNumber,
                start,
                self.pos,
                if hex {
                    "invalid hexadecimal numeric literal"
                } else {
                    "invalid decimal numeric literal"
                },
            );
        }
    }

    fn take_ascii_digits(&mut self) {
        while self.peek_byte(0).is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
    }

    fn consume_line_ending(&mut self) {
        let byte = self.peek_byte(0).expect("cursor is at a line ending");
        self.pos += 1;
        if byte == b'\r' {
            self.eat_byte(b'\n');
        }
        self.new_line();
    }

    fn special(&mut self) {
        let start = self.pos;
        self.pos += 1;
        let mut escaped = false;
        while let Some(byte) = self.peek_byte(0) {
            if matches!(byte, b'\n' | b'\r') {
                break;
            }
            self.pos += 1;
            if byte == b'%' && !escaped {
                self.push(start, SyntaxKind::SPECIAL, TokenMetadata::None);
                return;
            }
            escaped = byte == b'\\' && !escaped;
        }
        self.push(start, SyntaxKind::ERROR_TOKEN, TokenMetadata::None);
        self.diagnose(
            LexicalDiagnosticKind::UnterminatedLiteral,
            start,
            self.pos,
            "unterminated special operator",
        );
    }

    fn operator_or_unknown(&mut self) {
        let start = self.pos;
        const OPERATORS: &[(&str, SyntaxKind, TokenMetadata)] = &[
            ("<<-", SyntaxKind::SUPER_LEFT_ASSIGN, TokenMetadata::None),
            ("->>", SyntaxKind::SUPER_RIGHT_ASSIGN, TokenMetadata::None),
            (":::", SyntaxKind::NS_GET_INTERNAL, TokenMetadata::None),
            ("[[", SyntaxKind::L_DBRACKET, TokenMetadata::None),
            ("**", SyntaxKind::CARET, TokenMetadata::DoubleStar),
            ("<=", SyntaxKind::LE, TokenMetadata::None),
            (">=", SyntaxKind::GE, TokenMetadata::None),
            ("==", SyntaxKind::EQ2, TokenMetadata::None),
            ("!=", SyntaxKind::NE, TokenMetadata::None),
            ("<-", SyntaxKind::LEFT_ASSIGN, TokenMetadata::None),
            ("->", SyntaxKind::RIGHT_ASSIGN, TokenMetadata::None),
            (":=", SyntaxKind::WALRUS, TokenMetadata::None),
            ("::", SyntaxKind::NS_GET, TokenMetadata::None),
            ("&&", SyntaxKind::AMP2, TokenMetadata::None),
            ("||", SyntaxKind::PIPE2, TokenMetadata::None),
            ("|>", SyntaxKind::NATIVE_PIPE, TokenMetadata::None),
            ("=>", SyntaxKind::PIPE_BIND, TokenMetadata::None),
        ];
        for (text, kind, metadata) in OPERATORS {
            if self.source[start..].starts_with(text) {
                self.pos += text.len();
                self.push(start, *kind, metadata.clone());
                return;
            }
        }
        let kind = match self.bytes()[start] {
            b'(' => SyntaxKind::L_PAREN,
            b')' => SyntaxKind::R_PAREN,
            b'{' => SyntaxKind::L_BRACE,
            b'}' => SyntaxKind::R_BRACE,
            b'[' => SyntaxKind::L_BRACKET,
            b']' => SyntaxKind::R_BRACKET,
            b',' => SyntaxKind::COMMA,
            b';' => SyntaxKind::SEMICOLON,
            b'+' => SyntaxKind::PLUS,
            b'-' => SyntaxKind::MINUS,
            b'*' => SyntaxKind::STAR,
            b'/' => SyntaxKind::SLASH,
            b'^' => SyntaxKind::CARET,
            b':' => SyntaxKind::COLON,
            b'~' => SyntaxKind::TILDE,
            b'?' => SyntaxKind::QUESTION,
            b'!' => SyntaxKind::BANG,
            b'&' => SyntaxKind::AMP,
            b'|' => SyntaxKind::PIPE,
            b'<' => SyntaxKind::LT,
            b'>' => SyntaxKind::GT,
            b'=' => SyntaxKind::EQ,
            b'$' => SyntaxKind::DOLLAR,
            b'@' => SyntaxKind::AT,
            b'\\' => SyntaxKind::BACKSLASH,
            b'_' => SyntaxKind::UNDERSCORE,
            _ => {
                self.advance_char();
                self.push(start, SyntaxKind::ERROR_TOKEN, TokenMetadata::None);
                self.diagnose(
                    LexicalDiagnosticKind::UnknownScalar,
                    start,
                    self.pos,
                    "unknown input scalar",
                );
                return;
            }
        };
        self.pos += 1;
        self.push(start, kind, TokenMetadata::None);
    }

    fn diagnose(&mut self, kind: LexicalDiagnosticKind, start: usize, end: usize, message: &str) {
        self.had_errors = true;
        if self.diagnostics.len() >= self.config.limits.max_diagnostics {
            self.diagnostics_truncated = true;
            return;
        }
        let code = match kind {
            LexicalDiagnosticKind::UnterminatedLiteral => DiagnosticCode::UNTERMINATED_LITERAL,
            LexicalDiagnosticKind::InvalidEscape => DiagnosticCode::INVALID_ESCAPE,
            LexicalDiagnosticKind::InvalidString => DiagnosticCode::new("R-LEX-008"),
            LexicalDiagnosticKind::InvalidNumber => DiagnosticCode::INVALID_NUMBER,
            LexicalDiagnosticKind::UnknownScalar => DiagnosticCode::new("R-LEX-004"),
            LexicalDiagnosticKind::InputLimit => DiagnosticCode::new("R-LEX-005"),
            LexicalDiagnosticKind::TokenLimit => DiagnosticCode::new("R-LEX-006"),
            LexicalDiagnosticKind::RawDelimiterLimit => DiagnosticCode::new("R-LEX-007"),
        };
        let text_range = range(start, end);
        let recovery_kind = if matches!(kind, LexicalDiagnosticKind::UnterminatedLiteral) {
            RecoveryKind::ReachedEndOfFile
        } else {
            RecoveryKind::None
        };
        let diagnostic =
            Diagnostic::new(code, Severity::Error, text_range, message).with_recovery(Recovery {
                kind: recovery_kind,
                range: text_range,
                token: None,
            });
        self.diagnostics
            .push(LexicalDiagnostic { kind, diagnostic });
    }

    fn limit_remainder(&mut self, kind: LexicalDiagnosticKind, message: &str) {
        if self.pos >= self.source.len() {
            return;
        }
        let start = self.pos;
        self.pos = self.source.len();
        self.push(start, SyntaxKind::ERROR_TOKEN, TokenMetadata::None);
        self.diagnose(kind, start, self.pos, message);
        self.tokens_truncated = true;
    }
}

fn range(start: usize, end: usize) -> TextRange {
    TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32))
}

fn is_non_newline_whitespace(ch: char) -> bool {
    ch != '\n' && ch != '\r' && ch.is_whitespace()
}

// This deliberately uses Rust's locale-independent Unicode tables rather than
// ambient C locale classification. Non-ASCII alphabetic/numeric scalars are
// stable identifier constituents for a pinned Rust toolchain.
fn is_identifier_start(ch: char) -> bool {
    ch == '.' || ch.is_alphabetic()
}

fn is_identifier_continue(ch: char) -> bool {
    ch == '.' || ch == '_' || ch.is_alphanumeric()
}

fn is_unicode_scalar(hex: &str) -> bool {
    !hex.is_empty()
        && u32::from_str_radix(hex, 16)
            .ok()
            .and_then(char::from_u32)
            .is_some()
}

fn numeric_value_is_integral(text: &str, hex: bool) -> bool {
    let exponent_marker = if hex { ['p', 'P'] } else { ['e', 'E'] };
    let (mantissa, exponent) = text.find(exponent_marker).map_or((text, 0_i64), |index| {
        (
            &text[..index],
            text[index + 1..].parse::<i64>().unwrap_or_else(|_| {
                if text[index + 1..].starts_with('-') {
                    i64::MIN
                } else {
                    i64::MAX
                }
            }),
        )
    });
    let fraction_digits = mantissa
        .split_once('.')
        .map_or(0_i64, |(_, fraction)| fraction.len() as i64);
    if hex {
        let mut trailing_zero_bits = 0_i64;
        for byte in mantissa.bytes().rev().filter(|byte| *byte != b'.') {
            let Some(digit) = (byte as char).to_digit(16) else {
                break;
            };
            trailing_zero_bits += i64::from(digit.trailing_zeros().min(4));
            if digit != 0 {
                break;
            }
        }
        4_i64
            .saturating_mul(fraction_digits)
            .saturating_sub(exponent)
            <= trailing_zero_bits
    } else {
        let trailing_zeroes = mantissa
            .bytes()
            .rev()
            .filter(|byte| *byte != b'.')
            .take_while(|byte| *byte == b'0')
            .count() as i64;
        fraction_digits.saturating_sub(exponent) <= trailing_zeroes
    }
}

fn parse_line_directive(comment: &str, physical_line: usize) -> Option<LineDirective> {
    let mut rest = comment.strip_prefix('#')?.trim_start_matches([' ', '\t']);
    rest = rest.strip_prefix("line")?;
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    rest = rest.trim_start_matches([' ', '\t']);
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let logical_line = rest.get(..digits)?.parse::<u32>().ok()?;
    if logical_line == 0 {
        return None;
    }
    rest = rest[digits..].trim_start_matches([' ', '\t']);
    let source_name = if rest.is_empty() {
        None
    } else {
        let quoted = rest.strip_prefix('"')?;
        let end = quoted.find('"')?;
        if !quoted[end + 1..].trim_matches([' ', '\t']).is_empty() {
            return None;
        }
        Some(Arc::<str>::from(&quoted[..end]))
    };
    Some(LineDirective {
        physical_line,
        logical_line,
        source_name,
    })
}
