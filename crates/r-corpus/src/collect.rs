use std::{
    collections::HashSet,
    fs,
    io::{Read, Seek, Write},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use flate2::read::GzDecoder;
use reqwest::blocking::{Client, Response};
use reqwest::Url;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::{
    model::{
        AcquisitionRecord, AcquisitionTerminalState, CorpusError, ErrorClass, FinalManifest,
        InventoryPackage, InventorySnapshot, InventoryVersion, Result, Sha256Digest,
        SourceEncoding, SourceOccurrence,
    },
    store::CorpusStore,
};

#[derive(Clone, Debug)]
pub struct RRepoConfig {
    pub base_url: String,
    pub concurrency: usize,
    pub timeout: Duration,
    pub token: Option<String>,
}

impl RRepoConfig {
    pub fn from_env(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            concurrency: 8,
            timeout: Duration::from_secs(60),
            token: std::env::var("RREPO_TOKEN").ok(),
        }
    }
}

#[derive(Clone)]
pub struct RRepoClient {
    client: Client,
    config: RRepoConfig,
    origin: Url,
}

impl RRepoClient {
    pub fn new(mut config: RRepoConfig) -> Result<Self> {
        config.base_url = config.base_url.trim_end_matches('/').to_owned();
        if config.concurrency == 0 {
            return Err(CorpusError::new(
                ErrorClass::ResourceLimit,
                "HTTP concurrency must be at least one",
            ));
        }
        let origin = Url::parse(&config.base_url).map_err(|error| {
            CorpusError::with_source(ErrorClass::Protocol, "invalid repository URL", error)
        })?;
        if !matches!(origin.scheme(), "http" | "https") || origin.host_str().is_none() {
            return Err(CorpusError::new(
                ErrorClass::Protocol,
                "repository URL must be HTTP(S) with a host",
            ));
        }
        let client = Client::builder()
            .timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            config,
            origin,
        })
    }

    pub fn fetch_inventory(&self) -> Result<InventorySnapshot> {
        let packages_value = self.get_json(&format!("{}/packages", self.config.base_url))?;
        let mut names = package_names(&packages_value)?;
        names.sort();
        names.dedup();
        for name in &names {
            validate_package_name(name)?;
        }

        let next = AtomicUsize::new(0);
        let results: Mutex<Vec<Option<Result<InventoryPackage>>>> =
            Mutex::new((0..names.len()).map(|_| None).collect());
        let workers = self.config.concurrency.min(names.len().max(1));
        thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= names.len() {
                        break;
                    }
                    let name = &names[index];
                    let result = self.fetch_package(name);
                    results.lock().expect("inventory mutex poisoned")[index] = Some(result);
                });
            }
        });

        let mut packages = Vec::with_capacity(names.len());
        for result in results.into_inner().expect("inventory mutex poisoned") {
            packages.push(result.expect("each inventory job completes")?);
        }
        packages.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(InventorySnapshot {
            schema_version: crate::model::INVENTORY_VERSION,
            created_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| {
                    CorpusError::with_source(ErrorClass::Io, "system clock predates epoch", error)
                })?
                .as_secs(),
            repository: self.config.base_url.clone(),
            packages,
        })
    }

    fn fetch_package(&self, name: &str) -> Result<InventoryPackage> {
        let endpoint = format!("{}/packages/{name}/versions", self.config.base_url);
        let value = self.get_json(&endpoint)?;
        let mut versions = package_versions(&value)?;
        versions.sort_by(|left, right| {
            (&left.version, &left.archive_url).cmp(&(&right.version, &right.archive_url))
        });
        versions.dedup_by(|left, right| left.version == right.version);
        Ok(InventoryPackage {
            name: name.to_owned(),
            versions,
        })
    }

    fn get_json(&self, url: &str) -> Result<Value> {
        let url = self.checked_url(url)?;
        let mut request = self.client.get(url);
        if let Some(token) = &self.config.token {
            request = request.bearer_auth(token);
        }
        let response = request.send()?.error_for_status()?;
        response.json().map_err(Into::into)
    }

    fn get(&self, url: &str) -> Result<Response> {
        let url = self.checked_url(url)?;
        let mut request = self.client.get(url);
        if let Some(token) = &self.config.token {
            request = request.bearer_auth(token);
        }
        Ok(request.send()?.error_for_status()?)
    }

    fn checked_url(&self, value: &str) -> Result<Url> {
        let url = Url::parse(value).map_err(|error| {
            CorpusError::with_source(ErrorClass::Protocol, "invalid rrepo response URL", error)
        })?;
        if url.scheme() != self.origin.scheme()
            || url.host_str() != self.origin.host_str()
            || url.port_or_known_default() != self.origin.port_or_known_default()
        {
            return Err(CorpusError::new(
                ErrorClass::Protocol,
                format!("rrepo URL escaped configured repository origin: {url}"),
            ));
        }
        Ok(url)
    }
}

fn package_names(value: &Value) -> Result<Vec<String>> {
    let array = value
        .as_array()
        .or_else(|| value.get("packages").and_then(Value::as_array))
        .ok_or_else(|| CorpusError::new(ErrorClass::Protocol, "/packages is not an array"))?;
    array
        .iter()
        .map(|item| {
            item.as_str()
                .or_else(|| item.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .ok_or_else(|| CorpusError::new(ErrorClass::Protocol, "package has no string name"))
        })
        .collect()
}

fn package_versions(value: &Value) -> Result<Vec<InventoryVersion>> {
    let array = value
        .as_array()
        .or_else(|| value.get("versions").and_then(Value::as_array))
        .ok_or_else(|| CorpusError::new(ErrorClass::Protocol, "/versions is not an array"))?;
    array
        .iter()
        .map(|item| {
            let version = item.get("version").and_then(Value::as_str).ok_or_else(|| {
                CorpusError::new(ErrorClass::Protocol, "version has no version string")
            })?;
            let archive_url = item
                .get("sourceUrl")
                .or_else(|| item.get("archive_url"))
                .or_else(|| item.get("url"))
                .or_else(|| item.get("download_url"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    CorpusError::new(ErrorClass::Protocol, "version has no archive URL")
                })?;
            let digest = item
                .get("sha256")
                .or_else(|| item.get("digest"))
                .and_then(Value::as_str)
                .map(|value| value.strip_prefix("sha256:").unwrap_or(value))
                .map(|value| Sha256Digest::parse(value.to_owned()))
                .transpose()?;
            Ok(InventoryVersion {
                version: version.to_owned(),
                archive_url: archive_url.to_owned(),
                authoritative_sha256: digest,
            })
        })
        .collect()
}

fn validate_package_name(name: &str) -> Result<()> {
    if !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_' || byte == b'-'
        })
    {
        Ok(())
    } else {
        Err(CorpusError::new(
            ErrorClass::Protocol,
            format!("unsafe package name {name:?}"),
        ))
    }
}

#[derive(Clone, Debug)]
pub struct CollectionLimits {
    pub max_compressed_bytes: u64,
    pub max_entries: u64,
    pub max_entry_bytes: u64,
    pub max_total_uncompressed_bytes: u64,
    pub max_source_bytes: usize,
    pub max_decoded_bytes: usize,
}

impl Default for CollectionLimits {
    fn default() -> Self {
        Self {
            max_compressed_bytes: 256 * 1024 * 1024,
            max_entries: 100_000,
            max_entry_bytes: 64 * 1024 * 1024,
            max_total_uncompressed_bytes: 1024 * 1024 * 1024,
            max_source_bytes: 16 * 1024 * 1024,
            max_decoded_bytes: 32 * 1024 * 1024,
        }
    }
}

pub struct Collector {
    repository: RRepoClient,
    temporary_directory: PathBuf,
    pub limits: CollectionLimits,
}

impl Collector {
    pub fn new(
        repository: RRepoClient,
        temporary_directory: impl Into<PathBuf>,
        limits: CollectionLimits,
    ) -> Result<Self> {
        let temporary_directory = temporary_directory.into();
        fs::create_dir_all(&temporary_directory)?;
        Ok(Self {
            repository,
            temporary_directory,
            limits,
        })
    }

    /// Collects every immutable inventory item and returns a complete manifest;
    /// each attempted item is persisted in exactly one terminal state.
    pub fn collect_snapshot(
        &self,
        snapshot: &InventorySnapshot,
        inventory_sha256: Sha256Digest,
        store: &mut CorpusStore,
    ) -> Result<FinalManifest> {
        let mut manifest = FinalManifest::new(inventory_sha256);
        for package in &snapshot.packages {
            for version in &package.versions {
                if let Some((record, sources, source_failures)) =
                    store.completed_acquisition(&package.name, &version.version)?
                {
                    if record.archive_url != version.archive_url {
                        return Err(CorpusError::new(
                            ErrorClass::Integrity,
                            format!(
                                "archive URL changed for {} {}",
                                package.name, version.version
                            ),
                        ));
                    }
                    let reusable =
                        !matches!(record.terminal, AcquisitionTerminalState::Failed { .. });
                    if reusable {
                        if let Some(expected) = &version.authoritative_sha256 {
                            let observed = match &record.terminal {
                                AcquisitionTerminalState::Collected { archive_sha256, .. } => {
                                    Some(archive_sha256)
                                }
                                AcquisitionTerminalState::Rejected {
                                    observed_archive_sha256,
                                    ..
                                } => observed_archive_sha256.as_ref(),
                                AcquisitionTerminalState::Failed { .. } => None,
                            };
                            if observed.is_some_and(|digest| digest != expected) {
                                return Err(CorpusError::new(
                                    ErrorClass::Integrity,
                                    format!(
                                        "stored archive digest disagrees with inventory for {} {}",
                                        package.name, version.version
                                    ),
                                ));
                            }
                        }
                        manifest.sources.extend(sources);
                        manifest.source_failures.extend(source_failures);
                        manifest.acquisitions.push(record);
                        continue;
                    }
                }
                let (record, sources, source_failures) =
                    self.collect_one(&package.name, version, store);
                store.finish_acquisition(&record, &sources, &source_failures)?;
                manifest.sources.extend(sources);
                manifest.source_failures.extend(source_failures);
                manifest.acquisitions.push(record);
            }
        }
        Ok(manifest)
    }

    pub fn collect_one(
        &self,
        package: &str,
        version: &InventoryVersion,
        store: &CorpusStore,
    ) -> (
        AcquisitionRecord,
        Vec<SourceOccurrence>,
        Vec<crate::model::SourceFailureRecord>,
    ) {
        let mut observed = None;
        let attempt = self.collect_one_inner(package, version, store, &mut observed);
        match attempt {
            Ok((sources, source_failures)) => (
                AcquisitionRecord {
                    package: package.to_owned(),
                    version: version.version.clone(),
                    archive_url: version.archive_url.clone(),
                    terminal: AcquisitionTerminalState::Collected {
                        archive_sha256: observed.expect("successful download has a digest"),
                        source_count: (sources.len() + source_failures.len()) as u64,
                        decoded_source_count: sources.len() as u64,
                    },
                },
                sources,
                source_failures,
            ),
            Err(error) => {
                let terminal = if matches!(
                    error.class,
                    ErrorClass::Integrity
                        | ErrorClass::UnsafeArchive
                        | ErrorClass::ResourceLimit
                        | ErrorClass::Decode
                ) {
                    AcquisitionTerminalState::Rejected {
                        failure: error.failure(),
                        observed_archive_sha256: observed,
                    }
                } else {
                    AcquisitionTerminalState::Failed {
                        failure: error.failure(),
                        observed_archive_sha256: observed,
                    }
                };
                (
                    AcquisitionRecord {
                        package: package.to_owned(),
                        version: version.version.clone(),
                        archive_url: version.archive_url.clone(),
                        terminal,
                    },
                    Vec::new(),
                    Vec::new(),
                )
            }
        }
    }

    fn collect_one_inner(
        &self,
        package: &str,
        version: &InventoryVersion,
        store: &CorpusStore,
        observed_out: &mut Option<Sha256Digest>,
    ) -> Result<(
        Vec<SourceOccurrence>,
        Vec<crate::model::SourceFailureRecord>,
    )> {
        validate_package_name(package)?;
        let response = self.repository.get(&version.archive_url)?;
        let (mut temporary, observed) = download_once(
            response,
            &self.temporary_directory,
            self.limits.max_compressed_bytes,
        )?;
        *observed_out = Some(observed.clone());

        if let Some(authoritative) = &version.authoritative_sha256 {
            if authoritative != &observed {
                return Err(CorpusError::new(
                    ErrorClass::Integrity,
                    format!("repository digest {authoritative} does not match archive {observed}"),
                ));
            }
        }
        if let Some(previous) = store.observed_archive_digest(package, &version.version)? {
            if previous != observed {
                return Err(CorpusError::new(
                    ErrorClass::Integrity,
                    format!("archive changed from {previous} to {observed}"),
                ));
            }
        }
        // Upstream rrepo should supply an authoritative digest; this TOFU pin is temporary.
        store.record_observed_archive(package, &version.version, &observed)?;

        temporary.as_file_mut().rewind()?;
        let (cas_archive, _) = store.cas().put_reader(temporary.as_file_mut())?;
        if cas_archive != observed {
            return Err(CorpusError::new(
                ErrorClass::Integrity,
                "archive changed while being copied into CAS",
            ));
        }
        temporary.as_file_mut().rewind()?;
        let scanned = scan_archive(temporary.as_file_mut(), &self.limits)?;
        materialize_sources(package, version, &observed, scanned, store, &self.limits)
    }
}

fn download_once(
    mut response: Response,
    directory: &Path,
    limit: u64,
) -> Result<(NamedTempFile, Sha256Digest)> {
    let expected_length = response.content_length();
    if expected_length.is_some_and(|length| length > limit) {
        return Err(CorpusError::new(
            ErrorClass::ResourceLimit,
            format!("archive Content-Length exceeds {limit} bytes"),
        ));
    }
    let mut temporary = NamedTempFile::new_in(directory)?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| CorpusError::new(ErrorClass::ResourceLimit, "archive size overflow"))?;
        if total > limit {
            return Err(CorpusError::new(
                ErrorClass::ResourceLimit,
                format!("archive exceeds {limit} bytes"),
            ));
        }
        temporary.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
    }
    if expected_length.is_some_and(|length| length != total) {
        return Err(CorpusError::new(
            ErrorClass::Integrity,
            format!(
                "archive Content-Length was {}, but the response contained {total} bytes",
                expected_length.unwrap_or_default()
            ),
        ));
    }
    temporary.as_file_mut().sync_all()?;
    let digest = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok((temporary, Sha256Digest::parse(digest)?))
}

#[derive(Debug)]
struct ScannedArchive {
    description: Vec<u8>,
    candidates: Vec<(String, Vec<u8>)>,
}

fn scan_archive(reader: impl Read, limits: &CollectionLimits) -> Result<ScannedArchive> {
    let decoder = GzDecoder::new(reader);
    let mut archive = tar::Archive::new(decoder);
    let mut seen = HashSet::new();
    let mut description = None;
    let mut candidates = Vec::new();
    let mut count = 0_u64;
    let mut total = 0_u64;
    for entry in archive.entries().map_err(archive_error)? {
        let mut entry = entry.map_err(archive_error)?;
        count += 1;
        if count > limits.max_entries {
            return Err(limit_error("archive entry count", limits.max_entries));
        }
        let entry_type = entry.header().entry_type();
        if !(entry_type.is_file() || entry_type.is_dir()) {
            return Err(CorpusError::new(
                ErrorClass::UnsafeArchive,
                "archive contains a link, device, or other special entry",
            ));
        }
        let path = entry.path().map_err(archive_error)?.into_owned();
        let normalized = normalized_archive_path(&path)?;
        if !seen.insert(normalized.clone()) {
            return Err(CorpusError::new(
                ErrorClass::UnsafeArchive,
                format!("duplicate archive path {normalized:?}"),
            ));
        }
        let size = entry.header().size().map_err(archive_error)?;
        if size > limits.max_entry_bytes {
            return Err(limit_error("archive entry size", limits.max_entry_bytes));
        }
        total = total.checked_add(size).ok_or_else(|| {
            CorpusError::new(ErrorClass::ResourceLimit, "uncompressed size overflow")
        })?;
        if total > limits.max_total_uncompressed_bytes {
            return Err(limit_error(
                "total uncompressed size",
                limits.max_total_uncompressed_bytes,
            ));
        }
        if entry_type.is_dir() {
            continue;
        }
        let components: Vec<_> = path
            .components()
            .filter_map(|component| match component {
                Component::Normal(value) => value.to_str(),
                _ => None,
            })
            .collect();
        let is_description = components.len() == 2 && components[1] == "DESCRIPTION";
        let is_candidate =
            components.len() >= 3 && components[1] == "R" && is_r_source_extension(&path);
        if is_description || is_candidate {
            let specific_limit = if is_candidate {
                limits.max_source_bytes as u64
            } else {
                limits.max_entry_bytes
            };
            if size > specific_limit {
                return Err(limit_error("selected file size", specific_limit));
            }
            let mut bytes = Vec::with_capacity(size as usize);
            entry.read_to_end(&mut bytes).map_err(archive_error)?;
            if bytes.len() as u64 != size {
                return Err(CorpusError::new(
                    ErrorClass::UnsafeArchive,
                    format!("archive entry {normalized:?} has an inconsistent size"),
                ));
            }
            if is_description {
                if description
                    .replace((components[0].to_owned(), bytes))
                    .is_some()
                {
                    return Err(CorpusError::new(
                        ErrorClass::UnsafeArchive,
                        "archive contains multiple top-level DESCRIPTION files",
                    ));
                }
            } else {
                candidates.push((normalized, bytes));
            }
        }
    }
    let (root, description) = description.ok_or_else(|| {
        CorpusError::new(
            ErrorClass::UnsafeArchive,
            "archive has no top-level package DESCRIPTION",
        )
    })?;
    candidates.retain(|(path, _)| path.starts_with(&format!("{root}/R/")));
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(ScannedArchive {
        description,
        candidates,
    })
}

fn normalized_archive_path(path: &Path) -> Result<String> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || path.to_string_lossy().contains('\\')
    {
        return Err(CorpusError::new(
            ErrorClass::UnsafeArchive,
            format!("unsafe archive path {:?}", path),
        ));
    }
    path.components()
        .map(|component| match component {
            Component::Normal(value) => value.to_str().ok_or_else(|| {
                CorpusError::new(ErrorClass::UnsafeArchive, "archive path is not UTF-8")
            }),
            _ => unreachable!("components were validated above"),
        })
        .collect::<Result<Vec<_>>>()
        .map(|components| components.join("/"))
}

fn is_r_source_extension(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("R" | "r" | "S" | "s" | "q")
    )
}

fn archive_error(error: std::io::Error) -> CorpusError {
    CorpusError::with_source(ErrorClass::UnsafeArchive, error.to_string(), error)
}

fn limit_error(what: &str, limit: u64) -> CorpusError {
    CorpusError::new(
        ErrorClass::ResourceLimit,
        format!("{what} exceeds limit {limit}"),
    )
}

fn materialize_sources(
    package: &str,
    version: &InventoryVersion,
    archive_sha256: &Sha256Digest,
    scanned: ScannedArchive,
    store: &CorpusStore,
    limits: &CollectionLimits,
) -> Result<(
    Vec<SourceOccurrence>,
    Vec<crate::model::SourceFailureRecord>,
)> {
    let declared =
        description_encoding(&scanned.description).unwrap_or_else(|_| "<invalid>".to_owned());
    let decoding = match declared.to_ascii_lowercase().as_str() {
        "utf-8" | "utf8" | "ascii" => {
            Some((SourceEncoding::Utf8, r_source::EncodingProfile::UTF8_STRICT))
        }
        "latin1" | "latin-1" | "iso-8859-1" => {
            Some((SourceEncoding::Latin1, r_source::EncodingProfile::LATIN1))
        }
        _ => None,
    };
    let mut occurrences = Vec::with_capacity(scanned.candidates.len());
    let mut failures = Vec::new();
    for (path, raw) in scanned.candidates {
        let raw_sha256 = store.cas().put_bytes(&raw)?;
        let Some((encoding, profile)) = decoding else {
            failures.push(crate::model::SourceFailureRecord {
                package: package.to_owned(),
                version: version.version.clone(),
                archive_sha256: archive_sha256.clone(),
                archive_path: path,
                declared_encoding: declared.clone(),
                raw_sha256,
                raw_bytes: raw.len() as u64,
                failure: crate::model::Failure {
                    class: ErrorClass::Decode,
                    message: format!("unsupported DESCRIPTION Encoding {declared:?}"),
                },
            });
            continue;
        };
        let decoded = match r_source::decode(raw.clone(), profile, limits.max_decoded_bytes) {
            Ok(decoded) => decoded,
            Err(error) => {
                failures.push(crate::model::SourceFailureRecord {
                    package: package.to_owned(),
                    version: version.version.clone(),
                    archive_sha256: archive_sha256.clone(),
                    archive_path: path,
                    declared_encoding: declared.clone(),
                    raw_sha256,
                    raw_bytes: raw.len() as u64,
                    failure: crate::model::Failure {
                        class: ErrorClass::Decode,
                        message: error.to_string(),
                    },
                });
                continue;
            }
        };
        let decoded_sha256 = store.cas().put_bytes(decoded.text().as_bytes())?;
        occurrences.push(SourceOccurrence {
            package: package.to_owned(),
            version: version.version.clone(),
            archive_sha256: archive_sha256.clone(),
            archive_path: path,
            declared_encoding: declared.clone(),
            decoded_encoding: encoding,
            raw_sha256,
            decoded_sha256,
            raw_bytes: raw.len() as u64,
            decoded_bytes: decoded.text().len() as u64,
            curated: false,
        });
    }
    Ok((occurrences, failures))
}

fn description_encoding(bytes: &[u8]) -> Result<String> {
    // DESCRIPTION field names and Encoding values are ASCII even when package
    // source files use Latin-1.
    for line in bytes.split(|byte| *byte == b'\n') {
        if let Some(colon) = line.iter().position(|byte| *byte == b':') {
            let name = &line[..colon];
            if name.eq_ignore_ascii_case(b"Encoding") {
                let value = line[colon + 1..]
                    .strip_suffix(b"\r")
                    .unwrap_or(&line[colon + 1..]);
                let value = trim_ascii(value);
                if value.is_empty() {
                    return Err(CorpusError::new(
                        ErrorClass::Decode,
                        "DESCRIPTION Encoding is empty",
                    ));
                }
                let value = std::str::from_utf8(value).map_err(|error| {
                    CorpusError::with_source(
                        ErrorClass::Decode,
                        "DESCRIPTION Encoding is not ASCII",
                        error,
                    )
                })?;
                return Ok(value.to_owned());
            }
        }
    }
    Ok("UTF-8".to_owned())
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use std::io::Cursor;

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut compressed = Vec::new();
        {
            let encoder = GzEncoder::new(&mut compressed, Compression::default());
            let mut builder = tar::Builder::new(encoder);
            for (path, bytes) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, path, *bytes).unwrap();
            }
            builder.into_inner().unwrap().finish().unwrap();
        }
        compressed
    }

    #[test]
    fn source_selection_is_case_explicit() {
        assert!(is_r_source_extension(Path::new("pkg/R/a.R")));
        assert!(is_r_source_extension(Path::new("pkg/R/a.q")));
        assert!(!is_r_source_extension(Path::new("pkg/R/a.txt")));
        assert!(!is_r_source_extension(Path::new("pkg/R/a.Q")));
    }

    #[test]
    fn traversal_is_unsafe() {
        assert!(normalized_archive_path(Path::new("../outside")).is_err());
        assert!(normalized_archive_path(Path::new("pkg/R/a.R")).is_ok());
    }

    #[test]
    fn description_defaults_to_utf8() {
        assert_eq!(description_encoding(b"Package: x\n").unwrap(), "UTF-8");
        assert_eq!(
            description_encoding(b"Package: x\nEncoding: latin1\n").unwrap(),
            "latin1"
        );
    }

    #[test]
    fn parses_live_rrepo_source_url_shape() {
        let value = serde_json::json!({
            "versions": [{
                "version": "1.2.3",
                "sourceUrl": "https://example.test/cran/packages/pkg/versions/1.2.3/source"
            }]
        });
        let versions = package_versions(&value).unwrap();
        assert_eq!(versions[0].version, "1.2.3");
        assert!(versions[0].archive_url.ends_with("/source"));
    }

    #[test]
    fn repository_client_rejects_cross_origin_urls() {
        let client = RRepoClient::new(RRepoConfig::from_env("https://example.test/cran")).unwrap();
        assert!(client
            .checked_url("https://attacker.test/archive.tar.gz")
            .is_err());
        assert!(client
            .checked_url("https://example.test/cran/packages")
            .is_ok());
    }

    #[test]
    fn scans_only_selected_package_root_sources() {
        let bytes = archive(&[
            ("pkg/DESCRIPTION", b"Package: pkg\nEncoding: UTF-8\n"),
            ("pkg/R/a.R", b"x <- 1\n"),
            ("pkg/R/unix/b.s", b"y <- 2\n"),
            ("other/R/ignored.R", b"stop()\n"),
            ("pkg/tests/ignored.R", b"stop()\n"),
        ]);
        let scanned = scan_archive(Cursor::new(bytes), &CollectionLimits::default()).unwrap();
        assert_eq!(scanned.candidates.len(), 2);
        assert_eq!(scanned.candidates[0].0, "pkg/R/a.R");
        assert_eq!(scanned.candidates[1].0, "pkg/R/unix/b.s");
    }

    #[test]
    fn rejects_special_entries_and_archive_limits() {
        let mut compressed = Vec::new();
        {
            let encoder = GzEncoder::new(&mut compressed, Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_link_name("../../outside").unwrap();
            header.set_cksum();
            builder
                .append_data(&mut header, "pkg/R/link.R", &[][..])
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let error =
            scan_archive(Cursor::new(compressed), &CollectionLimits::default()).unwrap_err();
        assert_eq!(error.class, ErrorClass::UnsafeArchive);

        let bytes = archive(&[("pkg/DESCRIPTION", b"Package: pkg\n"), ("pkg/R/a.R", b"x")]);
        let limits = CollectionLimits {
            max_entries: 1,
            ..CollectionLimits::default()
        };
        assert_eq!(
            scan_archive(Cursor::new(bytes), &limits).unwrap_err().class,
            ErrorClass::ResourceLimit
        );
    }

    #[test]
    fn decoding_failures_are_terminal_source_records() {
        let directory = tempfile::tempdir().unwrap();
        let store = CorpusStore::open(
            directory.path().join("index.sqlite3"),
            directory.path().join("objects"),
        )
        .unwrap();
        let scanned = ScannedArchive {
            description: b"Package: pkg\nEncoding: UTF-8\n".to_vec(),
            candidates: vec![("pkg/R/bad.R".into(), vec![0xff])],
        };
        let version = InventoryVersion {
            version: "1.0".into(),
            archive_url: "https://example.test/source".into(),
            authoritative_sha256: None,
        };
        let archive_sha256 = Sha256Digest::parse("a".repeat(64)).unwrap();
        let (sources, failures) = materialize_sources(
            "pkg",
            &version,
            &archive_sha256,
            scanned,
            &store,
            &CollectionLimits::default(),
        )
        .unwrap();
        assert!(sources.is_empty());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].failure.class, ErrorClass::Decode);
        assert_eq!(store.cas().verify(&failures[0].raw_sha256).unwrap(), 1);
    }
}
