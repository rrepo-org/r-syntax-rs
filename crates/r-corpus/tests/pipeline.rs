use std::{path::Path, time::Duration};

use r_corpus::{
    diff::DiffPolicy,
    manifest::export_manifest,
    model::{
        AcquisitionRecord, AcquisitionTerminalState, FinalManifest, Sha256Digest, SourceEncoding,
        SourceOccurrence,
    },
    store::Cas,
    task::{self, RunOptions, StoreLayout},
    worker::DEFAULT_PARSER_CONFIG,
};

#[test]
fn offline_manifest_worker_cache_and_diff_pipeline() {
    let directory = tempfile::tempdir().unwrap();
    let store_root = directory.path().join("store");
    let layout = StoreLayout::conventional(&store_root);
    let cas = Cas::open(&layout.objects).unwrap();
    let source = b"x <- function(a) a + 1\nx(2)\n";
    let source_digest = cas.put_bytes(source).unwrap();
    let archive_digest = Sha256Digest::parse("a".repeat(64)).unwrap();
    let mut manifest = FinalManifest::new(Sha256Digest::parse("b".repeat(64)).unwrap());
    manifest.acquisitions.push(AcquisitionRecord {
        package: "fixture".into(),
        version: "1.0".into(),
        archive_url: "https://example.test/fixture/source".into(),
        terminal: AcquisitionTerminalState::Collected {
            archive_sha256: archive_digest.clone(),
            source_count: 1,
            decoded_source_count: 1,
        },
    });
    manifest.sources.push(SourceOccurrence {
        package: "fixture".into(),
        version: "1.0".into(),
        archive_sha256: archive_digest,
        archive_path: "fixture/R/code.R".into(),
        declared_encoding: "UTF-8".into(),
        decoded_encoding: SourceEncoding::Utf8,
        raw_sha256: source_digest.clone(),
        decoded_sha256: source_digest,
        raw_bytes: source.len() as u64,
        decoded_bytes: source.len() as u64,
        curated: false,
    });

    let manifest_path = directory.path().join("manifest.ndjson.zst");
    export_manifest(&manifest, &manifest_path).unwrap();
    let worker = Path::new(env!("CARGO_BIN_EXE_r-parse-worker"));
    let baseline_records = directory.path().join("baseline-records.json.zst");
    let baseline_results = directory.path().join("baseline-results.json.zst");
    run(
        &manifest_path,
        &store_root,
        worker,
        "baseline",
        &baseline_records,
        &baseline_results,
        &directory.path().join("baseline-cache"),
    );
    let candidate_records = directory.path().join("candidate-records.json.zst");
    let candidate_results = directory.path().join("candidate-results.json.zst");
    run(
        &manifest_path,
        &store_root,
        worker,
        "candidate",
        &candidate_records,
        &candidate_results,
        &directory.path().join("candidate-cache"),
    );

    let diff_path = directory.path().join("diff.json.zst");
    let summary = directory.path().join("summary.md");
    let hard = task::diff(
        &baseline_results,
        &candidate_results,
        None,
        &DiffPolicy::default(),
        &diff_path,
        &summary,
    )
    .unwrap();
    assert!(!hard);
    assert!(diff_path.is_file());
    assert!(std::fs::read_to_string(summary)
        .unwrap()
        .contains("Cases compared: 1"));

    // A second baseline run must come entirely from the durable parser cache.
    let cached_records = directory.path().join("cached-records.json.zst");
    let cached_results = directory.path().join("cached-results.json.zst");
    run(
        &manifest_path,
        &store_root,
        worker,
        "baseline",
        &cached_records,
        &cached_results,
        &directory.path().join("baseline-cache"),
    );
    let cached: task::FullRunExport = task::read_json_zst(&cached_records).unwrap();
    assert_eq!(cached.records.len(), 1);
    assert!(cached.records[0].cached);
}

fn run(
    manifest: &Path,
    store: &Path,
    worker: &Path,
    implementation: &str,
    records: &Path,
    results: &Path,
    cache: &Path,
) {
    let (total, failures) = task::run(RunOptions {
        manifest,
        store,
        worker,
        implementation,
        parser_config: DEFAULT_PARSER_CONFIG,
        records_output: records,
        results_output: results,
        cache,
        shard: None,
        parallelism: 1,
        timeout: Duration::from_secs(10),
        max_rss_bytes: Some(256 * 1024 * 1024),
    })
    .unwrap();
    assert_eq!((total, failures), (1, 0));
}
