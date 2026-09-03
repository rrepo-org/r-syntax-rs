//! Deterministic machine-readable reports and standalone finding bundles.

use crate::diff::{DiffReport, Disposition, Finding};
use crate::oracle::OracleRecord;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum ReportError {
    Io(io::Error),
    Json(serde_json::Error),
    InvalidBundlePath(PathBuf),
}

impl std::fmt::Display for ReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "report I/O failed: {error}"),
            Self::Json(error) => write!(f, "report serialization failed: {error}"),
            Self::InvalidBundlePath(path) => {
                write!(f, "bundle path is not a directory: {}", path.display())
            }
        }
    }
}

impl std::error::Error for ReportError {}

impl From<io::Error> for ReportError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for ReportError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

/// Write compact JSON inside a deterministic zstd frame.
pub fn write_json_zst<T: Serialize>(path: &Path, value: &T) -> Result<(), ReportError> {
    let file = BufWriter::new(File::create(path)?);
    let mut encoder = zstd::stream::write::Encoder::new(file, 19)?;
    encoder.include_checksum(false)?;
    serde_json::to_writer(&mut encoder, value)?;
    encoder.write_all(b"\n")?;
    encoder.finish()?.flush()?;
    Ok(())
}

/// Write one compact JSON value per line inside a deterministic zstd frame.
pub fn write_ndjson_zst<T: Serialize>(path: &Path, values: &[T]) -> Result<(), ReportError> {
    let file = BufWriter::new(File::create(path)?);
    let mut encoder = zstd::stream::write::Encoder::new(file, 19)?;
    encoder.include_checksum(false)?;
    for value in values {
        serde_json::to_writer(&mut encoder, value)?;
        encoder.write_all(b"\n")?;
    }
    encoder.finish()?.flush()?;
    Ok(())
}

pub fn write_markdown(path: &Path, report: &DiffReport) -> Result<(), ReportError> {
    let hard = report
        .findings
        .iter()
        .filter(|f| f.disposition == Disposition::HardFailure)
        .count();
    let review = report.findings.len() - hard;
    let mut output = BufWriter::new(File::create(path)?);
    writeln!(output, "# Parser corpus differential report")?;
    writeln!(output)?;
    writeln!(
        output,
        "- Baseline: `{}`",
        markdown_code(&report.baseline_implementation)
    )?;
    writeln!(
        output,
        "- Candidate: `{}`",
        markdown_code(&report.candidate_implementation)
    )?;
    writeln!(output, "- Cases compared: {}", report.compared_cases)?;
    writeln!(
        output,
        "- Baseline accounting: expected {}, responses {}, supervisor failures {}, cache hits {}",
        report.baseline_accounting.expected,
        report.baseline_accounting.responses,
        report.baseline_accounting.supervisor_failures,
        report.baseline_accounting.cache_hits
    )?;
    writeln!(
        output,
        "- Candidate accounting: expected {}, responses {}, supervisor failures {}, cache hits {}",
        report.candidate_accounting.expected,
        report.candidate_accounting.responses,
        report.candidate_accounting.supervisor_failures,
        report.candidate_accounting.cache_hits
    )?;
    writeln!(output, "- Hard failures: {hard}")?;
    writeln!(output, "- Review items: {review}")?;
    writeln!(output)?;
    writeln!(output, "## Clusters")?;
    writeln!(output)?;
    writeln!(
        output,
        "| Disposition | Category | Code | Occurrences | Signature |"
    )?;
    writeln!(output, "| --- | --- | --- | ---: | --- |")?;
    for cluster in &report.clusters {
        writeln!(
            output,
            "| `{:?}` | `{:?}` | `{}` | {} | `{}` |",
            cluster.disposition,
            cluster.category,
            markdown_code(&cluster.code),
            cluster.occurrences.len(),
            cluster.signature
        )?;
    }
    output.flush()?;
    Ok(())
}

fn markdown_code(value: &str) -> String {
    value
        .replace('`', "\\`")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BundleCase {
    pub id: String,
    pub source_sha256: String,
    pub source: String,
    pub finding: Finding,
}

/// Inputs needed to emit a portable, standalone investigation directory.
#[derive(Clone, Debug, Serialize)]
pub struct Bundle<'a> {
    pub case: &'a BundleCase,
    pub baseline: &'a Value,
    pub candidate: &'a Value,
    pub oracle: Option<&'a OracleRecord>,
    /// Occurrence values should already be in stable order.
    pub occurrences: &'a [Value],
}

/// Create a bundle containing only deterministic content and fixed filenames.
/// Existing regular files with these names are replaced; unrelated files remain.
pub fn write_bundle(directory: &Path, bundle: &Bundle<'_>) -> Result<(), ReportError> {
    if directory.exists() && !directory.is_dir() {
        return Err(ReportError::InvalidBundlePath(directory.to_owned()));
    }
    fs::create_dir_all(directory)?;
    write_pretty_json(&directory.join("case.json"), bundle.case)?;
    write_pretty_json(&directory.join("baseline.json"), bundle.baseline)?;
    write_pretty_json(&directory.join("candidate.json"), bundle.candidate)?;
    write_pretty_json(&directory.join("oracle.json"), &bundle.oracle)?;
    write_ndjson_zst(
        &directory.join("occurrences.ndjson.zst"),
        bundle.occurrences,
    )?;
    fs::write(
        directory.join("reproducer.R"),
        bundle.case.source.as_bytes(),
    )?;
    write_pretty_json(
        &directory.join("baseline-request.json"),
        &replay_request(bundle.baseline, bundle.case, "baseline"),
    )?;
    write_pretty_json(
        &directory.join("candidate-request.json"),
        &replay_request(bundle.candidate, bundle.case, "candidate"),
    )?;
    let replay = "#!/bin/sh\nset -eu\n: \"${BASELINE_WORKER:?set BASELINE_WORKER}\"\n: \"${CANDIDATE_WORKER:?set CANDIDATE_WORKER}\"\n\"$BASELINE_WORKER\" < baseline-request.json > baseline-replay.json\n\"$CANDIDATE_WORKER\" < candidate-request.json > candidate-replay.json\nif [ -n \"${R_IMAGE:-}\" ]; then\n  RUNTIME=${RUNTIME:-docker}\n  case \"$RUNTIME\" in docker|podman) ;; *) echo 'RUNTIME must be docker or podman' >&2; exit 2;; esac\n  digest=${R_IMAGE##*@sha256:}\n  if [ \"$digest\" = \"$R_IMAGE\" ] || [ \"${#digest}\" -ne 64 ]; then echo 'R_IMAGE must end in a full sha256 digest' >&2; exit 2; fi\n  case \"$digest\" in *[!0-9a-f]*) echo 'R_IMAGE digest must be lowercase hexadecimal' >&2; exit 2;; esac\n  \"$RUNTIME\" run --rm --network=none --read-only --cap-drop=ALL --security-opt=no-new-privileges --pids-limit=64 --memory=512m --cpus=1 --tmpfs=/tmp:rw,noexec,nosuid,size=16m --mount \"type=bind,src=$(pwd)/reproducer.R,dst=/reproducer.R,readonly\" --entrypoint=Rscript \"$R_IMAGE\" --vanilla -e 'parse(file=\"/reproducer.R\", keep.source=TRUE)'\nfi\n";
    let replay_path = directory.join("replay.sh");
    fs::write(&replay_path, replay)?;
    set_executable(&replay_path)?;
    Ok(())
}

fn write_pretty_json<T: Serialize>(path: &Path, value: &T) -> Result<(), ReportError> {
    let mut output = BufWriter::new(File::create(path)?);
    serde_json::to_writer_pretty(&mut output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn replay_request(record: &Value, case: &BundleCase, implementation: &str) -> Value {
    let mut request = record.get("request").cloned().unwrap_or_else(|| {
        serde_json::json!({
            "protocol_version": 1,
            "decoded_source": {},
            "parser": {
                "commit": implementation,
                "config": "r-parser-default-v1",
                "config_sha256": crate::worker::sha256_hex(b"r-parser-default-v1")
            },
            "roxygen": "when_present"
        })
    });
    request["decoded_source"] = serde_json::json!({
        "path": "reproducer.R",
        "sha256": case.source_sha256,
    });
    request
}

#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{FailureCategory, Finding};

    fn finding() -> Finding {
        Finding {
            case: "pkg/a.R".into(),
            source_sha256: Some("abc".into()),
            category: FailureCategory::Oracle,
            disposition: Disposition::HardFailure,
            code: "mismatch".into(),
            summary: "summary".into(),
            signature: "sig".into(),
        }
    }

    #[test]
    fn compressed_output_is_byte_reproducible() {
        let directory = tempfile::tempdir().unwrap();
        let one = directory.path().join("one.zst");
        let two = directory.path().join("two.zst");
        write_ndjson_zst(&one, &[finding()]).unwrap();
        write_ndjson_zst(&two, &[finding()]).unwrap();
        assert_eq!(fs::read(one).unwrap(), fs::read(two).unwrap());
    }

    #[test]
    fn bundle_contains_all_contract_files_and_embeds_source() {
        let directory = tempfile::tempdir().unwrap();
        let case = BundleCase {
            id: "x".into(),
            source_sha256: "abc".into(),
            source: "λ <- 1\n".into(),
            finding: finding(),
        };
        let value = serde_json::json!({"status": "accepted"});
        write_bundle(
            directory.path(),
            &Bundle {
                case: &case,
                baseline: &value,
                candidate: &value,
                oracle: None,
                occurrences: &[],
            },
        )
        .unwrap();
        for name in [
            "case.json",
            "reproducer.R",
            "baseline.json",
            "candidate.json",
            "oracle.json",
            "occurrences.ndjson.zst",
            "replay.sh",
        ] {
            assert!(directory.path().join(name).is_file(), "missing {name}");
        }
        assert_eq!(
            fs::read_to_string(directory.path().join("reproducer.R")).unwrap(),
            "λ <- 1\n"
        );
    }
}
