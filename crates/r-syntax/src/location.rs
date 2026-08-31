use crate::{DocumentId, ParseSnapshot, SyntaxElement, SyntaxKind, SyntaxNode};
use rowan::{NodeOrToken, TextRange};

/// Durable location of a node or token in one immutable document snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SyntaxLocation {
    document: DocumentId,
    path: Box<[u32]>,
    kind: SyntaxKind,
    range: TextRange,
    token: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationError {
    WrongDocument,
    MissingElement,
    StaleElement,
}

impl SyntaxLocation {
    pub fn new(document: DocumentId, element: &SyntaxElement) -> Self {
        let (parent, token) = match element {
            NodeOrToken::Node(node) => (Some(node.clone()), false),
            NodeOrToken::Token(token) => (token.parent(), true),
        };
        let mut indices = Vec::new();
        if token {
            let token = element.as_token().expect("token branch");
            let parent = parent.as_ref().expect("tokens always have a parent");
            indices.push(
                parent
                    .children_with_tokens()
                    .position(|item| &item == element)
                    .unwrap_or(token.index()) as u32,
            );
        }
        let mut current = parent;
        while let Some(node) = current {
            if let Some(parent) = node.parent() {
                indices.push(
                    parent
                        .children_with_tokens()
                        .position(|item| item.as_node() == Some(&node))
                        .unwrap_or(node.index()) as u32,
                );
                current = Some(parent);
            } else {
                current = None;
            }
        }
        indices.reverse();
        Self {
            document,
            path: indices.into_boxed_slice(),
            kind: element.kind(),
            range: element.text_range(),
            token,
        }
    }

    pub fn for_node(document: DocumentId, node: &SyntaxNode) -> Self {
        Self::new(document, &node.clone().into())
    }
    pub fn document(&self) -> DocumentId {
        self.document
    }
    pub fn path(&self) -> &[u32] {
        &self.path
    }
    pub fn kind(&self) -> SyntaxKind {
        self.kind
    }
    pub fn range(&self) -> TextRange {
        self.range
    }
    pub fn is_token(&self) -> bool {
        self.token
    }

    pub fn resolve(&self, snapshot: &ParseSnapshot) -> Result<SyntaxElement, LocationError> {
        if self.document != snapshot.document() {
            return Err(LocationError::WrongDocument);
        }
        let root = snapshot.root();
        if self.path.is_empty() {
            let element: SyntaxElement = root.into();
            return self.check(element);
        }
        let mut node = root;
        for (depth, index) in self.path.iter().copied().enumerate() {
            let element = node
                .children_with_tokens()
                .nth(index as usize)
                .ok_or(LocationError::MissingElement)?;
            if depth + 1 == self.path.len() {
                return self.check(element);
            }
            node = element.into_node().ok_or(LocationError::MissingElement)?;
        }
        Err(LocationError::MissingElement)
    }

    fn check(&self, element: SyntaxElement) -> Result<SyntaxElement, LocationError> {
        if element.kind() == self.kind
            && element.text_range() == self.range
            && element.as_token().is_some() == self.token
        {
            Ok(element)
        } else {
            Err(LocationError::StaleElement)
        }
    }
}
