//! High-level archive API for opening SOZip-enabled ZIP files.
//!
//! Wraps the `zip` crate for central directory parsing and provides
//! discovery of SOZip-enabled entries (both CD-listed and orphan-header)
//! plus construction of [`SozipReader`] instances for random access.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use zip::ZipArchive;

use crate::error::SozipError;
use crate::index::{self, SozipIndex};

/// ZIP local file header signature.
const LOCAL_HEADER_SIG: [u8; 4] = [0x50, 0x4b, 0x03, 0x04];

/// Information about a SOZip-enabled entry within an archive.
#[derive(Debug, Clone)]
pub struct SozipEntry {
    /// The entry name (path within the ZIP).
    pub name: String,
    /// Uncompressed size from the ZIP central directory.
    pub uncompressed_size: u64,
    /// Compressed size from the ZIP central directory.
    pub compressed_size: u64,
    /// Absolute byte offset where compressed data starts in the archive.
    pub data_start: u64,
    /// The parsed SOZip index.
    pub index: SozipIndex,
}

/// Metadata collected from the ZIP central directory for a single entry.
#[derive(Debug, Clone)]
struct EntryMeta {
    name: String,
    data_start: u64,
    uncompressed_size: u64,
    compressed_size: u64,
}

/// A SOZip-aware ZIP archive.
///
/// After construction, the inner reader is available for direct use by
/// [`SozipReader`].
pub struct SozipArchive<R: Read + Seek> {
    reader: R,
    /// Map from entry name to SOZip entry info (only for SOZip-enabled entries).
    entries: HashMap<String, SozipEntry>,
    /// All non-index entry names in order.
    file_names: Vec<String>,
}

impl SozipArchive<BufReader<File>> {
    /// Open a SOZip archive from a file path.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, SozipError> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        Self::new(reader)
    }
}

impl<R: Read + Seek> SozipArchive<R> {
    /// Create a SOZip archive from a seekable reader.
    ///
    /// Parses the ZIP central directory, discovers SOZip indices (both
    /// CD-listed and orphan local headers), and builds entry metadata.
    pub fn new(reader: R) -> Result<Self, SozipError> {
        let mut archive = ZipArchive::new(reader)?;

        // Phase 1: enumerate central directory entries
        let mut data_entries: Vec<EntryMeta> = Vec::new();
        let mut cd_index_names: HashMap<String, usize> = HashMap::new();
        let mut file_names: Vec<String> = Vec::new();

        for i in 0..archive.len() {
            let entry = archive.by_index_raw(i)?;
            let name = entry.name().to_string();
            if name.ends_with(".sozip.idx") {
                cd_index_names.insert(name, i);
            } else {
                data_entries.push(EntryMeta {
                    name: name.clone(),
                    data_start: entry.data_start(),
                    uncompressed_size: entry.size(),
                    compressed_size: entry.compressed_size(),
                });
                file_names.push(name);
            }
        }

        // Phase 1b: read any CD-listed index files while we still have
        // the ZipArchive (which handles decompression for us).
        let mut cd_index_data: HashMap<String, Vec<u8>> = HashMap::new();
        for (idx_name, &zip_idx) in &cd_index_names {
            let mut idx_entry = archive.by_index(zip_idx)?;
            let mut buf = Vec::new();
            idx_entry.read_to_end(&mut buf)?;
            cd_index_data.insert(idx_name.clone(), buf);
        }

        // Phase 2: get the raw reader back for orphan-header scanning
        let mut reader = archive.into_inner();

        // Phase 3: for each data entry, find its SOZip index
        let mut entries = HashMap::new();

        for meta in &data_entries {
            let expected_idx_name = index::index_filename(&meta.name);

            // Try CD-listed index first
            let idx_data = if let Some(data) = cd_index_data.get(&expected_idx_name) {
                Some(data.clone())
            } else {
                // Scan for orphan local header immediately after compressed data
                scan_orphan_index(
                    &mut reader,
                    meta.data_start + meta.compressed_size,
                    &expected_idx_name,
                )?
            };

            let Some(idx_data) = idx_data else {
                continue;
            };

            let sozip_index = match SozipIndex::from_bytes(&idx_data) {
                Ok(idx) => idx,
                Err(_) => continue,
            };

            // Consistency check
            if sozip_index.uncompressed_size != meta.uncompressed_size {
                return Err(SozipError::SizeMismatch {
                    index_size: sozip_index.uncompressed_size,
                    entry_size: meta.uncompressed_size,
                });
            }

            entries.insert(
                meta.name.clone(),
                SozipEntry {
                    name: meta.name.clone(),
                    uncompressed_size: meta.uncompressed_size,
                    compressed_size: meta.compressed_size,
                    data_start: meta.data_start,
                    index: sozip_index,
                },
            );
        }

        Ok(SozipArchive {
            reader,
            entries,
            file_names,
        })
    }

    /// Get a mutable reference to the underlying reader.
    ///
    /// This is used by [`SozipReader`] — you can construct a reader
    /// directly from the entry info and this reader reference, or
    /// (for file-backed archives) open a separate file handle.
    pub fn reader_mut(&mut self) -> &mut R {
        &mut self.reader
    }

    /// Consume the archive and return the inner reader.
    pub fn into_inner(self) -> R {
        self.reader
    }

    /// List all entry names in the archive (SOZip-enabled or not),
    /// excluding hidden `.sozip.idx` files.
    pub fn file_names(&self) -> &[String] {
        &self.file_names
    }

    /// List only SOZip-enabled entry names.
    pub fn sozip_entries(&self) -> Vec<&str> {
        self.entries.keys().map(|s| s.as_str()).collect()
    }

    /// Check whether a specific entry is SOZip-enabled.
    pub fn is_sozip(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// Get SOZip entry info (if the entry is SOZip-enabled).
    pub fn entry_info(&self, name: &str) -> Option<&SozipEntry> {
        self.entries.get(name)
    }

    /// Get a summary of SOZip status for all entries.
    pub fn validate(&self) -> Vec<ValidationResult> {
        self.file_names
            .iter()
            .map(|name| {
                if let Some(entry) = self.entries.get(name) {
                    ValidationResult {
                        name: name.clone(),
                        sozip: true,
                        chunk_size: Some(entry.index.chunk_size),
                        num_chunks: Some(entry.index.num_chunks()),
                    }
                } else {
                    ValidationResult {
                        name: name.clone(),
                        sozip: false,
                        chunk_size: None,
                        num_chunks: None,
                    }
                }
            })
            .collect()
    }
}

/// Scan for an orphan SOZip index local header at the given file offset.
///
/// Per the SOZip spec, the `.sozip.idx` file is written with a local
/// file header immediately after the compressed data of the target entry,
/// but is intentionally NOT listed in the central directory.
///
/// Returns `Ok(Some(payload_bytes))` if found and filename matches,
/// `Ok(None)` if no matching header at that offset.
fn scan_orphan_index<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    expected_name: &str,
) -> Result<Option<Vec<u8>>, SozipError> {
    reader.seek(SeekFrom::Start(offset))?;

    // Read local header signature (4 bytes)
    let mut sig = [0u8; 4];
    if reader.read_exact(&mut sig).is_err() {
        return Ok(None);
    }
    if sig != LOCAL_HEADER_SIG {
        return Ok(None);
    }

    // Parse the fixed-size portion of the local header (26 bytes after signature)
    let mut fixed = [0u8; 26];
    reader.read_exact(&mut fixed)?;

    // Bytes 4-5 (offset 8-9 in header): compression method
    let compression = u16::from_le_bytes([fixed[4], fixed[5]]);

    // Bytes 14-17 (offset 18-21): compressed size
    let comp_size = u32::from_le_bytes([fixed[14], fixed[15], fixed[16], fixed[17]]) as u64;

    // Bytes 22-23 (offset 26-27): filename length
    let fname_len = u16::from_le_bytes([fixed[22], fixed[23]]) as usize;

    // Bytes 24-25 (offset 28-29): extra field length
    let extra_len = u16::from_le_bytes([fixed[24], fixed[25]]) as usize;

    // Read filename
    let mut fname_buf = vec![0u8; fname_len];
    reader.read_exact(&mut fname_buf)?;
    let fname = String::from_utf8_lossy(&fname_buf);

    if fname != expected_name {
        return Ok(None);
    }

    // Skip extra field
    if extra_len > 0 {
        reader.seek(SeekFrom::Current(extra_len as i64))?;
    }

    // The index is stored uncompressed (compression method 0)
    if compression != 0 {
        return Ok(None);
    }

    // Read the payload
    let mut payload = vec![0u8; comp_size as usize];
    reader.read_exact(&mut payload)?;

    Ok(Some(payload))
}

/// Result of validating a single entry.
#[derive(Debug)]
pub struct ValidationResult {
    pub name: String,
    pub sozip: bool,
    pub chunk_size: Option<u32>,
    pub num_chunks: Option<usize>,
}

impl std::fmt::Display for ValidationResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.sozip {
            write!(
                f,
                "* File {} has a valid SOZip index, using chunk_size = {}",
                self.name,
                self.chunk_size.unwrap_or(0)
            )
        } else {
            write!(f, "  File {} is not SOZip-optimized", self.name)
        }
    }
}
