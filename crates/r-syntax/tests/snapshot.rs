use r_syntax::*;
use std::sync::Arc;

use r_source::{CompatibilityProfile, EncodingProfile, SourceLimits, SourceText};

fn tree(text: &str) -> GreenNode {
    let mut builder = GreenNodeBuilder::new();
    builder.start_node(rowan::SyntaxKind(SyntaxKind::SOURCE_FILE.as_u16()));
    builder.token(rowan::SyntaxKind(SyntaxKind::IDENTIFIER.as_u16()), text);
    builder.finish_node();
    builder.finish()
}

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn snapshot_is_send_and_sync() {
    assert_send_sync::<ParseSnapshot>();
}

#[test]
fn snapshot_can_retain_source_text_and_project_ranges() {
    let source = SourceText::from_bytes(
        Arc::<[u8]>::from(&b"#line 10 \"generated.R\"\n\xe9"[..]),
        Some(Arc::from("input.R")),
        CompatibilityProfile::default(),
        EncodingProfile::LATIN1,
        SourceLimits::UNLIMITED,
    )
    .unwrap();
    let decoded = source.text().to_owned();
    let start = decoded.len() - 2;
    let snapshot = ParseSnapshot::new_with_source_text(
        DocumentId(8),
        source,
        tree(&decoded),
        [],
        Completeness::Complete,
        SyntaxProfile::default(),
    );

    assert_eq!(snapshot.source(), decoded);
    assert_eq!(
        snapshot.source_text().unwrap().original_bytes().last(),
        Some(&0xe9)
    );
    assert_eq!(
        snapshot.original_byte_range(TextRange::new(
            TextSize::from(start as u32),
            TextSize::from((start + 2) as u32),
        )),
        Some((start)..(start + 1))
    );
    let location = snapshot
        .source_text()
        .unwrap()
        .logical_map()
        .location(1, 0)
        .unwrap();
    assert_eq!(location.line, 10);
    assert_eq!(location.source_name.as_deref(), Some("generated.R"));
}

#[test]
fn root_is_recreated_and_lossless() {
    let snapshot = ParseSnapshot::new(
        DocumentId(7),
        "x + 1",
        tree("x + 1"),
        Vec::<Diagnostic>::new(),
        Completeness::Complete,
        SyntaxProfile::default(),
    );
    let first = snapshot.root();
    let second = snapshot.root();
    assert_eq!(first.text().to_string(), snapshot.source());
    assert_eq!(first, second);
    assert_eq!(first.kind(), SyntaxKind::SOURCE_FILE);
    assert!(validate_snapshot(&snapshot).is_ok());
}

#[test]
fn locations_resolve_against_the_same_document() {
    let snapshot = ParseSnapshot::new(
        DocumentId(9),
        "name",
        tree("name"),
        [],
        Completeness::Complete,
        SyntaxProfile::default(),
    );
    let token = snapshot.root().first_token().unwrap();
    let location = SyntaxLocation::new(snapshot.document(), &token.clone().into());
    assert_eq!(
        location
            .resolve(&snapshot)
            .unwrap()
            .into_token()
            .unwrap()
            .text(),
        "name"
    );

    let other = ParseSnapshot::new(
        DocumentId(10),
        "name",
        tree("name"),
        [],
        Completeness::Complete,
        SyntaxProfile::default(),
    );
    assert_eq!(location.resolve(&other), Err(LocationError::WrongDocument));
}

#[test]
fn malformed_tree_still_has_typed_root() {
    use r_syntax::RowanAstNode;
    let snapshot = ParseSnapshot::new(
        DocumentId(11),
        "?",
        tree("?"),
        [],
        Completeness::Invalid,
        SyntaxProfile::default(),
    );
    assert!(SourceFile::cast(snapshot.root()).is_some());
    assert_eq!(snapshot.completeness(), Completeness::Invalid);
}

#[test]
fn source_file_traverses_expressions_inside_its_list() {
    use r_syntax::RowanAstNode;

    let mut builder = GreenNodeBuilder::new();
    builder.start_node(rowan::SyntaxKind(SyntaxKind::SOURCE_FILE.as_u16()));
    builder.start_node(rowan::SyntaxKind(SyntaxKind::EXPRESSION_LIST.as_u16()));
    builder.start_node(rowan::SyntaxKind(SyntaxKind::IDENTIFIER_EXPR.as_u16()));
    builder.token(rowan::SyntaxKind(SyntaxKind::IDENTIFIER.as_u16()), "x");
    builder.finish_node();
    builder.finish_node();
    builder.finish_node();
    let root = SyntaxNode::new_root(builder.finish());
    let source = SourceFile::cast(root).unwrap();
    assert_eq!(source.expressions().count(), 1);
}

#[test]
fn centralized_name_predicates_include_raw_strings() {
    assert!(SyntaxKind::IDENTIFIER.is_name_token());
    assert!(SyntaxKind::RAW_STRING.is_name_or_string_token());
    assert!(SyntaxKind::NULL_KW.is_tag_token());
    assert!(!SyntaxKind::ELLIPSIS.is_name_token());
}
