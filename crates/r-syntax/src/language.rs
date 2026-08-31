use crate::SyntaxKind;

/// Rowan language marker for the R concrete syntax tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RLanguage {}

impl rowan::Language for RLanguage {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> Self::Kind {
        SyntaxKind::from_u16(raw.0).unwrap_or(SyntaxKind::ERROR_TOKEN)
    }

    fn kind_to_raw(kind: Self::Kind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind.as_u16())
    }
}

pub type SyntaxNode = rowan::SyntaxNode<RLanguage>;
pub type SyntaxToken = rowan::SyntaxToken<RLanguage>;
pub type SyntaxElement = rowan::SyntaxElement<RLanguage>;
pub type SyntaxNodeChildren = rowan::SyntaxNodeChildren<RLanguage>;
pub type SyntaxElementChildren = rowan::SyntaxElementChildren<RLanguage>;
