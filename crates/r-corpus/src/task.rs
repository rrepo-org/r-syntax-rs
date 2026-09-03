//! Command-oriented orchestration for the corpus workflow.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs::{self, File},
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
    time::Duration,
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::{NamedTempFile, TempDir};

use crate::{
    collect::{CollectionLimits, Collector, RRepoClient, RRepoConfig},
    diff::{self, DiffPolicy, RunAccounting, WorkerObservation, WorkerResultSet},
    manifest::{
        export_manifest, import_manifest, read_inventory_snapshot, write_inventory_snapshot,
    },
    minimize::{self, MinimizeConfig, PredicateCache},
    model::{AcquisitionTerminalState, FinalManifest, InventorySnapshot},
    oracle::{OracleConfig, OracleRun, OracleRunner},
    report,
    run::{self, RunFailure, SupervisorConfig, WorkerCache},
    store::{stable_shard, Cas, CorpusStore},
    worker::{
        self, ParserIdentity, ParserStatus, SourceIdentity, WorkerRequest, WorkerResponse,
        DEFAULT_PARSER_CONFIG,
    },
};

pub type TaskResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Clone, Debug)]
pub struct StoreLayout {
    pub root: PathBuf,
    pub database: PathBuf,
    pub objects: PathBuf,
    pub temporary: PathBuf,
}

impl StoreLayout {
    pub fn conventional(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            database: root.join("index.sqlite3"),
            objects: root.join("objects"),
            temporary: root.join("tmp"),
            root,
        }
    }
}

pub fn snapshot(
    repository: &str,
    output: &Path,
    concurrency: usize,
    timeout: Duration,
) -> TaskResult<(InventorySnapshot, String)> {
    let mut config = RRepoConfig::from_env(repository);
    config.concurrency = concurrency;
    config.timeout = timeout;
    let inventory = RRepoClient::new(config)?.fetch_inventory()?;
    let digest = write_inventory_snapshot(&inventory, output)?;
    Ok((inventory, digest.to_string()))
}

#[derive(Clone, Debug, Default)]
pub struct CollectionAccounting {
    pub inventory_records: usize,
    pub collected: usize,
    pub rejected: usize,
    pub failed: usize,
    pub incomplete: usize,
    pub sources: usize,
    pub decode_failures: usize,
    pub utf8_sources: usize,
    pub latin1_sources: usize,
    pub unique_raw_sources: usize,
    pub unique_decoded_sources: usize,
    pub occurrence_raw_bytes: u64,
    pub occurrence_decoded_bytes: u64,
    pub unique_source_cas_bytes: u64,
    pub deduplicated_bytes_saved: u64,
    pub failures_by_class: BTreeMap<String, usize>,
}

pub fn collect(
    inventory_path: &Path,
    store_root: &Path,
    output: &Path,
) -> TaskResult<CollectionAccounting> {
    let (inventory, inventory_digest) = read_inventory_snapshot(inventory_path)?;
    let layout = StoreLayout::conventional(store_root);
    fs::create_dir_all(&layout.temporary)?;
    let client = RRepoClient::new(RRepoConfig::from_env(&inventory.repository))?;
    let collector = Collector::new(client, &layout.temporary, CollectionLimits::default())?;
    let mut store = CorpusStore::open(&layout.database, &layout.objects)?;
    let manifest = collector.collect_snapshot(&inventory, inventory_digest, &mut store)?;
    let accounting = validate_collection(&inventory, &manifest)?;
    export_manifest(&manifest, output)?;
    Ok(accounting)
}

fn validate_collection(
    inventory: &InventorySnapshot,
    manifest: &FinalManifest,
) -> TaskResult<CollectionAccounting> {
    let expected = inventory
        .packages
        .iter()
        .flat_map(|package| {
            package.versions.iter().map(move |version| {
                (
                    (package.name.clone(), version.version.clone()),
                    version.archive_url.as_str(),
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    let mut observed = BTreeSet::new();
    let mut accounting = CollectionAccounting {
        inventory_records: expected.len(),
        sources: manifest.sources.len(),
        decode_failures: manifest.source_failures.len(),
        ..CollectionAccounting::default()
    };
    let mut raw_objects = BTreeMap::new();
    let mut decoded_objects = BTreeMap::new();
    let mut all_objects = BTreeMap::new();
    for source in &manifest.sources {
        accounting.occurrence_raw_bytes += source.raw_bytes;
        accounting.occurrence_decoded_bytes += source.decoded_bytes;
        match source.decoded_encoding {
            crate::model::SourceEncoding::Utf8 => accounting.utf8_sources += 1,
            crate::model::SourceEncoding::Latin1 => accounting.latin1_sources += 1,
        }
        raw_objects
            .entry(source.raw_sha256.as_str())
            .or_insert(source.raw_bytes);
        decoded_objects
            .entry(source.decoded_sha256.as_str())
            .or_insert(source.decoded_bytes);
        all_objects
            .entry(source.raw_sha256.as_str())
            .or_insert(source.raw_bytes);
        all_objects
            .entry(source.decoded_sha256.as_str())
            .or_insert(source.decoded_bytes);
    }
    for source in &manifest.source_failures {
        accounting.occurrence_raw_bytes += source.raw_bytes;
        raw_objects
            .entry(source.raw_sha256.as_str())
            .or_insert(source.raw_bytes);
        all_objects
            .entry(source.raw_sha256.as_str())
            .or_insert(source.raw_bytes);
    }
    accounting.unique_raw_sources = raw_objects.len();
    accounting.unique_decoded_sources = decoded_objects.len();
    accounting.unique_source_cas_bytes = all_objects.values().sum();
    accounting.deduplicated_bytes_saved = accounting
        .occurrence_raw_bytes
        .saturating_add(accounting.occurrence_decoded_bytes)
        .saturating_sub(accounting.unique_source_cas_bytes);
    for record in &manifest.acquisitions {
        let key = (record.package.clone(), record.version.clone());
        if !observed.insert(key.clone()) {
            return Err(format!("duplicate manifest acquisition {} {}", key.0, key.1).into());
        }
        let expected_url = expected.get(&key).ok_or_else(|| {
            format!(
                "manifest contains acquisition absent from inventory: {} {}",
                key.0, key.1
            )
        })?;
        if *expected_url != record.archive_url {
            return Err(format!("archive URL changed for {} {}", key.0, key.1).into());
        }
        match &record.terminal {
            AcquisitionTerminalState::Collected {
                source_count,
                decoded_source_count,
                ..
            } => {
                accounting.collected += 1;
                let decoded = manifest
                    .sources
                    .iter()
                    .filter(|source| source.package == key.0 && source.version == key.1)
                    .count() as u64;
                let decode_failures = manifest
                    .source_failures
                    .iter()
                    .filter(|source| source.package == key.0 && source.version == key.1)
                    .count() as u64;
                if decoded != *decoded_source_count || decoded + decode_failures != *source_count {
                    return Err(format!(
                        "manifest source records disappeared for {} {}: expected {} selected/{} decoded, found {}/{}",
                        key.0, key.1, source_count, decoded_source_count, decoded + decode_failures, decoded
                    )
                    .into());
                }
            }
            AcquisitionTerminalState::Rejected { failure, .. } => {
                accounting.rejected += 1;
                *accounting
                    .failures_by_class
                    .entry(format!("{:?}", failure.class).to_ascii_lowercase())
                    .or_default() += 1;
            }
            AcquisitionTerminalState::Failed { failure, .. } => {
                accounting.failed += 1;
                *accounting
                    .failures_by_class
                    .entry(format!("{:?}", failure.class).to_ascii_lowercase())
                    .or_default() += 1;
            }
        }
    }
    accounting.incomplete = expected.len().saturating_sub(observed.len());
    if accounting.incomplete != 0 || observed.len() != expected.len() {
        let missing = expected
            .keys()
            .filter(|key| !observed.contains(*key))
            .map(|(package, version)| format!("{package} {version}"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "manifest lost {} inventory acquisition record(s): {missing}",
            accounting.incomplete
        )
        .into());
    }
    Ok(accounting)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FullRunExport {
    pub implementation: String,
    pub manifest_sha256: String,
    pub shard: Option<String>,
    pub expected_cases: Vec<String>,
    pub accounting: RunAccounting,
    pub records: Vec<FullRunRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FullRunRecord {
    pub request: WorkerRequest,
    pub elapsed_ns: u64,
    pub peak_rss_bytes: Option<u64>,
    pub cached: bool,
    #[serde(flatten)]
    pub outcome: FullRunOutcome,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FullRunOutcome {
    Response { response: Box<WorkerResponse> },
    Failure { failure: SerializableRunFailure },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SerializableRunFailure {
    pub kind: String,
    pub message: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stderr: String,
}

impl From<&RunFailure> for SerializableRunFailure {
    fn from(value: &RunFailure) -> Self {
        Self {
            kind: format!("{:?}", value.kind).to_ascii_lowercase(),
            message: value.message.clone(),
            exit_code: value.exit_code,
            signal: value.signal,
            stderr: value.stderr.clone(),
        }
    }
}

pub struct RunOptions<'a> {
    pub manifest: &'a Path,
    pub store: &'a Path,
    pub worker: &'a Path,
    pub implementation: &'a str,
    pub parser_config: &'a str,
    pub records_output: &'a Path,
    pub results_output: &'a Path,
    pub cache: &'a Path,
    pub shard: Option<(u32, u32)>,
    pub parallelism: usize,
    pub timeout: Duration,
    pub max_rss_bytes: Option<u64>,
}

pub fn run(options: RunOptions<'_>) -> TaskResult<(usize, usize)> {
    let (manifest, hashes) = import_manifest(options.manifest, None)?;
    validate_manifest_shape(&manifest)?;
    let cas = Cas::open(StoreLayout::conventional(options.store).objects)?;
    let mut representatives = BTreeMap::new();
    for source in &manifest.sources {
        representatives
            .entry(source.decoded_sha256.clone())
            .and_modify(|archive: &mut crate::model::Sha256Digest| {
                if source.archive_sha256 < *archive {
                    *archive = source.archive_sha256.clone();
                }
            })
            .or_insert_with(|| source.archive_sha256.clone());
    }
    let curated = manifest
        .sources
        .iter()
        .filter(|source| source.curated)
        .map(|source| source.decoded_sha256.as_str())
        .collect::<BTreeSet<_>>();
    if let Some((index, count)) = options.shard {
        if index >= count || count == 0 {
            return Err(
                format!("shard {index}/{count} is invalid (indices are zero-based)").into(),
            );
        }
        representatives.retain(|_, archive| {
            matches!(stable_shard(archive.as_str().as_bytes(), count), Ok(shard) if shard == index)
        });
    }
    let requests = representatives
        .keys()
        .map(|digest| {
            cas.verify(digest)?;
            Ok(WorkerRequest::new(
                SourceIdentity {
                    path: cas.path(digest),
                    sha256: digest.to_string(),
                },
                ParserIdentity::new(options.implementation, options.parser_config),
            ))
        })
        .collect::<crate::Result<Vec<_>>>()?;
    let mut config = SupervisorConfig::new(options.worker);
    config.parallelism = options.parallelism.max(1);
    config.wall_timeout = options.timeout;
    config.max_rss_bytes = options.max_rss_bytes;
    let cache = FileWorkerCache::open(options.cache)?;
    let outcomes = run::run_sources(&config, requests, &cache);
    let cache_errors = cache.errors();
    let expected_cases = representatives
        .keys()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let accounting = RunAccounting {
        expected: expected_cases.len(),
        responses: outcomes
            .iter()
            .filter(|outcome| outcome.response.is_ok())
            .count(),
        supervisor_failures: outcomes
            .iter()
            .filter(|outcome| outcome.response.is_err())
            .count(),
        cache_hits: outcomes.iter().filter(|outcome| outcome.cached).count(),
    };
    let shard = options
        .shard
        .map(|(index, count)| format!("{index}/{count}"));
    let normalized = WorkerResultSet {
        implementation: options.implementation.to_owned(),
        manifest_sha256: hashes.content_sha256.to_string(),
        shard: shard.clone(),
        expected_cases: expected_cases.clone(),
        accounting: accounting.clone(),
        results: outcomes
            .iter()
            .map(|outcome| {
                WorkerObservation::from_run_outcome(
                    outcome,
                    curated.contains(outcome.request.decoded_source.sha256.as_str()),
                )
            })
            .collect(),
    };
    let failures = outcomes
        .iter()
        .filter(|outcome| outcome.response.is_err())
        .count();
    let full = FullRunExport {
        implementation: options.implementation.to_owned(),
        manifest_sha256: hashes.content_sha256.to_string(),
        shard,
        expected_cases,
        accounting,
        records: outcomes.iter().map(full_record).collect(),
    };
    write_json_zst(options.records_output, &full)?;
    write_json_zst(options.results_output, &normalized)?;
    if !cache_errors.is_empty() {
        return Err(format!("worker cache I/O failed: {}", cache_errors.join("; ")).into());
    }
    Ok((outcomes.len(), failures))
}

fn validate_manifest_shape(manifest: &FinalManifest) -> TaskResult<()> {
    let mut acquisitions = BTreeMap::new();
    for acquisition in &manifest.acquisitions {
        if acquisitions
            .insert(
                (&acquisition.package, &acquisition.version),
                &acquisition.terminal,
            )
            .is_some()
        {
            return Err(format!(
                "duplicate acquisition in manifest: {} {}",
                acquisition.package, acquisition.version
            )
            .into());
        }
    }
    let mut occurrences = BTreeSet::new();
    for source in &manifest.sources {
        let key = (&source.package, &source.version);
        let Some(terminal) = acquisitions.get(&key) else {
            return Err(format!(
                "source occurrence has no acquisition: {} {} {}",
                source.package, source.version, source.archive_path
            )
            .into());
        };
        if !matches!(terminal, AcquisitionTerminalState::Collected { .. }) {
            return Err(format!(
                "non-collected acquisition has source occurrence: {} {}",
                source.package, source.version
            )
            .into());
        }
        if !occurrences.insert((key, &source.archive_path)) {
            return Err(format!(
                "duplicate source occurrence: {} {} {}",
                source.package, source.version, source.archive_path
            )
            .into());
        }
    }
    for source in &manifest.source_failures {
        let key = (&source.package, &source.version);
        let Some(terminal) = acquisitions.get(&key) else {
            return Err(format!(
                "source failure has no acquisition: {} {} {}",
                source.package, source.version, source.archive_path
            )
            .into());
        };
        if !matches!(terminal, AcquisitionTerminalState::Collected { .. }) {
            return Err(format!(
                "non-collected acquisition has source failure: {} {}",
                source.package, source.version
            )
            .into());
        }
        if !occurrences.insert((key, &source.archive_path)) {
            return Err(format!(
                "duplicate selected source: {} {} {}",
                source.package, source.version, source.archive_path
            )
            .into());
        }
    }
    for ((package, version), terminal) in acquisitions {
        if let AcquisitionTerminalState::Collected {
            source_count,
            decoded_source_count,
            ..
        } = terminal
        {
            let decoded = manifest
                .sources
                .iter()
                .filter(|source| &source.package == package && &source.version == version)
                .count() as u64;
            let failed = manifest
                .source_failures
                .iter()
                .filter(|source| &source.package == package && &source.version == version)
                .count() as u64;
            if decoded != *decoded_source_count || decoded + failed != *source_count {
                return Err(format!(
                    "manifest source records disappeared for {package} {version}: expected {source_count} selected/{decoded_source_count} decoded, found {}/{}",
                    decoded + failed,
                    decoded
                )
                .into());
            }
        }
    }
    Ok(())
}

fn full_record(outcome: &run::RunOutcome) -> FullRunRecord {
    FullRunRecord {
        request: outcome.request.clone(),
        elapsed_ns: u64::try_from(outcome.elapsed.as_nanos()).unwrap_or(u64::MAX),
        peak_rss_bytes: outcome.peak_rss_bytes,
        cached: outcome.cached,
        outcome: match &outcome.response {
            Ok(response) => FullRunOutcome::Response {
                response: Box::new(response.clone()),
            },
            Err(failure) => FullRunOutcome::Failure {
                failure: failure.into(),
            },
        },
    }
}

struct FileWorkerCache {
    root: PathBuf,
    errors: Mutex<Vec<String>>,
}

impl FileWorkerCache {
    fn open(root: &Path) -> TaskResult<Self> {
        fs::create_dir_all(root)?;
        Ok(Self {
            root: root.to_owned(),
            errors: Mutex::new(Vec::new()),
        })
    }

    fn path(&self, request: &WorkerRequest) -> PathBuf {
        let bytes = serde_json::to_vec(request).expect("worker requests serialize");
        self.root.join(format!("{:x}.json", Sha256::digest(bytes)))
    }

    fn record_error(&self, error: impl ToString) {
        self.errors
            .lock()
            .expect("cache error lock poisoned")
            .push(error.to_string());
    }

    fn errors(&self) -> Vec<String> {
        self.errors
            .lock()
            .expect("cache error lock poisoned")
            .clone()
    }
}

impl WorkerCache for FileWorkerCache {
    fn load(&self, request: &WorkerRequest) -> Option<WorkerResponse> {
        let path = self.path(request);
        if !path.exists() {
            return None;
        }
        match File::open(&path).and_then(|file| {
            serde_json::from_reader(BufReader::new(file)).map_err(std::io::Error::other)
        }) {
            Ok(response) => Some(response),
            Err(error) => {
                self.record_error(format!("{}: {error}", path.display()));
                None
            }
        }
    }

    fn store(&self, request: &WorkerRequest, response: &WorkerResponse) {
        let path = self.path(request);
        let result = (|| -> Result<(), std::io::Error> {
            let mut temporary = NamedTempFile::new_in(&self.root)?;
            serde_json::to_writer(&mut temporary, response).map_err(std::io::Error::other)?;
            temporary.write_all(b"\n")?;
            temporary.as_file_mut().sync_all()?;
            match temporary.persist_noclobber(&path) {
                Ok(file) => {
                    file.sync_all()?;
                    File::open(&self.root)?.sync_all()
                }
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                Err(error) => Err(error.error),
            }
        })();
        if let Err(error) = result {
            self.record_error(format!("{}: {error}", path.display()));
        }
    }
}

pub fn diff(
    baseline_path: &Path,
    candidate_path: &Path,
    oracle_path: Option<&Path>,
    policy: &DiffPolicy,
    json_output: &Path,
    markdown_output: &Path,
) -> TaskResult<bool> {
    let baseline: WorkerResultSet = read_json_zst(baseline_path)?;
    let candidate: WorkerResultSet = read_json_zst(candidate_path)?;
    let oracle = oracle_path
        .map(read_json_zst::<OracleRun>)
        .transpose()?
        .map_or_else(Vec::new, |run| run.records);
    let result = diff::compare(&baseline, &candidate, &oracle, policy);
    write_json_zst(json_output, &result)?;
    ensure_parent(markdown_output)?;
    report::write_markdown(markdown_output, &result)?;
    Ok(result.has_hard_failures())
}

pub struct BundleOptions<'a> {
    pub diff: &'a Path,
    pub baseline_records: &'a Path,
    pub candidate_records: &'a Path,
    pub oracle: Option<&'a Path>,
    pub manifest: &'a Path,
    pub store: &'a Path,
    pub output: &'a Path,
}

pub fn emit_bundles(options: BundleOptions<'_>) -> TaskResult<usize> {
    let diff: crate::diff::DiffReport = read_json_zst(options.diff)?;
    let baseline: FullRunExport = read_json_zst(options.baseline_records)?;
    let candidate: FullRunExport = read_json_zst(options.candidate_records)?;
    let oracle = options
        .oracle
        .map(read_json_zst::<OracleRun>)
        .transpose()?
        .map_or_else(Vec::new, |run| run.records);
    let (manifest, _) = import_manifest(options.manifest, None)?;
    validate_manifest_shape(&manifest)?;
    let cas = Cas::open(StoreLayout::conventional(options.store).objects)?;
    let baseline = full_records_by_source(&baseline);
    let candidate = full_records_by_source(&candidate);
    let oracle = oracle
        .iter()
        .map(|record| (record.sha256.as_str(), record))
        .collect::<BTreeMap<_, _>>();
    fs::create_dir_all(options.output)?;
    let mut written = 0;
    for cluster in &diff.clusters {
        let Some(finding) = diff
            .findings
            .iter()
            .filter(|finding| finding.signature == cluster.signature)
            .filter_map(|finding| {
                let digest = finding.source_sha256.as_deref()?;
                let bytes = manifest
                    .sources
                    .iter()
                    .filter(|source| source.decoded_sha256.as_str() == digest)
                    .map(|source| source.decoded_bytes)
                    .min()?;
                Some((bytes, digest, finding))
            })
            .min_by(|left, right| (left.0, left.1).cmp(&(right.0, right.1)))
        else {
            continue;
        };
        let digest = crate::model::Sha256Digest::parse(finding.1.to_owned())?;
        let source = fs::read_to_string(cas.path(&digest))?;
        let baseline_value = baseline
            .get(finding.1)
            .map(serde_json::to_value)
            .transpose()?
            .unwrap_or(serde_json::Value::Null);
        let candidate_value = candidate
            .get(finding.1)
            .map(serde_json::to_value)
            .transpose()?
            .unwrap_or(serde_json::Value::Null);
        let occurrences = manifest
            .sources
            .iter()
            .filter(|occurrence| occurrence.decoded_sha256 == digest)
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?;
        let case = report::BundleCase {
            id: cluster.signature.clone(),
            source_sha256: finding.1.to_owned(),
            source,
            finding: finding.2.clone(),
        };
        report::write_bundle(
            &options.output.join(&cluster.signature),
            &report::Bundle {
                case: &case,
                baseline: &baseline_value,
                candidate: &candidate_value,
                oracle: oracle.get(finding.1).copied(),
                occurrences: &occurrences,
            },
        )?;
        written += 1;
    }
    Ok(written)
}

fn full_records_by_source(export: &FullRunExport) -> BTreeMap<&str, &FullRunRecord> {
    export
        .records
        .iter()
        .map(|record| (record.request.decoded_source.sha256.as_str(), record))
        .collect()
}

pub struct OracleOptions<'a> {
    pub manifest: &'a Path,
    pub store: &'a Path,
    pub output: &'a Path,
    pub runtime: &'a str,
    pub config: OracleConfig,
}

pub fn oracle(options: OracleOptions<'_>) -> TaskResult<(usize, usize)> {
    if !matches!(options.runtime, "docker" | "podman") {
        return Err("container runtime must be docker or podman; host R is forbidden".into());
    }
    let (manifest, _) = import_manifest(options.manifest, None)?;
    validate_manifest_shape(&manifest)?;
    let cas = Cas::open(StoreLayout::conventional(options.store).objects)?;
    let staging = TempDir::new()?;
    let digests = manifest
        .sources
        .iter()
        .map(|source| source.decoded_sha256.clone())
        .collect::<BTreeSet<_>>();
    let runner = OracleRunner::new(options.runtime, options.config)?;
    if options.output.exists() {
        let cached: OracleRun = read_json_zst(options.output)?;
        let cached_digests = cached
            .records
            .iter()
            .map(|record| record.sha256.as_str())
            .collect::<BTreeSet<_>>();
        let expected_digests = digests
            .iter()
            .map(crate::model::Sha256Digest::as_str)
            .collect::<BTreeSet<_>>();
        let cache_is_complete = cached.records.iter().all(|record| {
            !matches!(
                record.outcome,
                crate::oracle::OracleOutcome::Infrastructure { .. }
            )
        });
        if cache_is_complete
            && cached.provenance == runner.provenance()
            && cached_digests == expected_digests
        {
            let infrastructure = cached
                .records
                .iter()
                .filter(|record| {
                    matches!(
                        record.outcome,
                        crate::oracle::OracleOutcome::Infrastructure { .. }
                    )
                })
                .count();
            return Ok((cached.records.len(), infrastructure));
        }
    }
    let status = Command::new(options.runtime).arg("--version").status()?;
    if !status.success() {
        return Err(format!("{} --version failed with {status}", options.runtime).into());
    }
    for digest in &digests {
        cas.verify(digest)?;
        fs::copy(cas.path(digest), staging.path().join(digest.as_str()))?;
    }
    let result = runner.run(staging.path())?;
    let infrastructure = result
        .records
        .iter()
        .filter(|record| {
            matches!(
                record.outcome,
                crate::oracle::OracleOutcome::Infrastructure { .. }
            )
        })
        .count();
    write_json_zst(options.output, &result)?;
    Ok((result.records.len(), infrastructure))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestedStatus {
    Accepted,
    Rejected,
    Crash,
}

#[derive(Clone, Copy)]
pub enum MinimizePredicate<'a> {
    Status(RequestedStatus),
    Finding {
        baseline_worker: &'a Path,
        baseline_implementation: &'a str,
        code: &'a str,
    },
}

pub fn minimize(
    input: &Path,
    output: &Path,
    worker_path: &Path,
    implementation: &str,
    predicate: MinimizePredicate<'_>,
    timeout: Duration,
    max_rss_bytes: Option<u64>,
) -> TaskResult<minimize::MinimizationResult> {
    let source = fs::read_to_string(input)?;
    let directory = TempDir::new()?;
    let candidate_path = directory.path().join("candidate.R");
    let mut config = SupervisorConfig::new(worker_path);
    config.parallelism = 1;
    config.wall_timeout = timeout;
    config.max_rss_bytes = max_rss_bytes;
    let parser = ParserIdentity::new(implementation, DEFAULT_PARSER_CONFIG);
    let baseline = match predicate {
        MinimizePredicate::Status(_) => None,
        MinimizePredicate::Finding {
            baseline_worker,
            baseline_implementation,
            code,
        } => {
            let mut baseline_config = SupervisorConfig::new(baseline_worker);
            baseline_config.parallelism = 1;
            baseline_config.wall_timeout = timeout;
            baseline_config.max_rss_bytes = max_rss_bytes;
            Some((
                baseline_config,
                ParserIdentity::new(baseline_implementation, DEFAULT_PARSER_CONFIG),
                code,
            ))
        }
    };
    let result = minimize::minimize(
        &source,
        &MinimizeConfig::default(),
        &mut PredicateCache::default(),
        |candidate| -> TaskResult<bool> {
            fs::write(&candidate_path, candidate)?;
            let request = WorkerRequest::new(
                SourceIdentity {
                    path: candidate_path.clone(),
                    sha256: worker::sha256_hex(candidate.as_bytes()),
                },
                parser.clone(),
            );
            let outcome = run::run_one(&config, request);
            match &baseline {
                None => {
                    let MinimizePredicate::Status(requested) = predicate else {
                        unreachable!("baseline configuration matches finding predicate")
                    };
                    Ok(status_of(&outcome) == Some(requested))
                }
                Some((baseline_config, baseline_parser, code)) => {
                    let baseline_request = WorkerRequest::new(
                        SourceIdentity {
                            path: candidate_path.clone(),
                            sha256: worker::sha256_hex(candidate.as_bytes()),
                        },
                        baseline_parser.clone(),
                    );
                    let baseline_outcome = run::run_one(baseline_config, baseline_request);
                    let baseline_results = singleton_result_set(&baseline_outcome);
                    let candidate_results = singleton_result_set(&outcome);
                    let report = diff::compare(
                        &baseline_results,
                        &candidate_results,
                        &[],
                        &DiffPolicy::default(),
                    );
                    Ok(report.findings.iter().any(|finding| finding.code == *code))
                }
            }
        },
    )?;
    ensure_parent(output)?;
    fs::write(output, result.source.as_bytes())?;
    Ok(result)
}

fn singleton_result_set(outcome: &run::RunOutcome) -> WorkerResultSet {
    let case = outcome.request.decoded_source.sha256.clone();
    WorkerResultSet {
        implementation: outcome.request.parser.commit.clone(),
        manifest_sha256: "minimization".into(),
        shard: None,
        expected_cases: vec![case],
        accounting: RunAccounting {
            expected: 1,
            responses: usize::from(outcome.response.is_ok()),
            supervisor_failures: usize::from(outcome.response.is_err()),
            cache_hits: usize::from(outcome.cached),
        },
        results: vec![WorkerObservation::from_run_outcome(outcome, false)],
    }
}

fn status_of(outcome: &run::RunOutcome) -> Option<RequestedStatus> {
    match &outcome.response {
        Err(failure)
            if matches!(
                failure.kind,
                run::RunFailureKind::Crash | run::RunFailureKind::Signal
            ) =>
        {
            Some(RequestedStatus::Crash)
        }
        Err(_) => None,
        Ok(response) => match &response.outcome {
            worker::WorkerOutcome::Parsed { report }
                if matches!(report.status, ParserStatus::Empty | ParserStatus::Complete)
                    && !report.resource_limited
                    && !report.diagnostics_truncated
                    && report.validation.root_lossless
                    && report.validation.token_concatenation_lossless
                    && report.validation.ranges_valid
                    && report.validation.snapshot_valid =>
            {
                Some(RequestedStatus::Accepted)
            }
            worker::WorkerOutcome::Parsed { report }
                if matches!(
                    report.status,
                    ParserStatus::Incomplete | ParserStatus::Invalid
                ) =>
            {
                Some(RequestedStatus::Rejected)
            }
            worker::WorkerOutcome::Parsed { .. } | worker::WorkerOutcome::Failed { .. } => None,
        },
    }
}

pub fn replay(
    source: &Path,
    baseline_worker: &Path,
    candidate_worker: &Path,
    baseline_name: &str,
    candidate_name: &str,
    timeout: Duration,
    max_rss_bytes: Option<u64>,
) -> TaskResult<serde_json::Value> {
    let bytes = fs::read(source)?;
    let digest = worker::sha256_hex(&bytes);
    let run_worker = |path: &Path, implementation: &str| {
        let mut config = SupervisorConfig::new(path);
        config.wall_timeout = timeout;
        config.max_rss_bytes = max_rss_bytes;
        let request = WorkerRequest::new(
            SourceIdentity {
                path: source.to_owned(),
                sha256: digest.clone(),
            },
            ParserIdentity::new(implementation, DEFAULT_PARSER_CONFIG),
        );
        full_record(&run::run_one(&config, request))
    };
    Ok(serde_json::json!({
        "source_sha256": digest,
        "baseline": run_worker(baseline_worker, baseline_name),
        "candidate": run_worker(candidate_worker, candidate_name),
    }))
}

pub fn self_test() -> TaskResult<WorkerResponse> {
    let mut source = NamedTempFile::new()?;
    source.write_all(b"x <- function(a) a + 1\nx(2)\n")?;
    source.as_file_mut().sync_all()?;
    let request = WorkerRequest::new(
        SourceIdentity {
            path: source.path().to_owned(),
            sha256: worker::sha256_hex(b"x <- function(a) a + 1\nx(2)\n"),
        },
        ParserIdentity::new("self-test", DEFAULT_PARSER_CONFIG),
    );
    let response = worker::process_request(request);
    match &response.outcome {
        worker::WorkerOutcome::Parsed { report }
            if report.status == ParserStatus::Complete
                && report.validation.root_lossless
                && report.validation.token_concatenation_lossless
                && report.validation.ranges_valid
                && report.validation.snapshot_valid => {}
        other => return Err(format!("self-test parser/invariant failure: {other:?}").into()),
    }
    Ok(response)
}

pub fn write_json_zst<T: Serialize>(path: &Path, value: &T) -> TaskResult<()> {
    ensure_parent(path)?;
    report::write_json_zst(path, value)?;
    Ok(())
}

pub fn read_json_zst<T: DeserializeOwned>(path: &Path) -> TaskResult<T> {
    let decoder = zstd::stream::read::Decoder::new(File::open(path)?)?;
    let mut bytes = Vec::new();
    decoder
        .take(1024 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 * 1024 {
        return Err("compressed JSON expands beyond 1 GiB".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn ensure_parent(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")))
}
