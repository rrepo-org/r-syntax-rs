use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::model::{
    AcquisitionRecord, AcquisitionTerminalState, CorpusError, ErrorClass, Failure, Result,
    Sha256Digest, SourceEncoding, SourceFailureRecord, SourceOccurrence,
};

pub fn sha256_bytes(bytes: &[u8]) -> Sha256Digest {
    digest_from_hasher(Sha256::digest(bytes))
}

pub fn sha256_reader(mut reader: impl Read) -> Result<(Sha256Digest, u64)> {
    let mut hasher = Sha256::new();
    let bytes = io::copy(&mut reader, &mut HashWriter(&mut hasher))?;
    Ok((digest_from_hasher(hasher.finalize()), bytes))
}

fn digest_from_hasher(bytes: impl AsRef<[u8]>) -> Sha256Digest {
    let value = bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Sha256Digest::parse(value).expect("SHA-256 always has a valid representation")
}

struct HashWriter<'a>(&'a mut Sha256);

impl Write for HashWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn shard_for_digest(digest: &Sha256Digest) -> String {
    format!("{}/{}", &digest.as_str()[0..2], &digest.as_str()[2..4])
}

/// Assigns an identity to a stable shard without relying on randomized hashes.
pub fn stable_shard(identity: &[u8], shard_count: u32) -> Result<u32> {
    if shard_count == 0 {
        return Err(CorpusError::new(
            ErrorClass::ResourceLimit,
            "shard count must be at least one",
        ));
    }
    let digest = Sha256::digest(identity);
    let prefix = u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 contains an eight-byte prefix"),
    );
    Ok((prefix % u64::from(shard_count)) as u32)
}

pub struct Cas {
    root: PathBuf,
}

impl Cas {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, digest: &Sha256Digest) -> PathBuf {
        self.root
            .join(&digest.as_str()[0..2])
            .join(&digest.as_str()[2..4])
            .join(digest.as_str())
    }

    pub fn put_bytes(&self, bytes: &[u8]) -> Result<Sha256Digest> {
        let digest = sha256_bytes(bytes);
        self.put_verified(&digest, bytes)?;
        Ok(digest)
    }

    pub fn put_reader(&self, mut reader: impl Read) -> Result<(Sha256Digest, u64)> {
        let mut temporary = NamedTempFile::new_in(&self.root)?;
        let mut hasher = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            temporary.write_all(&buffer[..read])?;
            hasher.update(&buffer[..read]);
            total = total.checked_add(read as u64).ok_or_else(|| {
                CorpusError::new(ErrorClass::ResourceLimit, "CAS object size overflow")
            })?;
        }
        temporary.as_file_mut().sync_all()?;
        let digest = digest_from_hasher(hasher.finalize());
        self.install_temp(temporary, &digest)?;
        Ok((digest, total))
    }

    pub fn put_verified(&self, expected: &Sha256Digest, bytes: &[u8]) -> Result<()> {
        let observed = sha256_bytes(bytes);
        if &observed != expected {
            return Err(CorpusError::new(
                ErrorClass::Integrity,
                format!("expected {expected}, observed {observed}"),
            ));
        }
        let mut temporary = NamedTempFile::new_in(&self.root)?;
        temporary.write_all(bytes)?;
        temporary.as_file_mut().sync_all()?;
        self.install_temp(temporary, expected)
    }

    fn install_temp(&self, temporary: NamedTempFile, digest: &Sha256Digest) -> Result<()> {
        let destination = self.path(digest);
        let parent = destination.parent().expect("CAS path has a parent");
        fs::create_dir_all(parent)?;
        if destination.exists() {
            self.verify(digest)?;
            return Ok(());
        }
        match temporary.persist_noclobber(&destination) {
            Ok(file) => file.sync_all()?,
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                self.verify(digest)?;
            }
            Err(error) => return Err(error.error.into()),
        }
        sync_directory(parent)?;
        Ok(())
    }

    pub fn verify(&self, expected: &Sha256Digest) -> Result<u64> {
        let file = File::open(self.path(expected))?;
        let (observed, bytes) = sha256_reader(file)?;
        if &observed != expected {
            return Err(CorpusError::new(
                ErrorClass::Integrity,
                format!("CAS object {expected} contains {observed}"),
            ));
        }
        Ok(bytes)
    }

    pub fn open_object(&self, digest: &Sha256Digest) -> Result<File> {
        self.verify(digest)?;
        Ok(File::open(self.path(digest))?)
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

pub struct CorpusStore {
    connection: Connection,
    cas: Cas,
}

pub type CompletedAcquisition = (
    AcquisitionRecord,
    Vec<SourceOccurrence>,
    Vec<SourceFailureRecord>,
);

impl CorpusStore {
    pub fn open(database: impl AsRef<Path>, cas_root: impl Into<PathBuf>) -> Result<Self> {
        if let Some(parent) = database.as_ref().parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(database)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.execute_batch(SCHEMA)?;
        let has_curated = {
            let mut statement = connection.prepare("PRAGMA table_info(source_occurrences)")?;
            let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
            let mut found = false;
            for column in columns {
                found |= column? == "curated";
            }
            found
        };
        if !has_curated {
            connection.execute(
                "ALTER TABLE source_occurrences ADD COLUMN curated INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        Ok(Self {
            connection,
            cas: Cas::open(cas_root)?,
        })
    }

    pub fn cas(&self) -> &Cas {
        &self.cas
    }

    pub fn observed_archive_digest(
        &self,
        package: &str,
        version: &str,
    ) -> Result<Option<Sha256Digest>> {
        let value = self
            .connection
            .query_row(
                "SELECT archive_sha256 FROM observed_archives WHERE package=?1 AND version=?2",
                params![package, version],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        value.map(Sha256Digest::parse).transpose()
    }

    pub fn record_observed_archive(
        &self,
        package: &str,
        version: &str,
        digest: &Sha256Digest,
    ) -> Result<()> {
        let changed = self.connection.execute(
            "INSERT INTO observed_archives(package,version,archive_sha256) VALUES(?1,?2,?3) \
             ON CONFLICT(package,version) DO UPDATE SET archive_sha256=excluded.archive_sha256 \
             WHERE observed_archives.archive_sha256=excluded.archive_sha256",
            params![package, version, digest.as_str()],
        )?;
        if changed == 0 {
            return Err(CorpusError::new(
                ErrorClass::Integrity,
                format!("archive digest changed for {package} {version}"),
            ));
        }
        Ok(())
    }

    pub fn finish_acquisition(
        &mut self,
        record: &AcquisitionRecord,
        sources: &[SourceOccurrence],
        source_failures: &[SourceFailureRecord],
    ) -> Result<()> {
        let transaction = self.connection.transaction()?;
        store_terminal(&transaction, record)?;
        transaction.execute(
            "DELETE FROM source_occurrences WHERE package=?1 AND version=?2",
            params![record.package, record.version],
        )?;
        for source in sources {
            transaction.execute(
                "INSERT INTO source_occurrences(package,version,archive_sha256,archive_path,declared_encoding,decoded_encoding,raw_sha256,decoded_sha256,raw_bytes,decoded_bytes,curated) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    source.package,
                    source.version,
                    source.archive_sha256.as_str(),
                    source.archive_path,
                    source.declared_encoding,
                    serde_json::to_string(&source.decoded_encoding)?,
                    source.raw_sha256.as_str(),
                    source.decoded_sha256.as_str(),
                    source.raw_bytes,
                    source.decoded_bytes,
                    source.curated,
                ],
            )?;
        }
        transaction.execute(
            "DELETE FROM source_failures WHERE package=?1 AND version=?2",
            params![record.package, record.version],
        )?;
        for source in source_failures {
            transaction.execute(
                "INSERT INTO source_failures(package,version,archive_sha256,archive_path,declared_encoding,raw_sha256,raw_bytes,error_class,error_message) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    source.package,
                    source.version,
                    source.archive_sha256.as_str(),
                    source.archive_path,
                    source.declared_encoding,
                    source.raw_sha256.as_str(),
                    source.raw_bytes,
                    serde_json::to_string(&source.failure.class)?,
                    source.failure.message,
                ],
            )?;
        }
        transaction.commit()?;
        self.checkpoint()
    }

    pub fn checkpoint(&self) -> Result<()> {
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(FULL)")?;
        Ok(())
    }

    pub fn completed_acquisition(
        &self,
        package: &str,
        version: &str,
    ) -> Result<Option<CompletedAcquisition>> {
        let row = self
            .connection
            .query_row(
                "SELECT archive_url,state,archive_sha256,source_count,decoded_source_count,error_class,error_message \
                 FROM acquisitions WHERE package=?1 AND version=?2",
                params![package, version],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((archive_url, state, archive, source_count, decoded_source_count, class, message)) =
            row
        else {
            return Ok(None);
        };
        let archive = archive.map(Sha256Digest::parse).transpose()?;
        let terminal = match state.as_str() {
            "collected" => AcquisitionTerminalState::Collected {
                archive_sha256: archive.ok_or_else(|| {
                    CorpusError::new(ErrorClass::Database, "collected archive has no digest")
                })?,
                source_count: source_count
                    .unwrap_or_default()
                    .try_into()
                    .map_err(|_| CorpusError::new(ErrorClass::Database, "invalid source count"))?,
                decoded_source_count: decoded_source_count
                    .unwrap_or_default()
                    .try_into()
                    .map_err(|_| {
                        CorpusError::new(ErrorClass::Database, "invalid decoded source count")
                    })?,
            },
            "rejected" | "failed" => {
                let failure = Failure {
                    class: serde_json::from_str(class.as_deref().ok_or_else(|| {
                        CorpusError::new(ErrorClass::Database, "failed acquisition has no class")
                    })?)?,
                    message: message.unwrap_or_default(),
                };
                if state == "rejected" {
                    AcquisitionTerminalState::Rejected {
                        failure,
                        observed_archive_sha256: archive,
                    }
                } else {
                    AcquisitionTerminalState::Failed {
                        failure,
                        observed_archive_sha256: archive,
                    }
                }
            }
            other => {
                return Err(CorpusError::new(
                    ErrorClass::Database,
                    format!("unknown acquisition state {other:?}"),
                ));
            }
        };
        let mut statement = self.connection.prepare(
            "SELECT archive_sha256,archive_path,declared_encoding,decoded_encoding,raw_sha256,decoded_sha256,raw_bytes,decoded_bytes,curated \
             FROM source_occurrences WHERE package=?1 AND version=?2 ORDER BY archive_path",
        )?;
        let rows = statement.query_map(params![package, version], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, bool>(8)?,
            ))
        })?;
        let mut sources = Vec::new();
        for row in rows {
            let (
                archive_sha256,
                archive_path,
                declared_encoding,
                decoded_encoding,
                raw_sha256,
                decoded_sha256,
                raw_bytes,
                decoded_bytes,
                curated,
            ) = row?;
            sources.push(SourceOccurrence {
                package: package.to_owned(),
                version: version.to_owned(),
                archive_sha256: Sha256Digest::parse(archive_sha256)?,
                archive_path,
                declared_encoding,
                decoded_encoding: serde_json::from_str::<SourceEncoding>(&decoded_encoding)?,
                raw_sha256: Sha256Digest::parse(raw_sha256)?,
                decoded_sha256: Sha256Digest::parse(decoded_sha256)?,
                raw_bytes: raw_bytes.try_into().map_err(|_| {
                    CorpusError::new(ErrorClass::Database, "invalid raw byte count")
                })?,
                decoded_bytes: decoded_bytes.try_into().map_err(|_| {
                    CorpusError::new(ErrorClass::Database, "invalid decoded byte count")
                })?,
                curated,
            });
        }
        let mut statement = self.connection.prepare(
            "SELECT archive_sha256,archive_path,declared_encoding,raw_sha256,raw_bytes,error_class,error_message \
             FROM source_failures WHERE package=?1 AND version=?2 ORDER BY archive_path",
        )?;
        let rows = statement.query_map(params![package, version], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;
        let mut source_failures = Vec::new();
        for row in rows {
            let (
                archive_sha256,
                archive_path,
                declared_encoding,
                raw_sha256,
                raw_bytes,
                class,
                message,
            ) = row?;
            source_failures.push(SourceFailureRecord {
                package: package.to_owned(),
                version: version.to_owned(),
                archive_sha256: Sha256Digest::parse(archive_sha256)?,
                archive_path,
                declared_encoding,
                raw_sha256: Sha256Digest::parse(raw_sha256)?,
                raw_bytes: raw_bytes.try_into().map_err(|_| {
                    CorpusError::new(ErrorClass::Database, "invalid raw byte count")
                })?,
                failure: Failure {
                    class: serde_json::from_str(&class)?,
                    message,
                },
            });
        }
        Ok(Some((
            AcquisitionRecord {
                package: package.to_owned(),
                version: version.to_owned(),
                archive_url,
                terminal,
            },
            sources,
            source_failures,
        )))
    }
}

fn store_terminal(transaction: &Transaction<'_>, record: &AcquisitionRecord) -> Result<()> {
    let (state, archive_sha256, source_count, decoded_source_count, class, message) =
        match &record.terminal {
            AcquisitionTerminalState::Collected {
                archive_sha256,
                source_count,
                decoded_source_count,
            } => (
                "collected",
                Some(archive_sha256.as_str()),
                Some(*source_count),
                Some(*decoded_source_count),
                None,
                None,
            ),
            AcquisitionTerminalState::Rejected {
                failure,
                observed_archive_sha256,
            } => (
                "rejected",
                observed_archive_sha256.as_ref().map(Sha256Digest::as_str),
                None,
                None,
                Some(serde_json::to_string(&failure.class)?),
                Some(failure.message.as_str()),
            ),
            AcquisitionTerminalState::Failed {
                failure,
                observed_archive_sha256,
            } => (
                "failed",
                observed_archive_sha256.as_ref().map(Sha256Digest::as_str),
                None,
                None,
                Some(serde_json::to_string(&failure.class)?),
                Some(failure.message.as_str()),
            ),
        };
    transaction.execute(
        "INSERT INTO acquisitions(package,version,archive_url,state,archive_sha256,source_count,decoded_source_count,error_class,error_message) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(package,version) DO UPDATE SET \
         archive_url=excluded.archive_url,state=excluded.state,archive_sha256=excluded.archive_sha256,source_count=excluded.source_count,decoded_source_count=excluded.decoded_source_count,error_class=excluded.error_class,error_message=excluded.error_message",
        params![record.package, record.version, record.archive_url, state, archive_sha256, source_count, decoded_source_count, class, message],
    )?;
    Ok(())
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL);
INSERT OR IGNORE INTO metadata(key,value) VALUES('schema_version','1');
CREATE TABLE IF NOT EXISTS observed_archives(
  package TEXT NOT NULL, version TEXT NOT NULL, archive_sha256 TEXT NOT NULL,
  PRIMARY KEY(package,version)
);
CREATE TABLE IF NOT EXISTS acquisitions(
  package TEXT NOT NULL, version TEXT NOT NULL, archive_url TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('collected','rejected','failed')),
  archive_sha256 TEXT, source_count INTEGER, decoded_source_count INTEGER,
  error_class TEXT, error_message TEXT,
  PRIMARY KEY(package,version)
);
CREATE TABLE IF NOT EXISTS source_occurrences(
  package TEXT NOT NULL, version TEXT NOT NULL, archive_sha256 TEXT NOT NULL,
  archive_path TEXT NOT NULL, declared_encoding TEXT NOT NULL, decoded_encoding TEXT NOT NULL,
  raw_sha256 TEXT NOT NULL, decoded_sha256 TEXT NOT NULL,
  raw_bytes INTEGER NOT NULL, decoded_bytes INTEGER NOT NULL,
  curated INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(package,version,archive_path),
  FOREIGN KEY(package,version) REFERENCES acquisitions(package,version) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS source_failures(
  package TEXT NOT NULL, version TEXT NOT NULL, archive_sha256 TEXT NOT NULL,
  archive_path TEXT NOT NULL, declared_encoding TEXT NOT NULL,
  raw_sha256 TEXT NOT NULL, raw_bytes INTEGER NOT NULL,
  error_class TEXT NOT NULL, error_message TEXT NOT NULL,
  PRIMARY KEY(package,version,archive_path),
  FOREIGN KEY(package,version) REFERENCES acquisitions(package,version) ON DELETE CASCADE
);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cas_is_sharded_and_verified() {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let digest = cas.put_bytes(b"corpus").unwrap();
        assert_eq!(cas.verify(&digest).unwrap(), 6);
        assert!(cas.path(&digest).ends_with(digest.as_str()));
    }

    #[test]
    fn stable_shards_are_repeatable() {
        assert_eq!(
            stable_shard(b"pkg/1.0", 17).unwrap(),
            stable_shard(b"pkg/1.0", 17).unwrap()
        );
        assert!(stable_shard(b"pkg", 0).is_err());
    }

    #[test]
    fn opening_an_older_store_adds_curated_provenance() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("index.sqlite3");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE source_occurrences(
                   package TEXT NOT NULL, version TEXT NOT NULL, archive_sha256 TEXT NOT NULL,
                   archive_path TEXT NOT NULL, declared_encoding TEXT NOT NULL,
                   decoded_encoding TEXT NOT NULL, raw_sha256 TEXT NOT NULL,
                   decoded_sha256 TEXT NOT NULL, raw_bytes INTEGER NOT NULL,
                   decoded_bytes INTEGER NOT NULL, PRIMARY KEY(package,version,archive_path)
                 );",
            )
            .unwrap();
        drop(connection);

        let store = CorpusStore::open(&database, directory.path().join("objects")).unwrap();
        let count = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('source_occurrences') WHERE name='curated'",
                [],
                |row| row.get::<_, usize>(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn terminal_acquisition_and_decode_failure_resume_after_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("index.sqlite3");
        let objects = directory.path().join("objects");
        let digest = Sha256Digest::parse("a".repeat(64)).unwrap();
        let raw = Sha256Digest::parse("b".repeat(64)).unwrap();
        let record = AcquisitionRecord {
            package: "pkg".into(),
            version: "1.0".into(),
            archive_url: "https://example.test/source".into(),
            terminal: AcquisitionTerminalState::Collected {
                archive_sha256: digest.clone(),
                source_count: 1,
                decoded_source_count: 0,
            },
        };
        let failure = SourceFailureRecord {
            package: "pkg".into(),
            version: "1.0".into(),
            archive_sha256: digest,
            archive_path: "pkg/R/bad.R".into(),
            declared_encoding: "UTF-8".into(),
            raw_sha256: raw,
            raw_bytes: 1,
            failure: Failure {
                class: ErrorClass::Decode,
                message: "invalid UTF-8".into(),
            },
        };
        {
            let mut store = CorpusStore::open(&database, &objects).unwrap();
            store
                .finish_acquisition(&record, &[], std::slice::from_ref(&failure))
                .unwrap();
        }
        let store = CorpusStore::open(&database, &objects).unwrap();
        let (loaded, sources, failures) =
            store.completed_acquisition("pkg", "1.0").unwrap().unwrap();
        assert_eq!(loaded.archive_url, record.archive_url);
        assert!(sources.is_empty());
        assert_eq!(failures, [failure]);
    }
}
