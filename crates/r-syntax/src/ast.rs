use crate::{SyntaxKind, SyntaxNode, SyntaxToken};
use rowan::ast::{support, AstChildren, AstNode};

pub use rowan::ast::{AstNode as RowanAstNode, AstPtr, SyntaxNodePtr};

pub trait AstNodeExt: AstNode<Language = crate::RLanguage> {
    fn child<N: AstNode<Language = crate::RLanguage>>(&self) -> Option<N> {
        support::child(self.syntax())
    }
    fn children<N: AstNode<Language = crate::RLanguage>>(&self) -> AstChildren<N> {
        support::children(self.syntax())
    }
    fn token(&self, kind: SyntaxKind) -> Option<SyntaxToken> {
        support::token(self.syntax(), kind)
    }
}
impl<T: AstNode<Language = crate::RLanguage>> AstNodeExt for T {}

macro_rules! ast_node {
    ($name:ident, $kind:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(SyntaxNode);
        impl AstNode for $name {
            type Language = crate::RLanguage;
            fn can_cast(kind: SyntaxKind) -> bool {
                kind == SyntaxKind::$kind
            }
            fn cast(node: SyntaxNode) -> Option<Self> {
                Self::can_cast(node.kind()).then_some(Self(node))
            }
            fn syntax(&self) -> &SyntaxNode {
                &self.0
            }
        }
    };
}

ast_node!(SourceFile, SOURCE_FILE);
ast_node!(ErrorNode, ERROR);
ast_node!(Missing, MISSING);
ast_node!(ExpressionList, EXPRESSION_LIST);
ast_node!(IdentifierExpr, IDENTIFIER_EXPR);
ast_node!(LiteralExpr, LITERAL_EXPR);
ast_node!(ParenExpr, PAREN_EXPR);
ast_node!(BracedExpr, BRACED_EXPR);
ast_node!(UnaryExpr, UNARY_EXPR);
ast_node!(BinaryExpr, BINARY_EXPR);
ast_node!(AssignmentExpr, ASSIGNMENT_EXPR);
ast_node!(CallExpr, CALL_EXPR);
ast_node!(ArgumentList, ARGUMENT_LIST);
ast_node!(Argument, ARGUMENT);
ast_node!(FunctionExpr, FUNCTION_EXPR);
ast_node!(ParameterList, PARAMETER_LIST);
ast_node!(Parameter, PARAMETER);
ast_node!(IfExpr, IF_EXPR);
ast_node!(WhileExpr, WHILE_EXPR);
ast_node!(ForExpr, FOR_EXPR);
ast_node!(RepeatExpr, REPEAT_EXPR);
ast_node!(NextExpr, NEXT_EXPR);
ast_node!(BreakExpr, BREAK_EXPR);
ast_node!(SubsetExpr, SUBSET_EXPR);
ast_node!(Subset2Expr, SUBSET2_EXPR);
ast_node!(MemberExpr, MEMBER_EXPR);
ast_node!(NamespaceExpr, NAMESPACE_EXPR);
ast_node!(FormulaExpr, FORMULA_EXPR);
ast_node!(HelpExpr, HELP_EXPR);
ast_node!(PipeExpr, PIPE_EXPR);
ast_node!(IndexArgumentList, INDEX_ARGUMENT_LIST);
ast_node!(Condition, CONDITION);
ast_node!(ElseClause, ELSE_CLAUSE);

/// Any major expression node, preserving malformed children without requiring them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Expr(SyntaxNode);

impl AstNode for Expr {
    type Language = crate::RLanguage;
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::IDENTIFIER_EXPR
                | SyntaxKind::LITERAL_EXPR
                | SyntaxKind::PAREN_EXPR
                | SyntaxKind::BRACED_EXPR
                | SyntaxKind::UNARY_EXPR
                | SyntaxKind::BINARY_EXPR
                | SyntaxKind::ASSIGNMENT_EXPR
                | SyntaxKind::CALL_EXPR
                | SyntaxKind::FUNCTION_EXPR
                | SyntaxKind::IF_EXPR
                | SyntaxKind::WHILE_EXPR
                | SyntaxKind::FOR_EXPR
                | SyntaxKind::REPEAT_EXPR
                | SyntaxKind::NEXT_EXPR
                | SyntaxKind::BREAK_EXPR
                | SyntaxKind::SUBSET_EXPR
                | SyntaxKind::SUBSET2_EXPR
                | SyntaxKind::MEMBER_EXPR
                | SyntaxKind::NAMESPACE_EXPR
                | SyntaxKind::FORMULA_EXPR
                | SyntaxKind::HELP_EXPR
                | SyntaxKind::PIPE_EXPR
                | SyntaxKind::ERROR
                | SyntaxKind::MISSING
        )
    }
    fn cast(node: SyntaxNode) -> Option<Self> {
        Self::can_cast(node.kind()).then_some(Self(node))
    }
    fn syntax(&self) -> &SyntaxNode {
        &self.0
    }
}

impl SourceFile {
    pub fn expression_list(&self) -> Option<ExpressionList> {
        support::child(self.syntax())
    }

    pub fn expressions(&self) -> impl Iterator<Item = Expr> + '_ {
        self.expression_list()
            .into_iter()
            .flat_map(|list| list.syntax().children().filter_map(Expr::cast))
    }
}

impl CallExpr {
    pub fn callee(&self) -> Option<Expr> {
        self.syntax().children().find_map(Expr::cast)
    }
    pub fn arguments(&self) -> Option<ArgumentList> {
        support::child(self.syntax())
    }
}

impl ArgumentList {
    pub fn arguments(&self) -> AstChildren<Argument> {
        support::children(self.syntax())
    }
}

impl FunctionExpr {
    pub fn parameters(&self) -> Option<ParameterList> {
        support::child(self.syntax())
    }
    pub fn body(&self) -> Option<Expr> {
        self.syntax().children().filter_map(Expr::cast).last()
    }
}

impl ParameterList {
    pub fn parameters(&self) -> AstChildren<Parameter> {
        support::children(self.syntax())
    }
}

impl BinaryExpr {
    pub fn operands(&self) -> impl Iterator<Item = Expr> + '_ {
        self.syntax().children().filter_map(Expr::cast)
    }
}

impl IfExpr {
    pub fn condition(&self) -> Option<Condition> {
        support::child(self.syntax())
    }
    pub fn branches(&self) -> impl Iterator<Item = Expr> + '_ {
        self.syntax().children().filter_map(Expr::cast)
    }
}
