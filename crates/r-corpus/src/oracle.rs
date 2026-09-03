//! A pinned, container-only reference-R parser oracle.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;
use wait_timeout::ChildExt;

pub const PARSE_SCRIPT: &str = include_str!("../oracle/parse.R");

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OracleConfig {
    /// Docker/OCI image pinned by digest, for example `r-base@sha256:...`.
    pub image: String,
    pub r_version: String,
    pub locale: String,
    /// OCI platform passed verbatim to the runtime, for example `linux/amd64`.
    pub platform: String,
    pub memory: String,
    pub cpus: String,
    pub pids_limit: u32,
    pub timeout_seconds: u64,
    pub max_output_bytes: usize,
}

impl OracleConfig {
    pub fn validate(&self) -> Result<(), OracleError> {
        let digest = self.image.rsplit_once("@sha256:").map(|(_, digest)| digest);
        if !matches!(digest, Some(value) if value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        {
            return Err(OracleError::InvalidConfig(
                "oracle image must contain a full @sha256 digest".into(),
            ));
        }
        for (name, value) in [
            ("R version", &self.r_version),
            ("locale", &self.locale),
            ("platform", &self.platform),
            ("memory", &self.memory),
            ("cpus", &self.cpus),
        ] {
            if value.is_empty() || value.contains('\0') {
                return Err(OracleError::InvalidConfig(format!(
                    "{name} must be non-empty"
                )));
            }
        }
        if self.pids_limit == 0 {
            return Err(OracleError::InvalidConfig(
                "pids_limit must be positive".into(),
            ));
        }
        if self.timeout_seconds == 0 || self.max_output_bytes == 0 {
            return Err(OracleError::InvalidConfig(
                "timeout and output limit must be positive".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OracleProvenance {
    pub image: String,
    pub r_version: String,
    pub locale: String,
    pub platform: String,
    pub script_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OracleOutcome {
    Accepted,
    Rejected {
        message: String,
        line: Option<u64>,
        column: Option<u64>,
    },
    Infrastructure {
        message: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OracleRecord {
    pub path: String,
    pub sha256: String,
    pub outcome: OracleOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OracleRun {
    pub provenance: OracleProvenance,
    /// Sorted by normalized corpus-relative path.
    pub records: Vec<OracleRecord>,
}

impl OracleRun {
    /// Stable cache key covering the input manifest and all oracle provenance.
    pub fn cache_key(&self) -> String {
        let bytes = serde_json::to_vec(self).expect("oracle models are serializable");
        format!("{:x}", Sha256::digest(bytes))
    }
}

#[derive(Debug)]
pub enum OracleError {
    InvalidConfig(String),
    Io(std::io::Error),
    Runtime { status: Option<i32>, stderr: String },
    Protocol { line: usize, message: String },
}

impl fmt::Display for OracleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(f, "invalid oracle configuration: {message}"),
            Self::Io(error) => write!(f, "container runtime failed: {error}"),
            Self::Runtime { status, stderr } => {
                write!(f, "oracle container exited with {status:?}: {stderr}")
            }
            Self::Protocol { line, message } => {
                write!(f, "invalid oracle output at line {line}: {message}")
            }
        }
    }
}

impl std::error::Error for OracleError {}

impl From<std::io::Error> for OracleError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

/// Abstract command execution so tests need not install or invoke a runtime.
pub trait CommandExecutor {
    fn output(
        &self,
        program: &str,
        args: &[String],
        timeout: Duration,
        max_output_bytes: usize,
    ) -> std::io::Result<Output>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessExecutor;

impl CommandExecutor for ProcessExecutor {
    fn output(
        &self,
        program: &str,
        args: &[String],
        timeout: Duration,
        max_output_bytes: usize,
    ) -> std::io::Result<Output> {
        let mut child = Command::new(program)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let stdout = thread::spawn(move || read_bounded(stdout, max_output_bytes));
        let stderr = thread::spawn(move || read_bounded(stderr, max_output_bytes));
        let status = match child.wait_timeout(timeout)? {
            Some(status) => status,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                if let Some(name) = args.iter().find_map(|arg| arg.strip_prefix("--name=")) {
                    let _ = Command::new(program).args(["rm", "-f", name]).status();
                }
                let _ = stdout.join();
                let _ = stderr.join();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "oracle container exceeded wall timeout",
                ));
            }
        };
        Ok(Output {
            status,
            stdout: stdout
                .join()
                .map_err(|_| std::io::Error::other("oracle stdout reader panicked"))??,
            stderr: stderr
                .join()
                .map_err(|_| std::io::Error::other("oracle stderr reader panicked"))??,
        })
    }
}

pub struct OracleRunner<E = ProcessExecutor> {
    runtime: String,
    config: OracleConfig,
    executor: E,
}

impl OracleRunner<ProcessExecutor> {
    pub fn new(runtime: impl Into<String>, config: OracleConfig) -> Result<Self, OracleError> {
        Self::with_executor(runtime, config, ProcessExecutor)
    }
}

impl<E: CommandExecutor> OracleRunner<E> {
    pub fn with_executor(
        runtime: impl Into<String>,
        config: OracleConfig,
        executor: E,
    ) -> Result<Self, OracleError> {
        config.validate()?;
        let runtime = runtime.into();
        if runtime != "docker" && runtime != "podman" {
            return Err(OracleError::InvalidConfig(
                "runtime must be exactly docker or podman; host R is forbidden".into(),
            ));
        }
        Ok(Self {
            runtime,
            config,
            executor,
        })
    }

    pub fn provenance(&self) -> OracleProvenance {
        OracleProvenance {
            image: self.config.image.clone(),
            r_version: self.config.r_version.clone(),
            locale: self.config.locale.clone(),
            platform: self.config.platform.clone(),
            script_sha256: format!("{:x}", Sha256::digest(PARSE_SCRIPT.as_bytes())),
        }
    }

    /// Parse every `.R` file under `decoded_corpus` in the pinned container.
    pub fn run(&self, decoded_corpus: &Path) -> Result<OracleRun, OracleError> {
        let corpus = absolute(decoded_corpus)?;
        let mut script_file = tempfile::NamedTempFile::new()?;
        script_file.write_all(PARSE_SCRIPT.as_bytes())?;
        script_file.as_file_mut().sync_all()?;
        let script = absolute(script_file.path())?;
        let sources = source_hashes(&corpus)?;
        let args = self.arguments(&corpus, &script);
        let output = match self.executor.output(
            &self.runtime,
            &args,
            Duration::from_secs(self.config.timeout_seconds),
            self.config.max_output_bytes,
        ) {
            Ok(output) => output,
            Err(error) => {
                let message = format!("failed to start container runtime: {error}");
                return Ok(OracleRun {
                    provenance: self.provenance(),
                    records: sources
                        .into_iter()
                        .map(|(path, sha256)| OracleRecord {
                            path,
                            sha256,
                            outcome: OracleOutcome::Infrastructure {
                                message: message.clone(),
                            },
                        })
                        .collect(),
                });
            }
        };
        if !output.status.success() {
            let message = format!(
                "container exited with {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return Ok(OracleRun {
                provenance: self.provenance(),
                records: sources
                    .into_iter()
                    .map(|(path, sha256)| OracleRecord {
                        path,
                        sha256,
                        outcome: OracleOutcome::Infrastructure {
                            message: message.clone(),
                        },
                    })
                    .collect(),
            });
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut records = Vec::new();
        for (index, line) in stdout.lines().enumerate() {
            let wire: WireRecord =
                serde_json::from_str(line).map_err(|error| OracleError::Protocol {
                    line: index + 1,
                    message: error.to_string(),
                })?;
            let sha256 = sources
                .get(&wire.path)
                .ok_or_else(|| OracleError::Protocol {
                    line: index + 1,
                    message: format!("unknown source path {}", wire.path),
                })?
                .clone();
            records.push(OracleRecord {
                path: wire.path,
                sha256,
                outcome: wire.outcome,
            });
        }
        records.sort_by(|left: &OracleRecord, right| left.path.cmp(&right.path));
        if records.len() != sources.len() {
            return Err(OracleError::Protocol {
                line: 0,
                message: format!(
                    "oracle returned {} of {} source results",
                    records.len(),
                    sources.len()
                ),
            });
        }
        for pair in records.windows(2) {
            if pair[0].path == pair[1].path {
                return Err(OracleError::Protocol {
                    line: 0,
                    message: format!("duplicate path {}", pair[0].path),
                });
            }
        }
        Ok(OracleRun {
            provenance: self.provenance(),
            records,
        })
    }

    pub fn arguments(&self, corpus: &Path, script: &Path) -> Vec<String> {
        let name_hash = format!("{:x}", Sha256::digest(self.config.image.as_bytes()));
        vec![
            "run".into(),
            "--rm".into(),
            format!(
                "--name=r-corpus-oracle-{}-{}",
                std::process::id(),
                &name_hash[..12]
            ),
            "--network=none".into(),
            "--read-only".into(),
            "--cap-drop=ALL".into(),
            "--security-opt=no-new-privileges".into(),
            format!("--platform={}", self.config.platform),
            format!("--memory={}", self.config.memory),
            format!("--cpus={}", self.config.cpus),
            format!("--pids-limit={}", self.config.pids_limit),
            "--tmpfs=/tmp:rw,noexec,nosuid,size=16m".into(),
            format!(
                "--mount=type=bind,src={},dst=/corpus,readonly",
                corpus.display()
            ),
            format!(
                "--mount=type=bind,src={},dst=/oracle/parse.R,readonly",
                script.display()
            ),
            format!("--env=EXPECTED_R_VERSION={}", self.config.r_version),
            format!("--env=LC_ALL={}", self.config.locale),
            format!("--env=LANG={}", self.config.locale),
            "--entrypoint=Rscript".into(),
            self.config.image.clone(),
            "--vanilla".into(),
            "/oracle/parse.R".into(),
            "/corpus".into(),
        ]
    }
}

fn read_bounded(mut reader: impl std::io::Read, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(output);
        }
        if output.len().saturating_add(read) > limit {
            return Err(std::io::Error::other(
                "oracle output exceeded configured limit",
            ));
        }
        output.extend_from_slice(&buffer[..read]);
    }
}

#[derive(Deserialize)]
struct WireRecord {
    path: String,
    outcome: OracleOutcome,
}

fn absolute(path: impl Into<PathBuf>) -> Result<PathBuf, std::io::Error> {
    let path = path.into();
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn source_hashes(root: &Path) -> Result<BTreeMap<String, String>, std::io::Error> {
    fn visit(
        root: &Path,
        directory: &Path,
        output: &mut BTreeMap<String, String>,
    ) -> Result<(), std::io::Error> {
        let mut entries = std::fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                visit(root, &path, output)?;
            } else if kind.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .expect("visited paths remain below root")
                    .to_string_lossy()
                    .replace('\\', "/");
                let digest = format!("{:x}", Sha256::digest(std::fs::read(&path)?));
                output.insert(relative, digest);
            }
        }
        Ok(())
    }

    let mut output = BTreeMap::new();
    visit(root, root, &mut output)?;
    Ok(output)
}

/// Build a path-keyed lookup and reject duplicate oracle rows.
pub fn index(run: &OracleRun) -> Result<BTreeMap<&str, &OracleRecord>, OracleError> {
    let mut result = BTreeMap::new();
    for record in &run.records {
        if result.insert(record.path.as_str(), record).is_some() {
            return Err(OracleError::Protocol {
                line: 0,
                message: format!("duplicate path {}", record.path),
            });
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> OracleConfig {
        OracleConfig {
            image: format!("r-base@sha256:{}", "a".repeat(64)),
            r_version: "4.4.1".into(),
            locale: "C.UTF-8".into(),
            platform: "linux/amd64".into(),
            memory: "512m".into(),
            cpus: "1".into(),
            pids_limit: 64,
            timeout_seconds: 3_600,
            max_output_bytes: 64 * 1024 * 1024,
        }
    }

    #[test]
    fn rejects_unpinned_images_and_host_r() {
        let mut invalid = config();
        invalid.image = "r-base:latest".into();
        assert!(invalid.validate().is_err());
        assert!(OracleRunner::new("Rscript", config()).is_err());
    }

    #[test]
    fn command_is_sandboxed_and_pinned() {
        let runner = OracleRunner::new("docker", config()).unwrap();
        let args = runner.arguments(Path::new("/input"), Path::new("/script"));
        assert!(args.contains(&"--network=none".to_owned()));
        assert!(args.contains(&"--read-only".to_owned()));
        assert!(args
            .iter()
            .any(|arg| arg == "--mount=type=bind,src=/input,dst=/corpus,readonly"));
        assert!(args.contains(&config().image));
    }

    #[test]
    fn checked_in_script_has_no_execution_primitives() {
        assert!(PARSE_SCRIPT.contains("parse(file = path, keep.source = TRUE)"));
        for forbidden in ["source(", "eval(", "load(", "install.packages("] {
            assert!(!PARSE_SCRIPT.contains(forbidden), "found {forbidden}");
        }
    }
}
