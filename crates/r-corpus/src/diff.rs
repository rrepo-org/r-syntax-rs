//! Policy evaluation and stable clustering for corpus worker results.
//!
//! `WorkerObservation` is the normalization boundary for `crate::worker` while
//! that module's wire model is developed independently.

use crate::oracle::{OracleOutcome, OracleRecord};
use crate::{run, worker};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    Acquisition,
    Archive,
    Decoding,
    Parser,
    Differential,
    Oracle,
    Resource,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    HardFailure,
    Review,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerFailureKind {
    Acquisition,
    Archive,
    Decoding,
    Crash,
    Invariant,
    Resource,
    Infrastructure,
}

impl WorkerFailureKind {
    fn category(self) -> FailureCategory {
        match self {
            Self::Acquisition => FailureCategory::Acquisition,
            Self::Archive => FailureCategory::Archive,
            Self::Decoding => FailureCategory::Decoding,
            Self::Resource => FailureCategory::Resource,
            Self::Crash | Self::Invariant | Self::Infrastructure => FailureCategory::Parser,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkerOutcome {
    Accepted,
    Rejected {
        message: String,
    },
    Failed {
        kind: WorkerFailureKind,
        message: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkerObservation {
    /// Stable corpus-relative path or case identifier.
    pub case: String,
    pub source_sha256: String,
    pub curated: bool,
    pub outcome: WorkerOutcome,
    pub elapsed_ns: u64,
    pub peak_bytes: Option<u64>,
    pub parser_status: Option<worker::ParserStatus>,
    pub tree_fingerprint: Option<String>,
    pub diagnostic_fingerprint: Option<String>,
    pub diagnostic_codes: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkerResultSet {
    pub implementation: String,
    pub manifest_sha256: String,
    pub shard: Option<String>,
    pub expected_cases: Vec<String>,
    pub accounting: RunAccounting,
    pub results: Vec<WorkerObservation>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAccounting {
    pub expected: usize,
    pub responses: usize,
    pub supervisor_failures: usize,
    pub cache_hits: usize,
}

impl WorkerObservation {
    /// Normalize a supervised worker result. Curated status comes from corpus metadata.
    pub fn from_run_outcome(value: &run::RunOutcome, curated: bool) -> Self {
        // Unique decoded content is the comparison unit; package paths remain
        // available through source-occurrence provenance in the manifest.
        let case = value.request.decoded_source.sha256.clone();
        let outcome = match &value.response {
            Err(error) => WorkerOutcome::Failed {
                kind: match error.kind {
                    run::RunFailureKind::Crash | run::RunFailureKind::Signal => {
                        WorkerFailureKind::Crash
                    }
                    run::RunFailureKind::Timeout | run::RunFailureKind::MemoryLimit => {
                        WorkerFailureKind::Resource
                    }
                    run::RunFailureKind::Spawn | run::RunFailureKind::Protocol => {
                        WorkerFailureKind::Infrastructure
                    }
                },
                message: error.message.clone(),
            },
            Ok(response) => match &response.outcome {
                worker::WorkerOutcome::Failed { error } => WorkerOutcome::Failed {
                    kind: match error.kind {
                        worker::WorkerErrorKind::ReadSource => WorkerFailureKind::Acquisition,
                        worker::WorkerErrorKind::InvalidUtf8 => WorkerFailureKind::Decoding,
                        worker::WorkerErrorKind::HashMismatch => WorkerFailureKind::Invariant,
                        worker::WorkerErrorKind::InvalidRequest
                        | worker::WorkerErrorKind::UnsupportedProtocol
                        | worker::WorkerErrorKind::UnsupportedConfig => {
                            WorkerFailureKind::Infrastructure
                        }
                    },
                    message: error.message.clone(),
                },
                worker::WorkerOutcome::Parsed { report }
                    if report.resource_limited || report.diagnostics_truncated =>
                {
                    WorkerOutcome::Failed {
                        kind: WorkerFailureKind::Resource,
                        message: "parser output was resource limited or truncated".into(),
                    }
                }
                worker::WorkerOutcome::Parsed { report }
                    if !report.validation.root_lossless
                        || !report.validation.token_concatenation_lossless
                        || !report.validation.ranges_valid
                        || !report.validation.snapshot_valid =>
                {
                    WorkerOutcome::Failed {
                        kind: WorkerFailureKind::Invariant,
                        message: "parser validation invariant failed".into(),
                    }
                }
                worker::WorkerOutcome::Parsed { report }
                    if matches!(
                        report.status,
                        worker::ParserStatus::Complete | worker::ParserStatus::Empty
                    ) =>
                {
                    WorkerOutcome::Accepted
                }
                worker::WorkerOutcome::Parsed { report } => WorkerOutcome::Rejected {
                    message: format!("parser status {:?}", report.status),
                },
            },
        };
        let report = value.response.as_ref().ok().and_then(|response| {
            if let worker::WorkerOutcome::Parsed { report } = &response.outcome {
                Some(report)
            } else {
                None
            }
        });
        Self {
            case,
            source_sha256: value.request.decoded_source.sha256.clone(),
            curated,
            outcome,
            elapsed_ns: value
                .response
                .as_ref()
                .ok()
                .and_then(|response| match &response.outcome {
                    worker::WorkerOutcome::Parsed { report } => Some(report.parse_nanos),
                    worker::WorkerOutcome::Failed { .. } => None,
                })
                .unwrap_or_else(|| u64::try_from(value.elapsed.as_nanos()).unwrap_or(u64::MAX)),
            peak_bytes: value.peak_rss_bytes,
            parser_status: report.map(|report| report.status),
            tree_fingerprint: report.map(|report| report.fingerprints.tree.clone()),
            diagnostic_fingerprint: report.map(|report| report.fingerprints.diagnostics.clone()),
            diagnostic_codes: report
                .map(|report| {
                    report
                        .diagnostics
                        .iter()
                        .map(|diagnostic| diagnostic.code.clone())
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffPolicy {
    /// Candidate/baseline ratio at or above which a performance issue is emitted.
    pub performance_ratio: f64,
    /// Ignore noisy ratio changes smaller than this absolute duration.
    pub performance_min_delta_ns: u64,
    /// Ignore noisy peak-memory changes smaller than this many bytes.
    pub memory_min_delta_bytes: u64,
    pub performance_disposition: Disposition,
}

impl Default for DiffPolicy {
    fn default() -> Self {
        Self {
            performance_ratio: 2.0,
            performance_min_delta_ns: 5_000_000,
            memory_min_delta_bytes: 1024 * 1024,
            performance_disposition: Disposition::Review,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub case: String,
    pub source_sha256: Option<String>,
    pub category: FailureCategory,
    pub disposition: Disposition,
    pub code: String,
    pub summary: String,
    /// Path-independent digest suitable for clustering across corpus runs.
    pub signature: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FindingCluster {
    pub signature: String,
    pub category: FailureCategory,
    pub disposition: Disposition,
    pub code: String,
    pub summary: String,
    pub occurrences: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiffReport {
    pub baseline_implementation: String,
    pub candidate_implementation: String,
    pub compared_cases: usize,
    pub baseline_accounting: RunAccounting,
    pub candidate_accounting: RunAccounting,
    pub findings: Vec<Finding>,
    pub clusters: Vec<FindingCluster>,
}

impl DiffReport {
    pub fn has_hard_failures(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.disposition == Disposition::HardFailure)
    }
}

/// Compare complete result sets. Missing, extra, duplicate, and hash-mismatched
/// cases are findings rather than silently disappearing in an inner join.
pub fn compare(
    baseline: &WorkerResultSet,
    candidate: &WorkerResultSet,
    oracle: &[OracleRecord],
    policy: &DiffPolicy,
) -> DiffReport {
    let (baseline_index, baseline_duplicates) = index_workers(&baseline.results);
    let (candidate_index, candidate_duplicates) = index_workers(&candidate.results);
    let (oracle_index, oracle_duplicates) = index_oracle(oracle);
    let cases = baseline_index
        .keys()
        .chain(candidate_index.keys())
        .chain(oracle_index.keys())
        .copied()
        .chain(baseline.expected_cases.iter().map(String::as_str))
        .chain(candidate.expected_cases.iter().map(String::as_str))
        .collect::<BTreeSet<_>>();
    let mut findings = Vec::new();
    if baseline.manifest_sha256 != candidate.manifest_sha256 || baseline.shard != candidate.shard {
        push(
            &mut findings,
            "<run-scope>",
            None,
            FailureCategory::Acquisition,
            Disposition::HardFailure,
            "run_scope_mismatch",
            "baseline and candidate use different manifest or shard scopes",
        );
    }
    validate_run_accounting("baseline", baseline, &mut findings);
    validate_run_accounting("candidate", candidate, &mut findings);

    for case in &baseline_duplicates {
        push(
            &mut findings,
            case,
            None,
            FailureCategory::Acquisition,
            Disposition::HardFailure,
            "duplicate_baseline",
            "baseline contains duplicate case identifiers",
        );
    }
    for case in &candidate_duplicates {
        push(
            &mut findings,
            case,
            None,
            FailureCategory::Acquisition,
            Disposition::HardFailure,
            "duplicate_candidate",
            "candidate contains duplicate case identifiers",
        );
    }
    for case in &oracle_duplicates {
        push(
            &mut findings,
            case,
            None,
            FailureCategory::Oracle,
            Disposition::HardFailure,
            "duplicate_oracle",
            "oracle contains duplicate case identifiers",
        );
    }

    for case in &cases {
        let old = baseline_index.get(case).copied();
        let new = candidate_index.get(case).copied();
        let reference = oracle_index.get(case).copied();
        match (old, new) {
            (None, Some(value)) => push(
                &mut findings,
                case,
                Some(&value.source_sha256),
                FailureCategory::Acquisition,
                Disposition::HardFailure,
                "missing_baseline",
                "case is absent from baseline result set",
            ),
            (Some(value), None) => push(
                &mut findings,
                case,
                Some(&value.source_sha256),
                FailureCategory::Acquisition,
                Disposition::HardFailure,
                "missing_candidate",
                "case is absent from candidate result set",
            ),
            (None, None) => {
                if reference.is_some() {
                    push(
                        &mut findings,
                        case,
                        None,
                        FailureCategory::Oracle,
                        Disposition::Review,
                        "orphan_oracle",
                        "oracle case is absent from both worker sets",
                    );
                } else {
                    push(
                        &mut findings,
                        case,
                        None,
                        FailureCategory::Acquisition,
                        Disposition::HardFailure,
                        "missing_both_runs",
                        "expected manifest case is absent from both worker sets",
                    );
                }
            }
            (Some(old), Some(new)) => {
                if !oracle.is_empty() && reference.is_none() {
                    push(
                        &mut findings,
                        case,
                        Some(&new.source_sha256),
                        FailureCategory::Oracle,
                        Disposition::Review,
                        "missing_oracle",
                        "case is absent from oracle result set",
                    );
                }
                compare_case(old, new, reference, policy, &mut findings);
            }
        }
    }

    findings
        .sort_by(|a, b| (&a.case, &a.code, &a.signature).cmp(&(&b.case, &b.code, &b.signature)));
    let clusters = cluster(&findings);
    DiffReport {
        baseline_implementation: baseline.implementation.clone(),
        candidate_implementation: candidate.implementation.clone(),
        compared_cases: cases.len(),
        baseline_accounting: baseline.accounting.clone(),
        candidate_accounting: candidate.accounting.clone(),
        findings,
        clusters,
    }
}

fn validate_run_accounting(name: &str, run: &WorkerResultSet, findings: &mut Vec<Finding>) {
    let unique_expected = run.expected_cases.iter().collect::<BTreeSet<_>>().len();
    let accounted = run
        .accounting
        .responses
        .saturating_add(run.accounting.supervisor_failures);
    if unique_expected != run.expected_cases.len()
        || run.accounting.expected != run.expected_cases.len()
        || accounted != run.accounting.expected
        || run.results.len() != run.accounting.expected
    {
        push(
            findings,
            "<run-accounting>",
            None,
            FailureCategory::Acquisition,
            Disposition::HardFailure,
            if name == "baseline" {
                "baseline_accounting_mismatch"
            } else {
                "candidate_accounting_mismatch"
            },
            "run accounting does not balance against its expected manifest cases",
        );
    }
}

fn compare_case(
    old: &WorkerObservation,
    new: &WorkerObservation,
    oracle: Option<&OracleRecord>,
    policy: &DiffPolicy,
    findings: &mut Vec<Finding>,
) {
    if old.source_sha256 != new.source_sha256 {
        push(
            findings,
            &new.case,
            Some(&new.source_sha256),
            FailureCategory::Acquisition,
            Disposition::HardFailure,
            "source_mismatch",
            "baseline and candidate parsed different bytes",
        );
        return;
    }
    if let WorkerOutcome::Failed { kind, message } = &old.outcome {
        push(
            findings,
            &old.case,
            Some(&old.source_sha256),
            kind.category(),
            Disposition::Review,
            baseline_failure_code(*kind),
            message,
        );
    }
    if let WorkerOutcome::Failed { kind, message } = &new.outcome {
        let disposition = if matches!(
            kind,
            WorkerFailureKind::Crash | WorkerFailureKind::Invariant | WorkerFailureKind::Resource
        ) {
            Disposition::HardFailure
        } else {
            Disposition::Review
        };
        push(
            findings,
            &new.case,
            Some(&new.source_sha256),
            kind.category(),
            disposition,
            failure_code(*kind),
            message,
        );
    }

    let old_accepts = matches!(old.outcome, WorkerOutcome::Accepted);
    let new_accepts = matches!(new.outcome, WorkerOutcome::Accepted);
    if old_accepts != new_accepts && !matches!(new.outcome, WorkerOutcome::Failed { .. }) {
        push(
            findings,
            &new.case,
            Some(&new.source_sha256),
            FailureCategory::Differential,
            Disposition::Review,
            if new_accepts {
                "candidate_newly_accepts"
            } else {
                "candidate_newly_rejects"
            },
            "baseline and candidate parser acceptance differs",
        );
    }
    if old.parser_status != new.parser_status {
        push(
            findings,
            &new.case,
            Some(&new.source_sha256),
            FailureCategory::Differential,
            Disposition::Review,
            "parser_status_changed",
            "baseline and candidate parser status differs",
        );
    }
    if old.tree_fingerprint != new.tree_fingerprint
        && old.tree_fingerprint.is_some()
        && new.tree_fingerprint.is_some()
    {
        push(
            findings,
            &new.case,
            Some(&new.source_sha256),
            FailureCategory::Differential,
            Disposition::Review,
            "tree_changed",
            "candidate tree fingerprint differs from baseline",
        );
    }
    if old.diagnostic_fingerprint != new.diagnostic_fingerprint
        && old.diagnostic_fingerprint.is_some()
        && new.diagnostic_fingerprint.is_some()
    {
        push(
            findings,
            &new.case,
            Some(&new.source_sha256),
            FailureCategory::Differential,
            Disposition::Review,
            "diagnostics_changed",
            &format!(
                "candidate diagnostics differ from baseline: {:?} -> {:?}",
                old.diagnostic_codes, new.diagnostic_codes
            ),
        );
    }

    if let Some(oracle) = oracle {
        if oracle.sha256 != new.source_sha256 {
            push(
                findings,
                &new.case,
                Some(&new.source_sha256),
                FailureCategory::Oracle,
                Disposition::HardFailure,
                "oracle_source_mismatch",
                "oracle and worker parsed different bytes",
            );
        } else {
            match (&oracle.outcome, &new.outcome) {
                (OracleOutcome::Accepted, WorkerOutcome::Rejected { .. }) => push(
                    findings,
                    &new.case,
                    Some(&new.source_sha256),
                    FailureCategory::Oracle,
                    Disposition::HardFailure,
                    "r_accepts_candidate_rejects",
                    "reference R accepts but candidate rejects",
                ),
                (OracleOutcome::Rejected { .. }, WorkerOutcome::Accepted) => push(
                    findings,
                    &new.case,
                    Some(&new.source_sha256),
                    FailureCategory::Oracle,
                    if new.curated {
                        Disposition::HardFailure
                    } else {
                        Disposition::Review
                    },
                    if new.curated {
                        "curated_r_rejects_candidate_accepts"
                    } else {
                        "r_rejects_candidate_accepts"
                    },
                    "reference R rejects but candidate silently accepts",
                ),
                (OracleOutcome::Infrastructure { message }, _) => push(
                    findings,
                    &new.case,
                    Some(&new.source_sha256),
                    FailureCategory::Oracle,
                    Disposition::Review,
                    "oracle_infrastructure",
                    message,
                ),
                _ => {}
            }
        }
    }

    let delta = new.elapsed_ns.saturating_sub(old.elapsed_ns);
    let ratio_exceeded = old.elapsed_ns == 0
        || (new.elapsed_ns as f64 / old.elapsed_ns as f64) >= policy.performance_ratio;
    if new.elapsed_ns > old.elapsed_ns && delta >= policy.performance_min_delta_ns && ratio_exceeded
    {
        push(
            findings,
            &new.case,
            Some(&new.source_sha256),
            FailureCategory::Resource,
            policy.performance_disposition,
            "performance_regression",
            "candidate exceeds performance threshold",
        );
    }
    if let (Some(old_peak), Some(new_peak)) = (old.peak_bytes, new.peak_bytes) {
        let memory_delta = new_peak.saturating_sub(old_peak);
        let memory_ratio =
            old_peak == 0 || (new_peak as f64 / old_peak as f64) >= policy.performance_ratio;
        if new_peak > old_peak && memory_delta >= policy.memory_min_delta_bytes && memory_ratio {
            push(
                findings,
                &new.case,
                Some(&new.source_sha256),
                FailureCategory::Resource,
                policy.performance_disposition,
                "memory_regression",
                "candidate exceeds memory threshold",
            );
        }
    }
}

fn failure_code(kind: WorkerFailureKind) -> &'static str {
    match kind {
        WorkerFailureKind::Acquisition => "candidate_acquisition_failure",
        WorkerFailureKind::Archive => "candidate_archive_failure",
        WorkerFailureKind::Decoding => "candidate_decoding_failure",
        WorkerFailureKind::Crash => "candidate_crash",
        WorkerFailureKind::Invariant => "candidate_invariant",
        WorkerFailureKind::Resource => "candidate_resource_failure",
        WorkerFailureKind::Infrastructure => "candidate_infrastructure_failure",
    }
}

fn baseline_failure_code(kind: WorkerFailureKind) -> &'static str {
    match kind {
        WorkerFailureKind::Acquisition => "baseline_acquisition_failure",
        WorkerFailureKind::Archive => "baseline_archive_failure",
        WorkerFailureKind::Decoding => "baseline_decoding_failure",
        WorkerFailureKind::Crash => "baseline_crash",
        WorkerFailureKind::Invariant => "baseline_invariant",
        WorkerFailureKind::Resource => "baseline_resource_failure",
        WorkerFailureKind::Infrastructure => "baseline_infrastructure_failure",
    }
}

fn push(
    findings: &mut Vec<Finding>,
    case: &str,
    sha: Option<&str>,
    category: FailureCategory,
    disposition: Disposition,
    code: &str,
    summary: &str,
) {
    // Messages are deliberately omitted: volatile paths and diagnostics must not split clusters.
    let signature_input = format!("{category:?}\0{disposition:?}\0{code}");
    findings.push(Finding {
        case: case.to_owned(),
        source_sha256: sha.map(str::to_owned),
        category,
        disposition,
        code: code.to_owned(),
        summary: summary.to_owned(),
        signature: format!("{:x}", Sha256::digest(signature_input.as_bytes())),
    });
}

fn index_workers(
    results: &[WorkerObservation],
) -> (BTreeMap<&str, &WorkerObservation>, BTreeSet<&str>) {
    let mut index = BTreeMap::new();
    let mut duplicates = BTreeSet::new();
    for result in results {
        if index.insert(result.case.as_str(), result).is_some() {
            duplicates.insert(result.case.as_str());
        }
    }
    (index, duplicates)
}

fn index_oracle(results: &[OracleRecord]) -> (BTreeMap<&str, &OracleRecord>, BTreeSet<&str>) {
    let mut index = BTreeMap::new();
    let mut duplicates = BTreeSet::new();
    for result in results {
        if index.insert(result.path.as_str(), result).is_some() {
            duplicates.insert(result.path.as_str());
        }
    }
    (index, duplicates)
}

pub fn cluster(findings: &[Finding]) -> Vec<FindingCluster> {
    let mut clusters: BTreeMap<&str, FindingCluster> = BTreeMap::new();
    for finding in findings {
        let cluster = clusters
            .entry(&finding.signature)
            .or_insert_with(|| FindingCluster {
                signature: finding.signature.clone(),
                category: finding.category,
                disposition: finding.disposition,
                code: finding.code.clone(),
                summary: finding.summary.clone(),
                occurrences: Vec::new(),
            });
        cluster.occurrences.push(finding.case.clone());
    }
    for cluster in clusters.values_mut() {
        cluster.occurrences.sort();
        cluster.occurrences.dedup();
    }
    clusters.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(case: &str, outcome: WorkerOutcome) -> WorkerObservation {
        WorkerObservation {
            case: case.into(),
            source_sha256: "abc".into(),
            curated: true,
            outcome,
            elapsed_ns: 10,
            peak_bytes: Some(10),
            parser_status: None,
            tree_fingerprint: None,
            diagnostic_fingerprint: None,
            diagnostic_codes: Vec::new(),
        }
    }

    fn result_set(name: &str, results: Vec<WorkerObservation>) -> WorkerResultSet {
        let expected_cases = results
            .iter()
            .map(|item| item.case.clone())
            .collect::<Vec<_>>();
        WorkerResultSet {
            implementation: name.into(),
            manifest_sha256: "manifest".into(),
            shard: None,
            accounting: RunAccounting {
                expected: expected_cases.len(),
                responses: results.len(),
                supervisor_failures: 0,
                cache_hits: 0,
            },
            expected_cases,
            results,
        }
    }

    #[test]
    fn applies_oracle_hard_failure_policy() {
        let baseline = result_set("old", vec![observation("x.R", WorkerOutcome::Accepted)]);
        let candidate = result_set(
            "new",
            vec![observation(
                "x.R",
                WorkerOutcome::Rejected {
                    message: "no".into(),
                },
            )],
        );
        let oracle = OracleRecord {
            path: "x.R".into(),
            sha256: "abc".into(),
            outcome: OracleOutcome::Accepted,
        };
        let report = compare(&baseline, &candidate, &[oracle], &DiffPolicy::default());
        assert!(report.has_hard_failures());
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "r_accepts_candidate_rejects"));
    }

    #[test]
    fn candidate_crashes_and_missing_cases_are_hard_failures() {
        let baseline = result_set(
            "old",
            vec![
                observation("crash.R", WorkerOutcome::Accepted),
                observation("missing.R", WorkerOutcome::Accepted),
            ],
        );
        let candidate = result_set(
            "new",
            vec![observation(
                "crash.R",
                WorkerOutcome::Failed {
                    kind: WorkerFailureKind::Crash,
                    message: "signal 6".into(),
                },
            )],
        );
        let report = compare(&baseline, &candidate, &[], &DiffPolicy::default());
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "candidate_crash"
                && finding.disposition == Disposition::HardFailure));
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "missing_candidate"));
    }

    #[test]
    fn expected_cases_cannot_disappear_from_both_runs() {
        let mut baseline = result_set("old", Vec::new());
        baseline.expected_cases.push("omitted.R".into());
        baseline.accounting.expected = 1;
        let mut candidate = result_set("new", Vec::new());
        candidate.expected_cases.push("omitted.R".into());
        candidate.accounting.expected = 1;

        let report = compare(&baseline, &candidate, &[], &DiffPolicy::default());
        assert!(report.has_hard_failures());
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "missing_both_runs"));
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "baseline_accounting_mismatch"));
    }

    #[test]
    fn curated_silent_acceptance_is_hard_but_external_is_review() {
        for (curated, expected) in [
            (true, Disposition::HardFailure),
            (false, Disposition::Review),
        ] {
            let baseline = result_set("old", vec![observation("x.R", WorkerOutcome::Accepted)]);
            let mut result = observation("x.R", WorkerOutcome::Accepted);
            result.curated = curated;
            let candidate = result_set("new", vec![result]);
            let oracle = OracleRecord {
                path: "x.R".into(),
                sha256: "abc".into(),
                outcome: OracleOutcome::Rejected {
                    message: "unexpected token".into(),
                    line: Some(1),
                    column: None,
                },
            };
            let report = compare(&baseline, &candidate, &[oracle], &DiffPolicy::default());
            assert!(report
                .findings
                .iter()
                .any(|finding| finding.disposition == expected));
        }
    }

    #[test]
    fn performance_threshold_requires_ratio_and_absolute_delta() {
        let mut old = observation("x.R", WorkerOutcome::Accepted);
        old.elapsed_ns = 10_000_000;
        let baseline = result_set("old", vec![old]);
        let mut new = observation("x.R", WorkerOutcome::Accepted);
        new.elapsed_ns = 25_000_000;
        let candidate = result_set("new", vec![new]);
        let report = compare(&baseline, &candidate, &[], &DiffPolicy::default());
        assert!(report
            .findings
            .iter()
            .any(|finding| finding.code == "performance_regression"));
    }

    #[test]
    fn signatures_cluster_independent_of_case_and_message() {
        let mut findings = Vec::new();
        push(
            &mut findings,
            "a",
            None,
            FailureCategory::Parser,
            Disposition::HardFailure,
            "candidate_crash",
            "one",
        );
        push(
            &mut findings,
            "b",
            None,
            FailureCategory::Parser,
            Disposition::HardFailure,
            "candidate_crash",
            "two",
        );
        let clusters = cluster(&findings);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].occurrences, ["a", "b"]);
    }

    #[test]
    fn reports_tree_and_diagnostic_changes_without_acceptance_change() {
        let mut old = observation("x.R", WorkerOutcome::Accepted);
        old.tree_fingerprint = Some("old-tree".into());
        old.diagnostic_fingerprint = Some("old-diagnostics".into());
        let mut new = observation("x.R", WorkerOutcome::Accepted);
        new.tree_fingerprint = Some("new-tree".into());
        new.diagnostic_fingerprint = Some("new-diagnostics".into());
        new.diagnostic_codes = vec!["R-PARSE-001".into()];
        let report = compare(
            &result_set("old", vec![old]),
            &result_set("new", vec![new]),
            &[],
            &DiffPolicy::default(),
        );
        assert!(report
            .findings
            .iter()
            .any(|item| item.code == "tree_changed"));
        assert!(report
            .findings
            .iter()
            .any(|item| item.code == "diagnostics_changed"));
    }
}
