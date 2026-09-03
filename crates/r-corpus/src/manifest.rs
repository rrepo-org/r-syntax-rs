use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Cursor, Read, Seek, Write},
    path::Path,
};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::{
    model::{
        AcquisitionRecord, CorpusError, ErrorClass, FinalManifest, InventorySnapshot,
        ManifestArchive, Result, Sha256Digest, SourceFailureRecord, SourceOccurrence,
        INVENTORY_VERSION, MANIFEST_VERSION,
    },
    store::{sha256_bytes, sha256_reader, shard_for_digest, stable_shard},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestHashes {
    pub content_sha256: Sha256Digest,
    pub archive_sha256: Sha256Digest,
    pub archive_shard: String,
}

#[derive(Serialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum ManifestLineRef<'a> {
    Header {
        schema_version: u32,
        inventory_sha256: &'a Sha256Digest,
    },
    Acquisition(&'a AcquisitionRecord),
    Source(&'a SourceOccurrence),
    SourceFailure(&'a SourceFailureRecord),
}

#[derive(Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum ManifestLine {
    Header {
        schema_version: u32,
        inventory_sha256: Sha256Digest,
    },
    Acquisition(AcquisitionRecord),
    Source(SourceOccurrence),
    SourceFailure(SourceFailureRecord),
}

/// Produces canonical NDJSON. Record ordering and JSON field ordering are fixed;
/// no hash maps or platform-specific path representations enter the format.
pub fn canonical_ndjson(manifest: &FinalManifest) -> Result<Vec<u8>> {
    if manifest.schema_version != MANIFEST_VERSION {
        return Err(CorpusError::new(
            ErrorClass::Format,
            format!("unsupported manifest version {}", manifest.schema_version),
        ));
    }
    let mut acquisitions: Vec<_> = manifest.acquisitions.iter().collect();
    acquisitions.sort_by(|left, right| {
        (&left.package, &left.version, &left.archive_url).cmp(&(
            &right.package,
            &right.version,
            &right.archive_url,
        ))
    });
    let mut sources: Vec<_> = manifest.sources.iter().collect();
    sources.sort_by(|left, right| {
        (&left.package, &left.version, &left.archive_path).cmp(&(
            &right.package,
            &right.version,
            &right.archive_path,
        ))
    });

    let mut output = Vec::new();
    write_line(
        &mut output,
        &ManifestLineRef::Header {
            schema_version: manifest.schema_version,
            inventory_sha256: &manifest.inventory_sha256,
        },
    )?;
    for acquisition in acquisitions {
        write_line(&mut output, &ManifestLineRef::Acquisition(acquisition))?;
    }
    for source in sources {
        write_line(&mut output, &ManifestLineRef::Source(source))?;
    }
    let mut source_failures: Vec<_> = manifest.source_failures.iter().collect();
    source_failures.sort_by(|left, right| {
        (&left.package, &left.version, &left.archive_path).cmp(&(
            &right.package,
            &right.version,
            &right.archive_path,
        ))
    });
    for failure in source_failures {
        write_line(&mut output, &ManifestLineRef::SourceFailure(failure))?;
    }
    Ok(output)
}

fn write_line(output: &mut Vec<u8>, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.push(b'\n');
    Ok(())
}

pub fn content_hash(manifest: &FinalManifest) -> Result<Sha256Digest> {
    Ok(sha256_bytes(&canonical_ndjson(manifest)?))
}

/// Selects one deterministic finalized-manifest shard by observed archive hash.
pub fn select_archive_shard(
    manifest: &FinalManifest,
    shard_index: u32,
    shard_count: u32,
) -> Result<FinalManifest> {
    if shard_index >= shard_count {
        return Err(CorpusError::new(
            ErrorClass::Format,
            format!("shard index {shard_index} is outside shard count {shard_count}"),
        ));
    }
    let acquisitions = manifest
        .acquisitions
        .iter()
        .filter(|record| {
            let digest = match &record.terminal {
                crate::model::AcquisitionTerminalState::Collected { archive_sha256, .. } => {
                    archive_sha256.as_str()
                }
                crate::model::AcquisitionTerminalState::Rejected {
                    observed_archive_sha256: Some(digest),
                    ..
                }
                | crate::model::AcquisitionTerminalState::Failed {
                    observed_archive_sha256: Some(digest),
                    ..
                } => digest.as_str(),
                _ => {
                    return matches!(
                        stable_shard(
                            format!("{}\0{}\0{}", record.package, record.version, record.archive_url)
                                .as_bytes(),
                            shard_count,
                        ),
                        Ok(selected) if selected == shard_index
                    )
                }
            };
            matches!(
                stable_shard(digest.as_bytes(), shard_count),
                Ok(selected) if selected == shard_index
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let selected = acquisitions
        .iter()
        .map(|record| (record.package.clone(), record.version.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    let sources = manifest
        .sources
        .iter()
        .filter(|source| selected.contains(&(source.package.clone(), source.version.clone())))
        .cloned()
        .collect();
    Ok(FinalManifest {
        schema_version: manifest.schema_version,
        inventory_sha256: manifest.inventory_sha256.clone(),
        acquisitions,
        sources,
        source_failures: manifest
            .source_failures
            .iter()
            .filter(|source| selected.contains(&(source.package.clone(), source.version.clone())))
            .cloned()
            .collect(),
        archive: None,
    })
}

/// Atomically exports deterministic zstd-compressed canonical NDJSON and fills
/// the out-of-band archive identity on the returned manifest.
pub fn export_manifest(
    manifest: &FinalManifest,
    destination: impl AsRef<Path>,
) -> Result<(FinalManifest, ManifestHashes)> {
    let canonical = canonical_ndjson(manifest)?;
    let content_sha256 = sha256_bytes(&canonical);
    let mut compressed = Vec::new();
    {
        let mut encoder = zstd::stream::write::Encoder::new(&mut compressed, 19)?;
        encoder.include_checksum(true)?;
        encoder.write_all(&canonical)?;
        encoder.finish()?;
    }
    let archive_sha256 = sha256_bytes(&compressed);
    atomic_write(destination.as_ref(), &compressed, false)?;
    let archive_shard = shard_for_digest(&archive_sha256);
    let mut finalized = manifest.clone();
    finalized.archive = Some(ManifestArchive {
        sha256: archive_sha256.clone(),
        shard: archive_shard.clone(),
    });
    Ok((
        finalized,
        ManifestHashes {
            content_sha256,
            archive_sha256,
            archive_shard,
        },
    ))
}

pub fn import_manifest(
    source: impl AsRef<Path>,
    expected_archive_sha256: Option<&Sha256Digest>,
) -> Result<(FinalManifest, ManifestHashes)> {
    let mut file = File::open(source)?;
    let (observed_archive, _) = sha256_reader(&mut file)?;
    if let Some(expected) = expected_archive_sha256 {
        if expected != &observed_archive {
            return Err(CorpusError::new(
                ErrorClass::Integrity,
                format!("manifest archive expected {expected}, observed {observed_archive}"),
            ));
        }
    }
    file.rewind()?;
    let decoder = zstd::stream::read::Decoder::new(file)?;
    let mut canonical = Vec::new();
    decoder
        .take(512 * 1024 * 1024 + 1)
        .read_to_end(&mut canonical)?;
    if canonical.len() > 512 * 1024 * 1024 {
        return Err(CorpusError::new(
            ErrorClass::ResourceLimit,
            "manifest expands beyond 512 MiB",
        ));
    }
    let manifest = parse_ndjson(&canonical)?;
    if canonical_ndjson(&manifest)? != canonical {
        return Err(CorpusError::new(
            ErrorClass::Format,
            "manifest NDJSON is not canonical",
        ));
    }
    let content_sha256 = sha256_bytes(&canonical);
    let archive_shard = shard_for_digest(&observed_archive);
    Ok((
        manifest,
        ManifestHashes {
            content_sha256,
            archive_sha256: observed_archive,
            archive_shard,
        },
    ))
}

fn parse_ndjson(bytes: &[u8]) -> Result<FinalManifest> {
    let mut lines = BufReader::new(Cursor::new(bytes)).lines();
    let first = lines.next().ok_or_else(|| {
        CorpusError::new(ErrorClass::Format, "manifest does not contain a header")
    })??;
    let ManifestLine::Header {
        schema_version,
        inventory_sha256,
    } = serde_json::from_str(&first)?
    else {
        return Err(CorpusError::new(
            ErrorClass::Format,
            "first manifest record is not a header",
        ));
    };
    if schema_version != MANIFEST_VERSION {
        return Err(CorpusError::new(
            ErrorClass::Format,
            format!("unsupported manifest version {schema_version}"),
        ));
    }
    let mut manifest = FinalManifest::new(inventory_sha256);
    for line in lines {
        let line = line?;
        if line.is_empty() {
            return Err(CorpusError::new(
                ErrorClass::Format,
                "manifest contains an empty record",
            ));
        }
        match serde_json::from_str(&line)? {
            ManifestLine::Header { .. } => {
                return Err(CorpusError::new(
                    ErrorClass::Format,
                    "manifest contains multiple headers",
                ));
            }
            ManifestLine::Acquisition(record) => manifest.acquisitions.push(record),
            ManifestLine::Source(source) => manifest.sources.push(source),
            ManifestLine::SourceFailure(failure) => manifest.source_failures.push(failure),
        }
    }
    Ok(manifest)
}

pub fn canonical_inventory(snapshot: &InventorySnapshot) -> Result<Vec<u8>> {
    if snapshot.schema_version != INVENTORY_VERSION {
        return Err(CorpusError::new(
            ErrorClass::Format,
            format!("unsupported inventory version {}", snapshot.schema_version),
        ));
    }
    let mut snapshot = snapshot.clone();
    snapshot
        .packages
        .sort_by(|left, right| left.name.cmp(&right.name));
    for package in &mut snapshot.packages {
        package.versions.sort_by(|left, right| {
            (&left.version, &left.archive_url).cmp(&(&right.version, &right.archive_url))
        });
    }
    let mut bytes = serde_json::to_vec(&snapshot)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn write_inventory_snapshot(
    snapshot: &InventorySnapshot,
    destination: impl AsRef<Path>,
) -> Result<Sha256Digest> {
    let bytes = canonical_inventory(snapshot)?;
    let digest = sha256_bytes(&bytes);
    atomic_write(destination.as_ref(), &bytes, false)?;
    Ok(digest)
}

pub fn read_inventory_snapshot(
    source: impl AsRef<Path>,
) -> Result<(InventorySnapshot, Sha256Digest)> {
    let bytes = fs::read(source)?;
    let snapshot: InventorySnapshot = serde_json::from_slice(&bytes)?;
    if canonical_inventory(&snapshot)? != bytes {
        return Err(CorpusError::new(
            ErrorClass::Format,
            "inventory snapshot is not canonical",
        ));
    }
    let digest = sha256_bytes(&bytes);
    Ok((snapshot, digest))
}

fn atomic_write(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    if !replace && path.exists() {
        let existing = fs::read(path)?;
        if existing == bytes {
            return Ok(());
        }
        return Err(CorpusError::new(
            ErrorClass::Integrity,
            format!("immutable snapshot already exists at {}", path.display()),
        ));
    }
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file_mut().sync_all()?;
    if replace {
        temporary.persist(path).map_err(|error| error.error)?;
    } else {
        temporary
            .persist_noclobber(path)
            .map_err(|error| error.error)?;
    }
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AcquisitionTerminalState, ErrorClass, Failure, InventoryPackage};

    #[test]
    fn canonical_inventory_sorts_packages() {
        let mut snapshot = InventorySnapshot::new("https://example.test", 1);
        snapshot.packages = vec![
            InventoryPackage {
                name: "z".into(),
                versions: Vec::new(),
            },
            InventoryPackage {
                name: "a".into(),
                versions: Vec::new(),
            },
        ];
        let text = String::from_utf8(canonical_inventory(&snapshot).unwrap()).unwrap();
        assert!(text.find("\"a\"").unwrap() < text.find("\"z\"").unwrap());
    }

    #[test]
    fn finalized_shards_account_for_digestless_failures() {
        let mut manifest = FinalManifest::new(Sha256Digest::parse("a".repeat(64)).unwrap());
        manifest.acquisitions = vec![
            AcquisitionRecord {
                package: "collected".into(),
                version: "1".into(),
                archive_url: "https://example.test/collected".into(),
                terminal: AcquisitionTerminalState::Collected {
                    archive_sha256: Sha256Digest::parse("b".repeat(64)).unwrap(),
                    source_count: 0,
                    decoded_source_count: 0,
                },
            },
            AcquisitionRecord {
                package: "failed".into(),
                version: "1".into(),
                archive_url: "https://example.test/failed".into(),
                terminal: AcquisitionTerminalState::Failed {
                    failure: Failure {
                        class: ErrorClass::Network,
                        message: "offline".into(),
                    },
                    observed_archive_sha256: None,
                },
            },
        ];
        let mut identities = Vec::new();
        for index in 0..8 {
            identities.extend(
                select_archive_shard(&manifest, index, 8)
                    .unwrap()
                    .acquisitions
                    .into_iter()
                    .map(|record| record.package),
            );
        }
        identities.sort();
        assert_eq!(identities, ["collected", "failed"]);
    }
}
