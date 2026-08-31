use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const INVENTORIES: &[(&str, &str, &str)] = &[
    ("tokens", "docs/tokens.md", "T_"),
    ("roxygen-tokens", "docs/tokens.md", "RT_"),
    ("grammar", "docs/grammar.md", "G"),
    ("roxygen-grammar", "docs/grammar.md", "RG"),
    ("operators", "docs/operators.md", "O"),
];

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let command = env::args().nth(1).unwrap_or_else(|| "help".to_owned());
    let root = option_env!("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or(
            env::current_dir()
                .map_err(|error| format!("cannot read current directory: {error}"))?,
        );
    match command.as_str() {
        "inventory" => inventory(&root),
        "check" => check(&root),
        "help" | "--help" | "-h" => {
            println!("usage: cargo run -p xtask -- <inventory|check>");
            Ok(())
        }
        _ => Err(format!(
            "unknown command {command:?}; expected inventory or check"
        )),
    }
}

fn inventory(root: &Path) -> Result<(), String> {
    for &(name, relative, prefix) in INVENTORIES {
        let ids = read_inventory(root, name, relative, prefix)?;
        println!("{name}: {}", ids.len());
        println!("  {}", ids.join(" "));
    }
    Ok(())
}

fn check(root: &Path) -> Result<(), String> {
    let required = [
        "README.md",
        "docs/phase-1.md",
        "docs/tokens.md",
        "docs/grammar.md",
        "docs/operators.md",
        "docs/conformance.md",
        "docs/roxygen.md",
    ];
    let mut combined = String::new();
    for relative in required {
        let text = read(root.join(relative))?;
        combined.push_str(&text);
    }
    for exact in ["R 4.6.1", "roxygen2 8.1.0", "Rowan 0.17"] {
        if !combined.contains(exact) {
            return Err(format!(
                "specification does not record exact default {exact}"
            ));
        }
    }

    require_rust_pins(root)?;

    let mut inventories = Vec::new();
    for &(name, relative, prefix) in INVENTORIES {
        let ids = read_inventory(root, name, relative, prefix)?;
        if ids.is_empty() {
            return Err(format!("{name} inventory is empty"));
        }
        let unique: BTreeSet<_> = ids.iter().collect();
        if unique.len() != ids.len() {
            return Err(format!("{name} inventory contains duplicate IDs"));
        }
        inventories.push((name, ids));
    }

    require_sequence(&inventories[2].1, "G", 1, 45)?;
    require_sequence(&inventories[3].1, "RG", 1, 15)?;
    require_sequence(&inventories[4].1, "O", 1, 21)?;
    let tokens: BTreeSet<&str> = inventories[0].1.iter().map(String::as_str).collect();
    let operator_text = read(root.join("docs/operators.md"))?;
    for referenced in backtick_ids(&operator_text, "T_") {
        if !tokens.contains(referenced.as_str()) {
            return Err(format!(
                "operator table references unknown token {referenced}"
            ));
        }
    }

    let r_kinds = read_rust_kinds(root, "crates/r-syntax/src/kind.rs", "syntax_kinds!")?;
    let roxygen_kinds = read_rust_kinds(root, "crates/r-roxygen/src/lib.rs", "kinds!")?;
    validate_kind_inventory("R", &r_kinds, &["EOF", "ROXYGEN_COMMENT", "SOURCE_FILE"])?;
    validate_kind_inventory(
        "roxygen",
        &roxygen_kinds,
        &["LINE_PREFIX", "SIDECAR", "ROXYGEN_BLOCK"],
    )?;
    check_fixtures(root)?;

    println!(
        "check: ok ({} documented R tokens, {} documented roxygen tokens, {} R productions, {} roxygen productions, {} operator rows, {} Rust R kinds, {} Rust roxygen kinds, 7 fixtures)",
        inventories[0].1.len(),
        inventories[1].1.len(),
        inventories[2].1.len(),
        inventories[3].1.len(),
        inventories[4].1.len(),
        r_kinds.len(),
        roxygen_kinds.len()
    );
    Ok(())
}

fn require_rust_pins(root: &Path) -> Result<(), String> {
    let sources = [
        (
            "crates/r-source/src/profile.rs",
            "pub const R_4_6_1: Self = Self::new(4, 6, 1);",
        ),
        (
            "crates/r-syntax/src/snapshot.rs",
            "r: Version::new(4, 6, 1),",
        ),
        (
            "crates/r-syntax/src/snapshot.rs",
            "roxygen2: Version::new(8, 1, 0),",
        ),
        (
            "crates/r-conformance/src/lib.rs",
            "pub const DEFAULT_R_VERSION: &str = \"4.6.1\";",
        ),
        (
            "crates/r-conformance/src/lib.rs",
            "pub const DEFAULT_ROXYGEN2_VERSION: &str = \"8.1.0\";",
        ),
        (
            "crates/r-conformance/src/lib.rs",
            "pub const DEFAULT_ROWAN_VERSION: &str = \"0.17\";",
        ),
    ];
    for (relative, expected) in sources {
        if !read(root.join(relative))?.contains(expected) {
            return Err(format!(
                "{relative} does not contain pinned constant {expected:?}"
            ));
        }
    }
    Ok(())
}

fn read_rust_kinds(
    root: &Path,
    relative: &str,
    invocation: &str,
) -> Result<Vec<(String, u16)>, String> {
    let text = read(root.join(relative))?;
    let body = text
        .split_once(invocation)
        .and_then(|(_, rest)| rest.split_once('{').map(|(_, body)| body))
        .and_then(|body| body.split_once('}').map(|(body, _)| body))
        .ok_or_else(|| format!("cannot locate {invocation} kind table in {relative}"))?;
    let mut kinds = Vec::new();
    for line in body.lines() {
        let line = line.trim().trim_end_matches(',');
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if !name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte == b'_')
        {
            continue;
        }
        let value = value
            .trim()
            .parse::<u16>()
            .map_err(|error| format!("invalid kind value in {relative}: {error}"))?;
        kinds.push((name.to_owned(), value));
    }
    Ok(kinds)
}

fn validate_kind_inventory(
    name: &str,
    kinds: &[(String, u16)],
    required: &[&str],
) -> Result<(), String> {
    if kinds.is_empty() {
        return Err(format!("{name} Rust kind inventory is empty"));
    }
    let names: BTreeSet<&str> = kinds.iter().map(|(kind, _)| kind.as_str()).collect();
    let values: BTreeSet<_> = kinds.iter().map(|(_, value)| value).collect();
    if names.len() != kinds.len() || values.len() != kinds.len() {
        return Err(format!("{name} Rust kind names and values must be unique"));
    }
    if !kinds.iter().any(|(_, value)| *value < 256) || !kinds.iter().any(|(_, value)| *value >= 256)
    {
        return Err(format!("{name} Rust kinds must contain tokens and nodes"));
    }
    for required in required {
        if !names.contains(required) {
            return Err(format!("{name} Rust kind inventory is missing {required}"));
        }
    }
    Ok(())
}

fn check_fixtures(root: &Path) -> Result<(), String> {
    const CASES: &[&str] = &[
        "precedence",
        "malformed",
        "incomplete",
        "lossless",
        "roxygen-grouping",
        "roxygen-tags-fences",
        "roxygen-null",
    ];
    let fixture_root = "crates/r-conformance/fixtures";
    for case in CASES {
        let source = read(root.join(fixture_root).join(format!("{case}.r")))?;
        if source.is_empty() {
            return Err(format!("fixture {case}.r is empty"));
        }
    }
    let manifest = read(root.join(fixture_root).join("cases.tsv"))?;
    let rows: Vec<_> = manifest
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .collect();
    if rows.len() != CASES.len() {
        return Err("fixture manifest must contain exactly seven cases".into());
    }
    for (case, row) in CASES.iter().zip(rows) {
        let columns: Vec<_> = row.split('\t').collect();
        if columns.len() != 7 || columns[0] != *case {
            return Err(format!("malformed or out-of-order fixture row for {case}"));
        }
        if !matches!(columns[1], "Empty" | "Complete" | "Incomplete" | "Invalid") {
            return Err(format!("invalid parser status for fixture {case}"));
        }
        if columns[2..]
            .iter()
            .any(|value| value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(format!("fixture {case} has an invalid fingerprint"));
        }
    }
    Ok(())
}

fn read_inventory(
    root: &Path,
    name: &str,
    relative: &str,
    prefix: &str,
) -> Result<Vec<String>, String> {
    let text = read(root.join(relative))?;
    let start = format!("<!-- inventory:{name}:start -->");
    let end = format!("<!-- inventory:{name}:end -->");
    let section = text
        .split_once(&start)
        .and_then(|(_, rest)| rest.split_once(&end).map(|(body, _)| body))
        .ok_or_else(|| format!("missing or malformed {name} inventory markers in {relative}"))?;
    Ok(backtick_ids(section, prefix)
        .into_iter()
        .filter(|id| {
            section
                .lines()
                .any(|line| line.trim_start().starts_with(&format!("| `{id}`")))
        })
        .collect())
}

fn backtick_ids(text: &str, prefix: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut rest = text;
    while let Some((_, after_tick)) = rest.split_once('`') {
        let Some((value, after_value)) = after_tick.split_once('`') else {
            break;
        };
        if value.starts_with(prefix)
            && value
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            ids.push(value.to_owned());
        }
        rest = after_value;
    }
    ids
}

fn require_sequence(ids: &[String], prefix: &str, first: usize, last: usize) -> Result<(), String> {
    let width = if prefix.ends_with('G') { 3 } else { 2 };
    let expected: Vec<_> = (first..=last)
        .map(|number| format!("{prefix}{number:0width$}"))
        .collect();
    if ids == expected {
        Ok(())
    } else {
        Err(format!(
            "{prefix} inventory must be contiguous from {first} through {last}"
        ))
    }
}

fn read(path: PathBuf) -> Result<String, String> {
    fs::read_to_string(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_identifier_shaped_backticks() {
        assert_eq!(
            backtick_ids("`T_PIPE` `T-bad` `G001`", "T_"),
            vec!["T_PIPE"]
        );
    }

    #[test]
    fn sequence_requires_order_and_no_gaps() {
        assert!(require_sequence(&["O01".into(), "O02".into()], "O", 1, 2).is_ok());
        assert!(require_sequence(&["O02".into(), "O01".into()], "O", 1, 2).is_err());
    }
}
