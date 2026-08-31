use r_lexer::{
    lex, lex_default, CommentMetadata, LexerConfig, LexerLimits, LineEnding, RoxygenIndent,
    TokenMetadata,
};
use r_syntax::SyntaxKind;

fn kinds(source: &str) -> Vec<SyntaxKind> {
    lex_default(source)
        .tokens
        .into_iter()
        .map(|token| token.kind)
        .collect()
}

#[test]
fn trivia_and_comment_metadata_are_exact() {
    let source = "#!/usr/bin/env R\r\n#' top\r  #' indented\n#line 20 \"gen.R\"\n";
    let lexed = lex_default(source);
    assert_eq!(lexed.round_trip(source), source);
    assert!(matches!(
        lexed.tokens[0].metadata,
        TokenMetadata::Comment(CommentMetadata::InitialShebang)
    ));
    assert_eq!(
        lexed.tokens[1].metadata,
        TokenMetadata::LineEnding(LineEnding::CrLf)
    );
    assert!(matches!(
        lexed.tokens[2].metadata,
        TokenMetadata::Comment(CommentMetadata::Roxygen(RoxygenIndent::NonIndented))
    ));
    assert_eq!(
        lexed.tokens[3].metadata,
        TokenMetadata::LineEnding(LineEnding::Cr)
    );
    assert!(matches!(
        lexed.tokens[5].metadata,
        TokenMetadata::Comment(CommentMetadata::Roxygen(RoxygenIndent::Indented))
    ));
    assert!(matches!(
        lexed.tokens[7].metadata,
        TokenMetadata::Comment(CommentMetadata::LineDirective(ref directive))
            if directive.logical_line == 20 && directive.source_name.as_deref() == Some("gen.R")
    ));
}

#[test]
fn trailing_roxygen_prefix_is_an_ordinary_comment() {
    let source = "x <- 1 #' trailing\n  #' documentation\n";
    let lexed = lex_default(source);
    let comments = lexed
        .tokens
        .iter()
        .filter(|token| {
            matches!(
                token.kind,
                SyntaxKind::COMMENT | SyntaxKind::ROXYGEN_COMMENT
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(comments.len(), 2);
    assert!(matches!(
        comments[0].metadata,
        TokenMetadata::Comment(CommentMetadata::Ordinary)
    ));
    assert!(matches!(
        comments[1].metadata,
        TokenMetadata::Comment(CommentMetadata::Roxygen(RoxygenIndent::Indented))
    ));
}

#[test]
fn recognizes_names_keywords_and_dot_forms() {
    let source = "if naïve .name ... ..12 ..0 .1 `not a name` return .... ..1x";
    assert_eq!(
        kinds(source),
        vec![
            SyntaxKind::IF_KW,
            SyntaxKind::WHITESPACE,
            SyntaxKind::IDENTIFIER,
            SyntaxKind::WHITESPACE,
            SyntaxKind::IDENTIFIER,
            SyntaxKind::WHITESPACE,
            SyntaxKind::ELLIPSIS,
            SyntaxKind::WHITESPACE,
            SyntaxKind::DOT_DOT_I,
            SyntaxKind::WHITESPACE,
            SyntaxKind::IDENTIFIER,
            SyntaxKind::WHITESPACE,
            SyntaxKind::DOUBLE,
            SyntaxKind::WHITESPACE,
            SyntaxKind::IDENTIFIER,
            SyntaxKind::WHITESPACE,
            SyntaxKind::IDENTIFIER,
            SyntaxKind::WHITESPACE,
            SyntaxKind::IDENTIFIER,
            SyntaxKind::WHITESPACE,
            SyntaxKind::IDENTIFIER,
        ]
    );
}

#[test]
fn recognizes_every_reserved_word_exactly() {
    let source = "NULL NA TRUE FALSE Inf NaN NA_integer_ NA_real_ NA_complex_ NA_character_ function if else for in while repeat next break NULLx";
    let atoms: Vec<_> = lex_default(source)
        .tokens
        .into_iter()
        .filter(|token| !token.kind.is_trivia())
        .map(|token| token.kind)
        .collect();
    assert_eq!(
        atoms,
        [
            SyntaxKind::NULL_KW,
            SyntaxKind::NA_KW,
            SyntaxKind::TRUE_KW,
            SyntaxKind::FALSE_KW,
            SyntaxKind::INF_KW,
            SyntaxKind::NAN_KW,
            SyntaxKind::NA_INTEGER_KW,
            SyntaxKind::NA_REAL_KW,
            SyntaxKind::NA_COMPLEX_KW,
            SyntaxKind::NA_CHARACTER_KW,
            SyntaxKind::FUNCTION_KW,
            SyntaxKind::IF_KW,
            SyntaxKind::ELSE_KW,
            SyntaxKind::FOR_KW,
            SyntaxKind::IN_KW,
            SyntaxKind::WHILE_KW,
            SyntaxKind::REPEAT_KW,
            SyntaxKind::NEXT_KW,
            SyntaxKind::BREAK_KW,
            SyntaxKind::IDENTIFIER,
        ]
    );
}

#[test]
fn recognizes_and_validates_numbers() {
    let source = "1 1L 1i .5 1. 1e-2 0xff 0x1.fp3 1.2L 1e+ 0x 1.0L 1e3L .0L";
    let lexed = lex_default(source);
    assert_eq!(lexed.round_trip(source), source);
    let atoms: Vec<_> = lexed
        .tokens
        .iter()
        .filter(|token| !token.kind.is_trivia())
        .map(|t| t.kind)
        .collect();
    assert_eq!(
        atoms,
        [
            SyntaxKind::DOUBLE,
            SyntaxKind::INTEGER,
            SyntaxKind::COMPLEX,
            SyntaxKind::DOUBLE,
            SyntaxKind::DOUBLE,
            SyntaxKind::DOUBLE,
            SyntaxKind::DOUBLE,
            SyntaxKind::DOUBLE,
            SyntaxKind::INTEGER,
            SyntaxKind::DOUBLE,
            SyntaxKind::DOUBLE,
            SyntaxKind::INTEGER,
            SyntaxKind::INTEGER,
            SyntaxKind::INTEGER,
        ]
    );
    assert_eq!(lexed.diagnostics.len(), 3);
}

#[test]
fn strings_raw_strings_and_escapes_are_lossless() {
    let source = r#""ok\n\x41" 'bad\q' r"(a)b)" R"---|a|b|---" `a\`b` r"(no end"#;
    let lexed = lex_default(source);
    assert_eq!(lexed.round_trip(source), source);
    assert_eq!(
        lexed
            .tokens
            .iter()
            .filter(|t| !t.kind.is_trivia())
            .map(|t| t.kind)
            .collect::<Vec<_>>(),
        [
            SyntaxKind::STRING,
            SyntaxKind::STRING,
            SyntaxKind::RAW_STRING,
            SyntaxKind::RAW_STRING,
            SyntaxKind::IDENTIFIER,
            SyntaxKind::ERROR_TOKEN
        ]
    );
    assert_eq!(lexed.diagnostics.len(), 2);
}

#[test]
fn all_raw_delimiters_and_multiline_quote_diagnostics_work() {
    let source = "r\"[x]\" R\"-{x}-\" r\"|x|\" \"a\nb\"\n#line 9";
    let lexed = lex_default(source);
    assert_eq!(lexed.round_trip(source), source);
    assert_eq!(
        lexed
            .tokens
            .iter()
            .filter(|token| token.kind == SyntaxKind::RAW_STRING)
            .count(),
        3
    );
    assert!(lexed
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == r_lexer::LexicalDiagnosticKind::InvalidString));
    assert!(lexed.tokens.iter().any(|token| matches!(
        token.metadata,
        TokenMetadata::Comment(CommentMetadata::LineDirective(ref directive))
            if directive.physical_line == 2
    )));
}

#[test]
fn operators_use_longest_match_and_closing_brackets_are_independent() {
    let source = "<<- ->> ::: :: [[ ]] ** ^ := |> => <= >= == != <- -> && || + ]";
    let lexed = lex_default(source);
    assert_eq!(lexed.round_trip(source), source);
    let atoms: Vec<_> = lexed
        .tokens
        .iter()
        .filter(|t| !t.kind.is_trivia())
        .map(|t| t.kind)
        .collect();
    assert_eq!(
        atoms,
        [
            SyntaxKind::SUPER_LEFT_ASSIGN,
            SyntaxKind::SUPER_RIGHT_ASSIGN,
            SyntaxKind::NS_GET_INTERNAL,
            SyntaxKind::NS_GET,
            SyntaxKind::L_DBRACKET,
            SyntaxKind::R_BRACKET,
            SyntaxKind::R_BRACKET,
            SyntaxKind::CARET,
            SyntaxKind::CARET,
            SyntaxKind::WALRUS,
            SyntaxKind::NATIVE_PIPE,
            SyntaxKind::PIPE_BIND,
            SyntaxKind::LE,
            SyntaxKind::GE,
            SyntaxKind::EQ2,
            SyntaxKind::NE,
            SyntaxKind::LEFT_ASSIGN,
            SyntaxKind::RIGHT_ASSIGN,
            SyntaxKind::AMP2,
            SyntaxKind::PIPE2,
            SyntaxKind::PLUS,
            SyntaxKind::R_BRACKET,
        ]
    );
    assert!(matches!(
        lexed
            .tokens
            .iter()
            .find(|t| t.text(source) == "**")
            .unwrap()
            .metadata,
        TokenMetadata::DoubleStar
    ));
    assert_eq!(
        lexed
            .tokens
            .iter()
            .find(|t| t.text(source) == "|>")
            .unwrap()
            .kind,
        SyntaxKind::NATIVE_PIPE
    );
}

#[test]
fn unicode_escapes_must_encode_scalars() {
    let source = r#""\u0041\U0001f980" "\uD800" "\U00110000""#;
    let lexed = lex_default(source);
    assert_eq!(lexed.round_trip(source), source);
    assert_eq!(lexed.diagnostics.len(), 2);
}

#[test]
fn suppressed_diagnostics_do_not_hide_lexical_errors() {
    let config = LexerConfig {
        limits: LexerLimits {
            max_diagnostics: 0,
            ..LexerLimits::default()
        },
        ..LexerConfig::default()
    };
    let lexed = lex("'", &config);
    assert!(lexed.had_errors);
    assert!(lexed.diagnostics.is_empty());
    assert!(lexed.diagnostics_truncated);
}

#[test]
fn special_operators_stop_before_newline() {
    let source = "%ok% %escaped\\%% %unterminated\nx";
    let lexed = lex_default(source);
    assert_eq!(lexed.round_trip(source), source);
    assert_eq!(lexed.tokens[0].kind, SyntaxKind::SPECIAL);
    assert!(lexed
        .tokens
        .iter()
        .any(|t| t.kind == SyntaxKind::ERROR_TOKEN && t.text(source) == "%unterminated"));
}

#[test]
fn limits_collapse_remainder_without_losing_text() {
    let config = LexerConfig {
        limits: LexerLimits {
            max_tokens: 2,
            ..LexerLimits::default()
        },
        ..LexerConfig::default()
    };
    let source = "a + b + c";
    let lexed = lex(source, &config);
    assert!(lexed.tokens_truncated);
    assert_eq!(lexed.round_trip(source), source);
    assert_eq!(lexed.tokens.last().unwrap().kind, SyntaxKind::ERROR_TOKEN);
}

#[test]
fn arbitrary_utf8_round_trips_progresses_and_is_deterministic() {
    let alphabet = [
        "a", "1", " ", "\n", "\r", "#", "'", "\\", "%", "é", "λ", "🦀", "\0",
    ];
    let mut state = 0x1234_5678_u64;
    for _ in 0..500 {
        let mut source = String::new();
        for _ in 0..(state as usize % 80) {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            source.push_str(alphabet[state as usize % alphabet.len()]);
        }
        let first = lex_default(&source);
        let second = lex_default(&source);
        assert_eq!(first, second);
        assert_eq!(first.round_trip(&source), source);
        let mut end = 0_u32;
        for token in &first.tokens {
            assert_eq!(u32::from(token.range.start()), end);
            assert!(token.range.end() > token.range.start());
            end = token.range.end().into();
        }
        assert_eq!(end as usize, source.len());
    }
}
