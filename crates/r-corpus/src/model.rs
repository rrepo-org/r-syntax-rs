use std::{error::Error, fmt, io, path::PathBuf};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

pub const INVENTORY_VERSION: u32 = 1;
pub const MANIFEST_VERSION: u32 = 1;

pub type Result<T> = std::result::Result<T, CorpusError>;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    Network,
    Protocol,
    Io,
    Database,
    Integrity,
    UnsafeArchive,
    ResourceLimit,
    Decode,
    Format,
}

#[derive(Debug)]
pub struct CorpusError {
    pub class: ErrorClass,
    pub message: String,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl CorpusError {
    pub fn new(class: ErrorClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
            source: None,
        }
    }

    pub fn with_source(
        class: ErrorClass,
        message: impl Into<String>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            class,
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    pub fn failure(&self) -> Failure {
        Failure {
            class: self.class,
            message: self.message.clone(),
        }
    }
}

impl fmt::Display for CorpusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", ErrorClassDisplay(self.class), self.message)
    }
}

impl Error for CorpusError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|error| error as _)
    }
}

struct ErrorClassDisplay(ErrorClass);

impl fmt::Display for ErrorClassDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl From<io::Error> for CorpusError {
    fn from(error: io::Error) -> Self {
        Self::with_source(ErrorClass::Io, error.to_string(), error)
    }
}

impl From<rusqlite::Error> for CorpusError {
    fn from(error: rusqlite::Error) -> Self {
        Self::with_source(ErrorClass::Database, error.to_string(), error)
    }
}

impl From<serde_json::Error> for CorpusError {
    fn from(error: serde_json::Error) -> Self {
        Self::with_source(ErrorClass::Format, error.to_string(), error)
    }
}

impl From<reqwest::Error> for CorpusError {
    fn from(error: reqwest::Error) -> Self {
        Self::with_source(ErrorClass::Network, error.to_string(), error)
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value))
        } else {
            Err(CorpusError::new(
                ErrorClass::Format,
                "SHA-256 digest must be 64 lowercase hexadecimal characters",
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InventorySnapshot {
    pub schema_version: u32,
    pub created_unix_seconds: u64,
    pub repository: String,
    pub packages: Vec<InventoryPackage>,
}

impl InventorySnapshot {
    pub fn new(repository: impl Into<String>, created_unix_seconds: u64) -> Self {
        Self {
            schema_version: INVENTORY_VERSION,
            created_unix_seconds,
            repository: repository.into(),
            packages: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InventoryPackage {
    pub name: String,
    pub versions: Vec<InventoryVersion>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InventoryVersion {
    pub version: String,
    pub archive_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authoritative_sha256: Option<Sha256Digest>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Failure {
    pub class: ErrorClass,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AcquisitionTerminalState {
    Collected {
        archive_sha256: Sha256Digest,
        /// Every selected R source, including terminal decode failures.
        source_count: u64,
        decoded_source_count: u64,
    },
    Rejected {
        failure: Failure,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed_archive_sha256: Option<Sha256Digest>,
    },
    Failed {
        failure: Failure,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed_archive_sha256: Option<Sha256Digest>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AcquisitionRecord {
    pub package: String,
    pub version: String,
    pub archive_url: String,
    pub terminal: AcquisitionTerminalState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceOccurrence {
    pub package: String,
    pub version: String,
    pub archive_sha256: Sha256Digest,
    pub archive_path: String,
    pub declared_encoding: String,
    pub decoded_encoding: SourceEncoding,
    pub raw_sha256: Sha256Digest,
    pub decoded_sha256: Sha256Digest,
    pub raw_bytes: u64,
    pub decoded_bytes: u64,
    #[serde(default)]
    pub curated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceFailureRecord {
    pub package: String,
    pub version: String,
    pub archive_sha256: Sha256Digest,
    pub archive_path: String,
    pub declared_encoding: String,
    pub raw_sha256: Sha256Digest,
    pub raw_bytes: u64,
    pub failure: Failure,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceEncoding {
    Utf8,
    Latin1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FinalManifest {
    pub schema_version: u32,
    pub inventory_sha256: Sha256Digest,
    pub acquisitions: Vec<AcquisitionRecord>,
    pub sources: Vec<SourceOccurrence>,
    pub source_failures: Vec<SourceFailureRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<ManifestArchive>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ManifestArchive {
    pub sha256: Sha256Digest,
    pub shard: String,
}

impl FinalManifest {
    pub fn new(inventory_sha256: Sha256Digest) -> Self {
        Self {
            schema_version: MANIFEST_VERSION,
            inventory_sha256,
            acquisitions: Vec::new(),
            sources: Vec::new(),
            source_failures: Vec::new(),
            archive: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct StorePaths {
    pub root: PathBuf,
    pub database: PathBuf,
    pub cas: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_validation_is_strict() {
        assert!(Sha256Digest::parse("a".repeat(64)).is_ok());
        assert!(Sha256Digest::parse("A".repeat(64)).is_err());
        assert!(Sha256Digest::parse("a".repeat(63)).is_err());
    }
}
