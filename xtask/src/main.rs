use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use r_corpus::{diff::Disposition, oracle::OracleConfig, task};

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
    let cli = Cli::parse();
    let root = option_env!("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or(
            env::current_dir()
                .map_err(|error| format!("cannot read current directory: {error}"))?,
        );
    match cli.command {
        Command::Inventory => inventory(&root),
        Command::Check => check(&root),
        Command::Corpus { command } => corpus(*command),
    }
}

#[derive(Parser)]
#[command(
    name = "cargo xtask",
    about = "Workspace maintenance and R corpus tooling"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print documented token and grammar inventories.
    Inventory,
    /// Validate specifications, syntax inventories, and fixtures.
    Check,
    /// Acquire and differentially test the external R package corpus.
    Corpus {
        #[command(subcommand)]
        command: Box<CorpusCommand>,
    },
}

#[derive(Subcommand)]
enum CorpusCommand {
    /// Fetch an rrepo inventory and write an immutable canonical snapshot.
    Snapshot(SnapshotArgs),
    /// Resume acquisition into a conventional durable store and finalize a manifest.
    Collect(CollectArgs),
    /// Run one external parser process per unique decoded source.
    Run(RunArgs),
    /// Compare normalized baseline and candidate exports.
    Diff(DiffArgs),
    /// Run reference R in a locked-down docker/podman container.
    Oracle(OracleArgs),
    /// Deterministically reduce a source while preserving a selected predicate.
    Minimize(MinimizeArgs),
    /// Run baseline and candidate workers on one decoded UTF-8 source.
    Replay(ReplayArgs),
    /// Exercise the parser and invariants directly without network or containers.
    SelfTest,
}

#[derive(Args)]
struct SnapshotArgs {
    /// rrepo API base URL exposing /packages and /packages/NAME/versions.
    #[arg(long)]
    repository: String,
    /// Destination canonical inventory JSON (never overwrites different content).
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 8)]
    concurrency: usize,
    #[arg(long, default_value_t = 60)]
    timeout_seconds: u64,
}

#[derive(Args)]
struct CollectArgs {
    /// Canonical inventory JSON produced by snapshot.
    #[arg(long)]
    inventory: PathBuf,
    /// Store root containing index.sqlite3, objects/, and tmp/.
    #[arg(long)]
    store: PathBuf,
    /// Final canonical .ndjson.zst manifest.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Args)]
struct RunArgs {
    #[arg(long)]
    manifest: PathBuf,
    /// Collection store root containing decoded CAS objects.
    #[arg(long)]
    store: PathBuf,
    /// Path to the parser worker executable (normally target/.../r-parse-worker).
    #[arg(long)]
    worker: PathBuf,
    /// Stable implementation/revision label embedded in outputs and cache keys.
    #[arg(long)]
    implementation: String,
    /// Compressed full supervision and worker records.
    #[arg(long)]
    records: PathBuf,
    /// Compressed normalized WorkerResultSet consumed by corpus diff.
    #[arg(long)]
    results: PathBuf,
    /// Durable worker-response cache directory.
    #[arg(long)]
    cache: PathBuf,
    /// Zero-based deterministic source shard N/M, for example 0/8.
    #[arg(long, value_parser = parse_shard)]
    shard: Option<(u32, u32)>,
    #[arg(long, default_value_t = 1)]
    jobs: usize,
    #[arg(long, default_value_t = 30)]
    timeout_seconds: u64,
    /// Per-worker RSS bound in bytes; omit to disable the RSS bound.
    #[arg(long)]
    max_rss_bytes: Option<u64>,
}

#[derive(Args)]
struct DiffArgs {
    #[arg(long)]
    baseline: PathBuf,
    #[arg(long)]
    candidate: PathBuf,
    /// Optional compressed OracleRun produced by corpus oracle.
    #[arg(long)]
    oracle: Option<PathBuf>,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    summary: PathBuf,
    #[arg(long, default_value_t = 2.0)]
    performance_ratio: f64,
    #[arg(long, default_value_t = 5_000_000)]
    performance_min_delta_ns: u64,
    #[arg(long, default_value_t = 1_048_576)]
    memory_min_delta_bytes: u64,
    /// Treat thresholded performance findings as hard failures.
    #[arg(long)]
    hard_performance: bool,
    /// Directory for standalone cluster replay bundles.
    #[arg(long, requires_all = ["baseline_records", "candidate_records", "manifest", "store"])]
    bundles: Option<PathBuf>,
    #[arg(long)]
    baseline_records: Option<PathBuf>,
    #[arg(long)]
    candidate_records: Option<PathBuf>,
    #[arg(long)]
    manifest: Option<PathBuf>,
    #[arg(long)]
    store: Option<PathBuf>,
}

#[derive(Args)]
struct OracleArgs {
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    store: PathBuf,
    #[arg(long)]
    output: PathBuf,
    /// Exactly docker or podman.
    #[arg(long, default_value = "docker")]
    runtime: String,
    /// OCI image with a full @sha256:... digest.
    #[arg(long)]
    image: String,
    #[arg(long, default_value = "4.6.1")]
    r_version: String,
    #[arg(long, default_value = "C.UTF-8")]
    locale: String,
    #[arg(long, default_value = "linux/amd64")]
    platform: String,
    #[arg(long, default_value = "512m")]
    memory: String,
    #[arg(long, default_value = "1")]
    cpus: String,
    #[arg(long, default_value_t = 64)]
    pids_limit: u32,
    #[arg(long, default_value_t = 3600)]
    timeout_seconds: u64,
    #[arg(long, default_value_t = 67_108_864)]
    max_output_bytes: usize,
}

#[derive(Clone, Copy, ValueEnum)]
enum StatusArg {
    Accepted,
    Rejected,
    Crash,
}

#[derive(Args)]
struct MinimizeArgs {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    worker: PathBuf,
    #[arg(long)]
    implementation: String,
    /// Preserve this single-worker parser status.
    #[arg(long, value_enum, conflicts_with = "finding_code")]
    status: Option<StatusArg>,
    /// Preserve this exact baseline/candidate differential finding code.
    #[arg(long, conflicts_with = "status")]
    finding_code: Option<String>,
    #[arg(long, requires = "finding_code")]
    baseline_worker: Option<PathBuf>,
    #[arg(long, requires = "finding_code")]
    baseline: Option<String>,
    #[arg(long, default_value_t = 30)]
    timeout_seconds: u64,
    #[arg(long)]
    max_rss_bytes: Option<u64>,
}

#[derive(Args)]
struct ReplayArgs {
    #[arg(long)]
    source: PathBuf,
    #[arg(long)]
    baseline_worker: PathBuf,
    #[arg(long)]
    candidate_worker: PathBuf,
    #[arg(long)]
    baseline: String,
    #[arg(long)]
    candidate: String,
    #[arg(long, default_value_t = 30)]
    timeout_seconds: u64,
    #[arg(long)]
    max_rss_bytes: Option<u64>,
}

fn corpus(command: CorpusCommand) -> Result<(), String> {
    match command {
        CorpusCommand::Snapshot(args) => {
            let (snapshot, digest) = task::snapshot(
                &args.repository,
                &args.output,
                args.concurrency,
                Duration::from_secs(args.timeout_seconds),
            )
            .map_err(task_error)?;
            let versions = snapshot
                .packages
                .iter()
                .map(|package| package.versions.len())
                .sum::<usize>();
            println!(
                "snapshot: {} packages, {versions} acquisitions, sha256 {digest}",
                snapshot.packages.len()
            );
            Ok(())
        }
        CorpusCommand::Collect(args) => {
            let result =
                task::collect(&args.inventory, &args.store, &args.output).map_err(task_error)?;
            println!(
                "collect: inventory={} collected={} rejected={} failed={} incomplete={} decoded_sources={} decode_failures={}",
                result.inventory_records,
                result.collected,
                result.rejected,
                result.failed,
                result.incomplete,
                result.sources,
                result.decode_failures
            );
            println!(
                "  utf8={} latin1={} unique_raw={} unique_decoded={} occurrence_raw_bytes={} occurrence_decoded_bytes={} source_cas_bytes={} deduplicated_bytes_saved={}",
                result.utf8_sources,
                result.latin1_sources,
                result.unique_raw_sources,
                result.unique_decoded_sources,
                result.occurrence_raw_bytes,
                result.occurrence_decoded_bytes,
                result.unique_source_cas_bytes,
                result.deduplicated_bytes_saved
            );
            for (class, count) in result.failures_by_class {
                println!("  failure.{class}={count}");
            }
            Ok(())
        }
        CorpusCommand::Run(args) => {
            let (total, failures) = task::run(task::RunOptions {
                manifest: &args.manifest,
                store: &args.store,
                worker: &args.worker,
                implementation: &args.implementation,
                parser_config: r_corpus::worker::DEFAULT_PARSER_CONFIG,
                records_output: &args.records,
                results_output: &args.results,
                cache: &args.cache,
                shard: args.shard,
                parallelism: args.jobs,
                timeout: Duration::from_secs(args.timeout_seconds),
                max_rss_bytes: args.max_rss_bytes,
            })
            .map_err(task_error)?;
            println!("run: {total} unique decoded sources, {failures} supervised failures");
            Ok(())
        }
        CorpusCommand::Diff(args) => {
            let policy = r_corpus::diff::DiffPolicy {
                performance_ratio: args.performance_ratio,
                performance_min_delta_ns: args.performance_min_delta_ns,
                memory_min_delta_bytes: args.memory_min_delta_bytes,
                performance_disposition: if args.hard_performance {
                    Disposition::HardFailure
                } else {
                    Disposition::Review
                },
            };
            let hard = task::diff(
                &args.baseline,
                &args.candidate,
                args.oracle.as_deref(),
                &policy,
                &args.output,
                &args.summary,
            )
            .map_err(task_error)?;
            if let Some(output) = args.bundles.as_deref() {
                let count = task::emit_bundles(task::BundleOptions {
                    diff: &args.output,
                    baseline_records: args.baseline_records.as_deref().expect("required by clap"),
                    candidate_records: args.candidate_records.as_deref().expect("required by clap"),
                    oracle: args.oracle.as_deref(),
                    manifest: args.manifest.as_deref().expect("required by clap"),
                    store: args.store.as_deref().expect("required by clap"),
                    output,
                })
                .map_err(task_error)?;
                println!("diff: wrote {count} replay bundles");
            }
            if hard {
                Err("corpus diff contains hard failures (outputs were written)".into())
            } else {
                println!("diff: outputs written; no hard failures");
                Ok(())
            }
        }
        CorpusCommand::Oracle(args) => {
            let (total, infrastructure) = task::oracle(task::OracleOptions {
                manifest: &args.manifest,
                store: &args.store,
                output: &args.output,
                runtime: &args.runtime,
                config: OracleConfig {
                    image: args.image,
                    r_version: args.r_version,
                    locale: args.locale,
                    platform: args.platform,
                    memory: args.memory,
                    cpus: args.cpus,
                    pids_limit: args.pids_limit,
                    timeout_seconds: args.timeout_seconds,
                    max_output_bytes: args.max_output_bytes,
                },
            })
            .map_err(task_error)?;
            println!(
                "oracle: {total} unique decoded sources, {infrastructure} infrastructure results"
            );
            Ok(())
        }
        CorpusCommand::Minimize(args) => {
            let predicate = match (args.status, args.finding_code.as_deref()) {
                (Some(status), None) => task::MinimizePredicate::Status(match status {
                    StatusArg::Accepted => task::RequestedStatus::Accepted,
                    StatusArg::Rejected => task::RequestedStatus::Rejected,
                    StatusArg::Crash => task::RequestedStatus::Crash,
                }),
                (None, Some(code)) => task::MinimizePredicate::Finding {
                    baseline_worker: args.baseline_worker.as_deref().ok_or_else(|| {
                        "--baseline-worker is required with --finding-code".to_owned()
                    })?,
                    baseline_implementation: args
                        .baseline
                        .as_deref()
                        .ok_or_else(|| "--baseline is required with --finding-code".to_owned())?,
                    code,
                },
                _ => return Err("specify exactly one of --status or --finding-code".into()),
            };
            let result = task::minimize(
                &args.input,
                &args.output,
                &args.worker,
                &args.implementation,
                predicate,
                Duration::from_secs(args.timeout_seconds),
                args.max_rss_bytes,
            )
            .map_err(task_error)?;
            println!(
                "minimize: {} -> {} bytes, {} predicate calls, sha256 {}",
                result.original_bytes,
                result.minimized_bytes,
                result.predicate_calls,
                result.sha256
            );
            Ok(())
        }
        CorpusCommand::Replay(args) => {
            let value = task::replay(
                &args.source,
                &args.baseline_worker,
                &args.candidate_worker,
                &args.baseline,
                &args.candidate,
                Duration::from_secs(args.timeout_seconds),
                args.max_rss_bytes,
            )
            .map_err(task_error)?;
            println!(
                "{}",
                serde_json::to_string(&value).map_err(|error| error.to_string())?
            );
            Ok(())
        }
        CorpusCommand::SelfTest => {
            let response = task::self_test().map_err(task_error)?;
            println!(
                "{}",
                serde_json::to_string(&response).map_err(|error| error.to_string())?
            );
            Ok(())
        }
    }
}

fn parse_shard(value: &str) -> Result<(u32, u32), String> {
    let (index, count) = value
        .split_once('/')
        .ok_or_else(|| "shard must have N/M form".to_owned())?;
    let index = index
        .parse::<u32>()
        .map_err(|error| format!("invalid shard index: {error}"))?;
    let count = count
        .parse::<u32>()
        .map_err(|error| format!("invalid shard count: {error}"))?;
    if count == 0 || index >= count {
        return Err("shard index must be zero-based and less than a positive count".into());
    }
    Ok((index, count))
}

fn task_error(error: Box<dyn std::error::Error + Send + Sync>) -> String {
    error.to_string()
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
        "docs/corpus.md",
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
