#![allow(non_camel_case_types)]

use core::fmt;

macro_rules! syntax_kinds {
    ($($name:ident = $value:literal),+ $(,)?) => {
        /// Stable tags shared by the R lexer, parser, CST, and typed AST.
        ///
        /// Values are part of the crate's serialized/inter-crate contract. Token
        /// values occupy `0..256`; node values start at `256`.
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(u16)]
        pub enum SyntaxKind { $($name = $value),+ }

        impl SyntaxKind {
            /// Converts a stable numeric tag, rejecting values unknown to this build.
            pub const fn from_u16(value: u16) -> Option<Self> {
                match value { $($value => Some(Self::$name),)+ _ => None }
            }

            pub const fn as_u16(self) -> u16 { self as u16 }
        }

        impl fmt::Debug for SyntaxKind {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(match self { $(Self::$name => stringify!($name),)+ })
            }
        }
    };
}

syntax_kinds! {
    TOMBSTONE = 0,
    EOF = 1,
    ERROR_TOKEN = 2,
    MISSING_TOKEN = 3,

    WHITESPACE = 10,
    NEWLINE = 11,
    COMMENT = 12,
    ROXYGEN_COMMENT = 13,

    IDENTIFIER = 20,
    DOT_DOT_I = 21,
    ELLIPSIS = 22,
    INTEGER = 23,
    DOUBLE = 24,
    COMPLEX = 25,
    STRING = 26,
    RAW_STRING = 27,

    NULL_KW = 40,
    NA_KW = 41,
    TRUE_KW = 42,
    FALSE_KW = 43,
    INF_KW = 44,
    NAN_KW = 45,
    NA_INTEGER_KW = 46,
    NA_REAL_KW = 47,
    NA_COMPLEX_KW = 48,
    NA_CHARACTER_KW = 49,
    FUNCTION_KW = 50,
    IF_KW = 51,
    ELSE_KW = 52,
    FOR_KW = 53,
    IN_KW = 54,
    WHILE_KW = 55,
    REPEAT_KW = 56,
    NEXT_KW = 57,
    BREAK_KW = 58,

    L_PAREN = 70,
    R_PAREN = 71,
    L_BRACE = 72,
    R_BRACE = 73,
    L_BRACKET = 74,
    R_BRACKET = 75,
    L_DBRACKET = 76,
    R_DBRACKET = 77,
    COMMA = 78,
    SEMICOLON = 79,

    PLUS = 90,
    MINUS = 91,
    STAR = 92,
    SLASH = 93,
    CARET = 94,
    COLON = 95,
    TILDE = 96,
    QUESTION = 97,
    BANG = 98,
    AMP = 99,
    AMP2 = 100,
    PIPE = 101,
    PIPE2 = 102,
    LT = 103,
    LE = 104,
    GT = 105,
    GE = 106,
    EQ2 = 107,
    NE = 108,
    EQ = 109,
    LEFT_ASSIGN = 110,
    SUPER_LEFT_ASSIGN = 111,
    RIGHT_ASSIGN = 112,
    SUPER_RIGHT_ASSIGN = 113,
    WALRUS = 114,
    NS_GET = 115,
    NS_GET_INTERNAL = 116,
    DOLLAR = 117,
    AT = 118,
    SPECIAL = 119,
    PIPE_BIND = 120,
    BACKSLASH = 121,
    UNDERSCORE = 122,
    NATIVE_PIPE = 123,

    SOURCE_FILE = 256,
    ERROR = 257,
    MISSING = 258,
    EXPRESSION_LIST = 259,
    IDENTIFIER_EXPR = 260,
    LITERAL_EXPR = 261,
    PAREN_EXPR = 262,
    BRACED_EXPR = 263,
    UNARY_EXPR = 264,
    BINARY_EXPR = 265,
    ASSIGNMENT_EXPR = 266,
    CALL_EXPR = 267,
    ARGUMENT_LIST = 268,
    ARGUMENT = 269,
    FUNCTION_EXPR = 270,
    PARAMETER_LIST = 271,
    PARAMETER = 272,
    IF_EXPR = 273,
    WHILE_EXPR = 274,
    FOR_EXPR = 275,
    REPEAT_EXPR = 276,
    NEXT_EXPR = 277,
    BREAK_EXPR = 278,
    SUBSET_EXPR = 279,
    SUBSET2_EXPR = 280,
    MEMBER_EXPR = 281,
    NAMESPACE_EXPR = 282,
    FORMULA_EXPR = 283,
    HELP_EXPR = 284,
    PIPE_EXPR = 285,
    INDEX_ARGUMENT_LIST = 286,
    CONDITION = 287,
    ELSE_CLAUSE = 288,
}

impl SyntaxKind {
    pub const fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::WHITESPACE | Self::NEWLINE | Self::COMMENT | Self::ROXYGEN_COMMENT
        )
    }

    pub const fn is_token(self) -> bool {
        self.as_u16() < 256
    }

    pub const fn is_node(self) -> bool {
        !self.is_token()
    }

    pub const fn is_literal_token(self) -> bool {
        matches!(
            self,
            Self::INTEGER
                | Self::DOUBLE
                | Self::COMPLEX
                | Self::STRING
                | Self::RAW_STRING
                | Self::NULL_KW
                | Self::NA_KW
                | Self::TRUE_KW
                | Self::FALSE_KW
                | Self::INF_KW
                | Self::NAN_KW
                | Self::NA_INTEGER_KW
                | Self::NA_REAL_KW
                | Self::NA_COMPLEX_KW
                | Self::NA_CHARACTER_KW
        )
    }

    /// Tokens accepted where R requires a syntactic name.
    pub const fn is_name_token(self) -> bool {
        matches!(self, Self::IDENTIFIER | Self::DOT_DOT_I)
    }

    /// Tokens accepted as names or quoted names in operand and tag positions.
    pub const fn is_name_or_string_token(self) -> bool {
        self.is_name_token() || matches!(self, Self::STRING | Self::RAW_STRING)
    }

    /// Tokens accepted as a top-level argument or subset tag.
    pub const fn is_tag_token(self) -> bool {
        self.is_name_or_string_token() || matches!(self, Self::NULL_KW)
    }

    pub const fn is_keyword(self) -> bool {
        matches!(
            self,
            Self::NULL_KW
                | Self::NA_KW
                | Self::TRUE_KW
                | Self::FALSE_KW
                | Self::INF_KW
                | Self::NAN_KW
                | Self::NA_INTEGER_KW
                | Self::NA_REAL_KW
                | Self::NA_COMPLEX_KW
                | Self::NA_CHARACTER_KW
                | Self::FUNCTION_KW
                | Self::IF_KW
                | Self::ELSE_KW
                | Self::FOR_KW
                | Self::IN_KW
                | Self::WHILE_KW
                | Self::REPEAT_KW
                | Self::NEXT_KW
                | Self::BREAK_KW
        )
    }

    pub fn keyword(text: &str) -> Option<Self> {
        Some(match text {
            "NULL" => Self::NULL_KW,
            "NA" => Self::NA_KW,
            "TRUE" => Self::TRUE_KW,
            "FALSE" => Self::FALSE_KW,
            "Inf" => Self::INF_KW,
            "NaN" => Self::NAN_KW,
            "NA_integer_" => Self::NA_INTEGER_KW,
            "NA_real_" => Self::NA_REAL_KW,
            "NA_complex_" => Self::NA_COMPLEX_KW,
            "NA_character_" => Self::NA_CHARACTER_KW,
            "function" => Self::FUNCTION_KW,
            "if" => Self::IF_KW,
            "else" => Self::ELSE_KW,
            "for" => Self::FOR_KW,
            "in" => Self::IN_KW,
            "while" => Self::WHILE_KW,
            "repeat" => Self::REPEAT_KW,
            "next" => Self::NEXT_KW,
            "break" => Self::BREAK_KW,
            _ => return None,
        })
    }
}
