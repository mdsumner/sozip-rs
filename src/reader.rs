//! SOZip chunk reader with `Read + Seek` over uncompressed data.
//!
//! Given a SOZip index and access to the raw compressed bytes of a ZIP entry,
//! this module provides a reader that can seek to any uncompressed offset
//! and read by fetching + inflating only the relevant chunk.

use std::io::{self, Read, Seek, SeekFrom};

use flate2::read::DeflateDecoder;

use crate::error::SozipError;
use crate::index::SozipIndex;

/// A reader providing `Read + Seek` over the uncompressed content of a
/// SOZip-enabled ZIP entry.
///
/// The reader maintains a cached decompressed chunk to avoid re-inflating
/// for sequential reads within the same chunk.
pub struct SozipReader<R: Read + Seek> {
    /// The underlying reader, positioned over the entire ZIP file.
    inner: R,
    /// Absolute byte offset in `inner` where the compressed data begins
    /// (i.e. first byte after the local file header).
    data_start: u64,
    /// The parsed SOZip index.
    index: SozipIndex,
    /// Current logical (uncompressed) position.
    pos: u64,
    /// Cached decompressed chunk data.
    cache: Vec<u8>,
    /// Which chunk is currently cached (None if cache is empty/invalid).
    cached_chunk: Option<usize>,
}

impl<R: Read + Seek> SozipReader<R> {
    /// Create a new SOZip reader.
    ///
    /// - `inner`: a reader over the entire ZIP file (must support seek)
    /// - `data_start`: absolute byte offset where the entry's compressed
    ///
    /// data begins (after the local file header)
    /// - `index`: the parsed SOZip index for this entry
    pub fn new(inner: R, data_start: u64, index: SozipIndex) -> Self {
        SozipReader {
            inner,
            data_start,
            index,
            pos: 0,
            cache: Vec::new(),
            cached_chunk: None,
        }
    }

    /// Ensure the given chunk is decompressed and in the cache.
    fn ensure_chunk(&mut self, chunk_index: usize) -> Result<(), SozipError> {
        if self.cached_chunk == Some(chunk_index) {
            return Ok(());
        }

        let (comp_start, comp_end) = self.index.compressed_range(chunk_index)?;
        let comp_len = (comp_end - comp_start) as usize;

        // Seek to the compressed chunk within the ZIP file
        let abs_offset = self.data_start + comp_start;
        self.inner.seek(SeekFrom::Start(abs_offset))?;

        // Read the compressed bytes
        let mut compressed = vec![0u8; comp_len];
        self.inner.read_exact(&mut compressed)?;

        // Decompress. Each chunk is independently decompressible because
        // SOZip inserts Z_SYNC_FLUSH + Z_FULL_FLUSH at chunk boundaries.
        let mut decoder = DeflateDecoder::new(&compressed[..]);

        // Expected decompressed size for this chunk
        let chunk_start_uncompressed = chunk_index as u64 * self.index.chunk_size as u64;
        let chunk_end_uncompressed =
            std::cmp::min(chunk_start_uncompressed + self.index.chunk_size as u64,
                          self.index.uncompressed_size);
        let expected_size = (chunk_end_uncompressed - chunk_start_uncompressed) as usize;

        self.cache.clear();
        self.cache.reserve(expected_size);

        // Read into cache — we use read_to_end because the last chunk
        // may be shorter than chunk_size
        decoder.read_to_end(&mut self.cache).map_err(|e| {
            SozipError::Inflate(format!("chunk {chunk_index}: {e}"))
        })?;

        if self.cache.len() != expected_size {
            return Err(SozipError::Inflate(format!(
                "chunk {chunk_index}: expected {expected_size} decompressed bytes, got {}",
                self.cache.len()
            )));
        }

        self.cached_chunk = Some(chunk_index);
        Ok(())
    }

    /// Access the underlying SOZip index.
    pub fn index(&self) -> &SozipIndex {
        &self.index
    }

    /// The total uncompressed size of the entry.
    pub fn uncompressed_size(&self) -> u64 {
        self.index.uncompressed_size
    }
}

impl<R: Read + Seek> Read for SozipReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.index.uncompressed_size {
            return Ok(0); // EOF
        }

        let (chunk_index, chunk_offset) = self.index.locate_chunk(self.pos)
            .map_err(io::Error::other)?;

        self.ensure_chunk(chunk_index)
            .map_err(io::Error::other)?;

        // How many bytes are available from chunk_offset to end of cached chunk
        let available = self.cache.len() - chunk_offset;
        let to_copy = std::cmp::min(available, buf.len());

        buf[..to_copy].copy_from_slice(&self.cache[chunk_offset..chunk_offset + to_copy]);
        self.pos += to_copy as u64;

        Ok(to_copy)
    }
}

impl<R: Read + Seek> Seek for SozipReader<R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let new_pos = match from {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::Current(offset) => self.pos as i64 + offset,
            SeekFrom::End(offset) => self.index.uncompressed_size as i64 + offset,
        };

        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek to negative position",
            ));
        }

        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}
