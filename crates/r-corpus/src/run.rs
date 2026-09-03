//! One-process-per-source worker supervision.

use std::{
    io::{self, Read, Write},
    path::PathBuf,
    process::{Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use sysinfo::{Pid, ProcessesToUpdate, System};

use crate::worker::{WorkerRequest, WorkerResponse, PROTOCOL_VERSION};

#[derive(Clone, Debug)]
pub struct SupervisorConfig {
    pub worker_executable: PathBuf,
    pub parallelism: usize,
    pub wall_timeout: Duration,
    pub max_rss_bytes: Option<u64>,
    pub poll_interval: Duration,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
}

impl SupervisorConfig {
    pub fn new(worker_executable: impl Into<PathBuf>) -> Self {
        Self {
            worker_executable: worker_executable.into(),
            parallelism: thread::available_parallelism().map_or(1, usize::from),
            wall_timeout: Duration::from_secs(30),
            max_rss_bytes: None,
            poll_interval: Duration::from_millis(10),
            max_stdout_bytes: 16 * 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunFailureKind {
    Spawn,
    Crash,
    Signal,
    Timeout,
    MemoryLimit,
    Protocol,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunFailure {
    pub kind: RunFailureKind,
    pub message: String,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stderr: String,
}

#[derive(Clone, Debug)]
pub struct RunOutcome {
    pub request: WorkerRequest,
    pub response: Result<WorkerResponse, RunFailure>,
    pub elapsed: Duration,
    pub peak_rss_bytes: Option<u64>,
    pub cached: bool,
}

/// Persistence seam for store-backed resume. Only valid protocol responses are cached.
pub trait WorkerCache: Sync {
    fn load(&self, request: &WorkerRequest) -> Option<WorkerResponse>;
    fn store(&self, request: &WorkerRequest, response: &WorkerResponse);
}

#[derive(Default)]
pub struct NoCache;

impl WorkerCache for NoCache {
    fn load(&self, _request: &WorkerRequest) -> Option<WorkerResponse> {
        None
    }

    fn store(&self, _request: &WorkerRequest, _response: &WorkerResponse) {}
}

/// Runs requests with bounded parallelism and returns results in input order.
pub fn run_sources<C: WorkerCache>(
    config: &SupervisorConfig,
    requests: impl IntoIterator<Item = WorkerRequest>,
    cache: &C,
) -> Vec<RunOutcome> {
    let requests = requests.into_iter().collect::<Vec<_>>();
    if requests.is_empty() {
        return Vec::new();
    }
    let next = AtomicUsize::new(0);
    let completed = Mutex::new(Vec::with_capacity(requests.len()));
    let workers = config.parallelism.max(1).min(requests.len());
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(request) = requests.get(index).cloned() else {
                    break;
                };
                let outcome = if let Some(response) = cache
                    .load(&request)
                    .filter(|response| response_matches(&request, response))
                {
                    RunOutcome {
                        request,
                        response: Ok(response),
                        elapsed: Duration::ZERO,
                        peak_rss_bytes: None,
                        cached: true,
                    }
                } else {
                    let outcome = run_one(config, request);
                    if let Ok(response) = &outcome.response {
                        cache.store(&outcome.request, response);
                    }
                    outcome
                };
                completed
                    .lock()
                    .expect("completion lock poisoned")
                    .push((index, outcome));
            });
        }
    });
    let mut completed = completed.into_inner().expect("completion lock poisoned");
    completed.sort_by_key(|(index, _)| *index);
    completed.into_iter().map(|(_, outcome)| outcome).collect()
}

/// Spawns a fresh worker, supervises it, and validates its sole response.
pub fn run_one(config: &SupervisorConfig, request: WorkerRequest) -> RunOutcome {
    let started = Instant::now();
    let mut child = match Command::new(&config.worker_executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return failed_outcome(
                request,
                started,
                None,
                RunFailureKind::Spawn,
                format!("failed to spawn worker: {error}"),
                None,
                String::new(),
            );
        }
    };

    let stdout = child.stdout.take().expect("piped stdout must be present");
    let stderr = child.stderr.take().expect("piped stderr must be present");
    let stdout_limit = config.max_stdout_bytes;
    let stderr_limit = config.max_stderr_bytes;
    let stdout_reader = thread::spawn(move || read_bounded(stdout, stdout_limit));
    let stderr_reader = thread::spawn(move || read_bounded(stderr, stderr_limit));

    let input_error = child.stdin.take().and_then(|mut stdin| {
        serde_json::to_writer(&mut stdin, &request)
            .map_err(io::Error::other)
            .and_then(|()| stdin.write_all(b"\n"))
            .err()
    });
    if input_error.is_some() {
        let _ = child.kill();
    }

    let pid = Pid::from_u32(child.id());
    let mut system = System::new();
    let mut peak_rss = None::<u64>;
    let mut forced = None::<RunFailureKind>;
    let mut wait_error = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(error) => {
                wait_error = Some(error);
                let _ = child.kill();
                break child.wait().ok();
            }
        }
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]));
        if let Some(process) = system.process(pid) {
            let rss = process.memory();
            peak_rss = Some(peak_rss.map_or(rss, |peak| peak.max(rss)));
            if config.max_rss_bytes.is_some_and(|limit| rss > limit) {
                forced = Some(RunFailureKind::MemoryLimit);
            }
        }
        if started.elapsed() >= config.wall_timeout {
            forced.get_or_insert(RunFailureKind::Timeout);
        }
        if forced.is_some() {
            let _ = child.kill();
            break child.wait().ok();
        }
        thread::sleep(config.poll_interval.max(Duration::from_millis(1)));
    };

    let stdout = stdout_reader
        .join()
        .unwrap_or_else(|_| BoundedRead::failed());
    let stderr = stderr_reader
        .join()
        .unwrap_or_else(|_| BoundedRead::failed());
    let stderr_text = String::from_utf8_lossy(&stderr.bytes).into_owned();
    if let Some(kind) = forced {
        let message = match kind {
            RunFailureKind::Timeout => {
                format!("worker exceeded {:?} wall timeout", config.wall_timeout)
            }
            RunFailureKind::MemoryLimit => format!(
                "worker exceeded {} byte RSS limit",
                config.max_rss_bytes.unwrap_or_default()
            ),
            _ => unreachable!(),
        };
        return failed_outcome(
            request,
            started,
            peak_rss,
            kind,
            message,
            status,
            stderr_text,
        );
    }
    if let Some(error) = wait_error {
        return failed_outcome(
            request,
            started,
            peak_rss,
            RunFailureKind::Crash,
            format!("failed to query worker status: {error}"),
            status,
            stderr_text,
        );
    }
    if let Some(error) = input_error {
        return failed_outcome(
            request,
            started,
            peak_rss,
            RunFailureKind::Crash,
            format!("failed to send request to worker: {error}"),
            status,
            stderr_text,
        );
    }
    let Some(status) = status else {
        return failed_outcome(
            request,
            started,
            peak_rss,
            RunFailureKind::Crash,
            "failed to wait for worker".into(),
            None,
            stderr_text,
        );
    };
    if !status.success() {
        let signal = exit_signal(&status);
        return failed_outcome(
            request,
            started,
            peak_rss,
            if signal.is_some() {
                RunFailureKind::Signal
            } else {
                RunFailureKind::Crash
            },
            if let Some(signal) = signal {
                format!("worker terminated by signal {signal}")
            } else {
                format!("worker exited with status {status}")
            },
            Some(status),
            stderr_text,
        );
    }
    if stdout.truncated || stdout.read_failed {
        return failed_outcome(
            request,
            started,
            peak_rss,
            RunFailureKind::Protocol,
            if stdout.truncated {
                "worker stdout exceeded capture limit".into()
            } else {
                "failed to read worker stdout".into()
            },
            Some(status),
            stderr_text,
        );
    }
    let response = match serde_json::from_slice::<WorkerResponse>(&stdout.bytes) {
        Ok(response) if response_matches(&request, &response) => response,
        Ok(_) => {
            return failed_outcome(
                request,
                started,
                peak_rss,
                RunFailureKind::Protocol,
                "worker response identity or protocol version did not match request".into(),
                Some(status),
                stderr_text,
            );
        }
        Err(error) => {
            return failed_outcome(
                request,
                started,
                peak_rss,
                RunFailureKind::Protocol,
                format!("worker did not emit exactly one valid JSON response: {error}"),
                Some(status),
                stderr_text,
            );
        }
    };
    RunOutcome {
        request,
        response: Ok(response),
        elapsed: started.elapsed(),
        peak_rss_bytes: peak_rss,
        cached: false,
    }
}

fn response_matches(request: &WorkerRequest, response: &WorkerResponse) -> bool {
    response.protocol_version == PROTOCOL_VERSION
        && response.decoded_source.as_ref() == Some(&request.decoded_source)
        && response.parser.as_ref() == Some(&request.parser)
        && response.roxygen == Some(request.roxygen)
}

struct BoundedRead {
    bytes: Vec<u8>,
    truncated: bool,
    read_failed: bool,
}

impl BoundedRead {
    fn failed() -> Self {
        Self {
            bytes: Vec::new(),
            truncated: false,
            read_failed: true,
        }
    }
}

fn read_bounded(mut reader: impl Read, limit: usize) -> BoundedRead {
    let mut output = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let available = limit.saturating_sub(output.len());
                let retained = available.min(read);
                output.extend_from_slice(&buffer[..retained]);
                truncated |= retained != read;
            }
            Err(_) => {
                return BoundedRead {
                    bytes: output,
                    truncated,
                    read_failed: true,
                };
            }
        }
    }
    BoundedRead {
        bytes: output,
        truncated,
        read_failed: false,
    }
}

fn failed_outcome(
    request: WorkerRequest,
    started: Instant,
    peak_rss_bytes: Option<u64>,
    kind: RunFailureKind,
    message: String,
    status: Option<ExitStatus>,
    stderr: String,
) -> RunOutcome {
    RunOutcome {
        request,
        response: Err(RunFailure {
            kind,
            message,
            exit_code: status.as_ref().and_then(ExitStatus::code),
            signal: status.as_ref().and_then(exit_signal),
            stderr,
        }),
        elapsed: started.elapsed(),
        peak_rss_bytes,
        cached: false,
    }
}

#[cfg(unix)]
fn exit_signal(status: &ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_status: &ExitStatus) -> Option<i32> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::worker::{ParserIdentity, SourceIdentity, DEFAULT_PARSER_CONFIG};

    fn request() -> WorkerRequest {
        WorkerRequest::new(
            SourceIdentity {
                path: "unused.r".into(),
                sha256: "0".repeat(64),
            },
            ParserIdentity::new("test", DEFAULT_PARSER_CONFIG),
        )
    }

    #[test]
    fn classifies_nonzero_exit_as_crash() {
        let config = SupervisorConfig::new("/usr/bin/false");
        let outcome = run_one(&config, request());
        assert_eq!(outcome.response.unwrap_err().kind, RunFailureKind::Crash);
    }

    #[test]
    fn classifies_invalid_stdout_as_protocol_failure() {
        let config = SupervisorConfig::new("/usr/bin/true");
        let outcome = run_one(&config, request());
        assert_eq!(outcome.response.unwrap_err().kind, RunFailureKind::Protocol);
    }
}
