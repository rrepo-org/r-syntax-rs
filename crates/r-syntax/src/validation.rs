use crate::{ParseSnapshot, SyntaxKind, SyntaxNode, WalkEvent};
use rowan::NodeOrToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TreeFingerprint(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationErrorKind {
    InvalidRoot,
    NodeKindUsedAsToken,
    TokenKindUsedAsNode,
    SourceMismatch,
    OutOfBoundsDiagnostic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub kind: ValidationErrorKind,
    pub message: String,
}

pub fn validate_snapshot(snapshot: &ParseSnapshot) -> Result<(), Vec<ValidationError>> {
    let root = snapshot.root();
    let mut errors = Vec::new();
    if root.kind() != SyntaxKind::SOURCE_FILE {
        errors.push(error(
            ValidationErrorKind::InvalidRoot,
            "root kind must be SOURCE_FILE",
        ));
    }
    for event in root.preorder_with_tokens() {
        if let WalkEvent::Enter(element) = event {
            match element {
                NodeOrToken::Node(node) if !node.kind().is_node() => errors.push(error(
                    ValidationErrorKind::TokenKindUsedAsNode,
                    format!("{:?} used as a node", node.kind()),
                )),
                NodeOrToken::Token(token) if !token.kind().is_token() => errors.push(error(
                    ValidationErrorKind::NodeKindUsedAsToken,
                    format!("{:?} used as a token", token.kind()),
                )),
                _ => {}
            }
        }
    }
    if root.text() != snapshot.source() {
        errors.push(error(
            ValidationErrorKind::SourceMismatch,
            "tree text differs from decoded source",
        ));
    }
    let source_len = rowan::TextSize::of(snapshot.source());
    for diagnostic in snapshot.diagnostics() {
        if diagnostic.range.end() > source_len || diagnostic.recovery.range.end() > source_len {
            errors.push(error(
                ValidationErrorKind::OutOfBoundsDiagnostic,
                format!("diagnostic {} is outside the source", diagnostic.code),
            ));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn error(kind: ValidationErrorKind, message: impl Into<String>) -> ValidationError {
    ValidationError {
        kind,
        message: message.into(),
    }
}

/// Stable FNV-1a fingerprint over kinds, boundaries, and token bytes.
pub fn fingerprint(root: &SyntaxNode) -> TreeFingerprint {
    let mut hash = 0xcbf29ce484222325_u64;
    for event in root.preorder_with_tokens() {
        let (marker, element) = match event {
            WalkEvent::Enter(element) => (1_u8, element),
            WalkEvent::Leave(element) => (2_u8, element),
        };
        feed(&mut hash, &[marker]);
        feed(&mut hash, &element.kind().as_u16().to_le_bytes());
        if let NodeOrToken::Token(token) = element {
            feed(&mut hash, token.text().as_bytes());
        }
    }
    TreeFingerprint(hash)
}

fn feed(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}
