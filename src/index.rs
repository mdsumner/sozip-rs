//! SOZip index parsing.
//!
//! The SOZip index is a hidden file inside the ZIP archive, named
//! `{filename}.sozip.idx` (or `{dir}/.{basename}.sozip.idx` for files in
//! subdirectories). It is preceded by a local file header but intentionally
//! NOT listed in the central directory.
//!
//! The index consists of a 32-byte header followed by an array of `u64`
//! offsets giving the compressed byte position of each chunk boundary.
//!
//! ## Index header layout (32 bytes, all little-endian)
//!
//! | Offset | Size | Field              | Value/Constraint                     |
//! |--------|------|--------------------|--------------------------------------|
//! | 0      | 4    | version            | Must be 1                            |
//! | 4      | 4    | skip_bytes         | Bytes to skip after header (usually 0)|
//! | 8      | 4    | chunk_size         | Must be > 0, typically 32768         |
//! | 12     | 4    | offset_size        | Must be 8                            |
//! | 16     | 8    | uncompressed_size  | Consistency check with ZIP entry     |
//! | 24     | 8    | compressed_size    | Consistency check with ZIP entry     |
//!
//! The offset section follows at byte 32 + skip_bytes. It contains
//! `ceil(uncompressed_size / chunk_size)` entries of `offset_size` bytes each.
//! Each entry is the byte offset within the compressed data stream where
//! that chunk's compressed data begins.

use crate::error::SozipError;

/// Current SOZip specification version.
pub const SOZIP_VERSION: u32 = 1;

/// Default chunk size in bytes (32 KB).
pub const SOZIP_DEFAULT_CHUNK_SIZE: u32 = 32768;

/// Parsed SOZip index: header metadata + chunk offset table.
#[derive(Debug, Clone)]
pub struct SozipIndex {
    /// Spec version (must be 1).
    pub version: u32,
    /// Chunk size in uncompressed bytes.
    pub chunk_size: u32,
    /// Uncompressed size of the target file (from index header).
    pub uncompressed_size: u64,
    /// Compressed size of the target file (from index header).
    pub compressed_size: u64,
    /// Array of compressed byte offsets, one per chunk.
    /// `offsets[i]` is the byte offset within the compressed data stream
    /// where chunk `i` begins.
    pub offsets: Vec<u64>,
}

impl SozipIndex {
    /// Parse a SOZip index from raw bytes.
    ///
    /// `data` must contain the complete contents of the `.sozip.idx` file
    /// (i.e. the uncompressed payload, which is stored uncompressed in the ZIP).
    pub fn from_bytes(data: &[u8]) -> Result<Self, SozipError> {
        if data.len() < 32 {
            return Err(SozipError::InvalidIndex {
                reason: format!("index too short: {} bytes, need at least 32", data.len()),
            });
        }

        let version = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let skip_bytes = u32::from_le_bytes(data[4..8].try_into().unwrap());
        let chunk_size = u32::from_le_bytes(data[8..12].try_into().unwrap());
        let offset_size = u32::from_le_bytes(data[12..16].try_into().unwrap());
        let uncompressed_size = u64::from_le_bytes(data[16..24].try_into().unwrap());
        let compressed_size = u64::from_le_bytes(data[24..32].try_into().unwrap());

        if version != SOZIP_VERSION {
            return Err(SozipError::InvalidIndex {
                reason: format!("unsupported version {version}, expected {SOZIP_VERSION}"),
            });
        }

        if chunk_size == 0 {
            return Err(SozipError::InvalidIndex {
                reason: "chunk_size must not be zero".into(),
            });
        }

        if offset_size != 8 {
            return Err(SozipError::InvalidIndex {
                reason: format!("offset_size must be 8, got {offset_size}"),
            });
        }

        // Number of chunks = ceil(uncompressed_size / chunk_size)
        let num_chunks = if uncompressed_size == 0 {
            0usize
        } else {
            ((uncompressed_size - 1) / chunk_size as u64 + 1) as usize
        };

        // The offset array stores num_chunks - 1 entries.
        // Chunk 0 implicitly starts at compressed offset 0.
        // offsets[0] is where chunk 1 begins, offsets[1] is chunk 2, etc.
        let num_stored_offsets = if num_chunks > 0 { num_chunks - 1 } else { 0 };

        let offset_start = 32 + skip_bytes as usize;
        let expected_len = offset_start + num_stored_offsets * 8;

        if data.len() < expected_len {
            return Err(SozipError::InvalidIndex {
                reason: format!(
                    "index data too short for {num_stored_offsets} offsets \
                     ({num_chunks} chunks): need {expected_len} bytes, have {}",
                    data.len()
                ),
            });
        }

        let mut offsets = Vec::with_capacity(num_stored_offsets);
        for i in 0..num_stored_offsets {
            let pos = offset_start + i * 8;
            let offset = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
            offsets.push(offset);
        }

        // Sanity: offsets should be monotonically non-decreasing
        for window in offsets.windows(2) {
            if window[1] < window[0] {
                return Err(SozipError::InvalidIndex {
                    reason: format!(
                        "non-monotonic offsets: {} followed by {}",
                        window[0], window[1]
                    ),
                });
            }
        }

        Ok(SozipIndex {
            version,
            chunk_size,
            uncompressed_size,
            compressed_size,
            offsets,
        })
    }

    /// Compute which chunk contains the given uncompressed byte offset,
    /// and the offset within that decompressed chunk.
    ///
    /// Returns `(chunk_index, offset_within_chunk)`.
    pub fn locate_chunk(&self, pos: u64) -> Result<(usize, usize), SozipError> {
        if pos >= self.uncompressed_size {
            return Err(SozipError::SeekOutOfRange {
                offset: pos,
                size: self.uncompressed_size,
            });
        }
        let chunk_index = (pos / self.chunk_size as u64) as usize;
        let chunk_offset = (pos % self.chunk_size as u64) as usize;
        Ok((chunk_index, chunk_offset))
    }

    /// Return the compressed byte range [start, end) for the given chunk.
    ///
    /// The range is relative to the start of the compressed data stream
    /// (i.e. the first byte after the local file header of the target file).
    /// The caller must add the data_start offset to get absolute file positions.
    ///
    /// Chunk 0 implicitly starts at compressed offset 0.
    /// `self.offsets[i]` gives the start of chunk `i+1`.
    pub fn compressed_range(&self, chunk_index: usize) -> Result<(u64, u64), SozipError> {
        let total_chunks = self.num_chunks();
        if chunk_index >= total_chunks {
            return Err(SozipError::InvalidIndex {
                reason: format!(
                    "chunk index {} out of range (have {} chunks)",
                    chunk_index, total_chunks
                ),
            });
        }

        // Chunk 0 starts at 0; chunk N starts at offsets[N-1]
        let start = if chunk_index == 0 {
            0
        } else {
            self.offsets[chunk_index - 1]
        };

        // Chunk N ends where chunk N+1 starts, or at compressed_size for the last
        let end = if chunk_index < self.offsets.len() {
            self.offsets[chunk_index]
        } else {
            self.compressed_size
        };

        Ok((start, end))
    }

    /// Number of chunks in the index.
    ///
    /// This is `offsets.len() + 1` (chunk 0 is implicit), or 0 if empty.
    pub fn num_chunks(&self) -> usize {
        if self.uncompressed_size == 0 {
            0
        } else {
            self.offsets.len() + 1
        }
    }
}

/// Compute the expected SOZip index filename for a given entry name.
///
/// Per the spec:
/// - `mydata.gpkg` → `mydata.gpkg.sozip.idx`  (hidden: `.mydata.gpkg.sozip.idx`)
/// - `subdir/mydata.gpkg` → `subdir/.mydata.gpkg.sozip.idx`
///
/// The index file is stored with a leading dot to make it "hidden" in the
/// ZIP listing (though it's actually hidden by not being in the central directory).
pub fn index_filename(entry_name: &str) -> String {
    if let Some(pos) = entry_name.rfind('/') {
        let dir = &entry_name[..=pos];
        let base = &entry_name[pos + 1..];
        format!("{dir}.{base}.sozip.idx")
    } else {
        format!(".{entry_name}.sozip.idx")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_index_bytes(
        version: u32,
        skip_bytes: u32,
        chunk_size: u32,
        offset_size: u32,
        uncompressed_size: u64,
        compressed_size: u64,
        offsets: &[u64],
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&version.to_le_bytes());
        buf.extend_from_slice(&skip_bytes.to_le_bytes());
        buf.extend_from_slice(&chunk_size.to_le_bytes());
        buf.extend_from_slice(&offset_size.to_le_bytes());
        buf.extend_from_slice(&uncompressed_size.to_le_bytes());
        buf.extend_from_slice(&compressed_size.to_le_bytes());
        // skip_bytes padding
        buf.extend(std::iter::repeat(0u8).take(skip_bytes as usize));
        for &off in offsets {
            buf.extend_from_slice(&off.to_le_bytes());
        }
        buf
    }

    #[test]
    fn parse_valid_index() {
        // 3 chunks: uncompressed_size=80000, chunk_size=32768
        // ceil(80000/32768) = 3 chunks, so 2 stored offsets
        // offsets[0] = start of chunk 1, offsets[1] = start of chunk 2
        let data = make_index_bytes(1, 0, 32768, 8, 80000, 50000, &[15000, 35000]);
        let idx = SozipIndex::from_bytes(&data).unwrap();
        assert_eq!(idx.version, 1);
        assert_eq!(idx.chunk_size, 32768);
        assert_eq!(idx.uncompressed_size, 80000);
        assert_eq!(idx.compressed_size, 50000);
        assert_eq!(idx.num_chunks(), 3);
        assert_eq!(idx.offsets, vec![15000, 35000]);
    }

    #[test]
    fn parse_with_skip_bytes() {
        // 2 chunks, 1 stored offset
        let data = make_index_bytes(1, 16, 32768, 8, 40000, 20000, &[10000]);
        let idx = SozipIndex::from_bytes(&data).unwrap();
        assert_eq!(idx.num_chunks(), 2);
        assert_eq!(idx.offsets, vec![10000]);
    }

    #[test]
    fn reject_bad_version() {
        let data = make_index_bytes(2, 0, 32768, 8, 40000, 20000, &[10000]);
        assert!(SozipIndex::from_bytes(&data).is_err());
    }

    #[test]
    fn reject_zero_chunk_size() {
        let data = make_index_bytes(1, 0, 0, 8, 40000, 20000, &[]);
        assert!(SozipIndex::from_bytes(&data).is_err());
    }

    #[test]
    fn reject_bad_offset_size() {
        let data = make_index_bytes(1, 0, 32768, 4, 40000, 20000, &[]);
        assert!(SozipIndex::from_bytes(&data).is_err());
    }

    #[test]
    fn reject_truncated() {
        // 3 chunks needs 2 stored offsets, but we only give 1
        let data = make_index_bytes(1, 0, 32768, 8, 80000, 50000, &[15000]);
        assert!(SozipIndex::from_bytes(&data).is_err());
    }

    #[test]
    fn reject_non_monotonic() {
        let data = make_index_bytes(1, 0, 32768, 8, 80000, 50000, &[35000, 15000]);
        assert!(SozipIndex::from_bytes(&data).is_err());
    }

    #[test]
    fn parse_foo_zip_index() {
        // Exact bytes from foo.zip: chunk_size=2, uncompressed=3, compressed=16
        // 2 chunks, 1 stored offset (13 = start of chunk 1)
        let data = make_index_bytes(1, 0, 2, 8, 3, 16, &[13]);
        let idx = SozipIndex::from_bytes(&data).unwrap();
        assert_eq!(idx.chunk_size, 2);
        assert_eq!(idx.uncompressed_size, 3);
        assert_eq!(idx.compressed_size, 16);
        assert_eq!(idx.num_chunks(), 2);
        assert_eq!(idx.offsets, vec![13]);

        // Chunk 0: compressed bytes [0, 13)
        assert_eq!(idx.compressed_range(0).unwrap(), (0, 13));
        // Chunk 1: compressed bytes [13, 16)
        assert_eq!(idx.compressed_range(1).unwrap(), (13, 16));
    }

    #[test]
    fn locate_chunk_arithmetic() {
        // 3 chunks, chunk_size=32768, 2 stored offsets
        let data = make_index_bytes(1, 0, 32768, 8, 80000, 50000, &[15000, 35000]);
        let idx = SozipIndex::from_bytes(&data).unwrap();

        // Byte 0 → chunk 0, offset 0
        assert_eq!(idx.locate_chunk(0).unwrap(), (0, 0));

        // Byte 32767 → chunk 0, offset 32767
        assert_eq!(idx.locate_chunk(32767).unwrap(), (0, 32767));

        // Byte 32768 → chunk 1, offset 0
        assert_eq!(idx.locate_chunk(32768).unwrap(), (1, 0));

        // Byte 65536 → chunk 2, offset 0
        assert_eq!(idx.locate_chunk(65536).unwrap(), (2, 0));

        // Byte 79999 → chunk 2, offset 14463
        assert_eq!(idx.locate_chunk(79999).unwrap(), (2, 79999 - 65536));

        // Byte 80000 → out of range
        assert!(idx.locate_chunk(80000).is_err());
    }

    #[test]
    fn compressed_range_arithmetic() {
        // 3 chunks, 2 stored offsets
        let data = make_index_bytes(1, 0, 32768, 8, 80000, 50000, &[15000, 35000]);
        let idx = SozipIndex::from_bytes(&data).unwrap();

        // Chunk 0: implicit start at 0, ends at offsets[0]
        assert_eq!(idx.compressed_range(0).unwrap(), (0, 15000));
        // Chunk 1: starts at offsets[0], ends at offsets[1]
        assert_eq!(idx.compressed_range(1).unwrap(), (15000, 35000));
        // Chunk 2 (last): starts at offsets[1], ends at compressed_size
        assert_eq!(idx.compressed_range(2).unwrap(), (35000, 50000));

        assert!(idx.compressed_range(3).is_err());
    }

    #[test]
    fn locate_chunk_foo() {
        // foo.zip: chunk_size=2, 3 bytes
        let data = make_index_bytes(1, 0, 2, 8, 3, 16, &[13]);
        let idx = SozipIndex::from_bytes(&data).unwrap();

        // Byte 0 → chunk 0, offset 0
        assert_eq!(idx.locate_chunk(0).unwrap(), (0, 0));
        // Byte 1 → chunk 0, offset 1
        assert_eq!(idx.locate_chunk(1).unwrap(), (0, 1));
        // Byte 2 → chunk 1, offset 0
        assert_eq!(idx.locate_chunk(2).unwrap(), (1, 0));
        // Byte 3 → out of range
        assert!(idx.locate_chunk(3).is_err());
    }

    #[test]
    fn index_filename_no_dir() {
        assert_eq!(index_filename("mydata.gpkg"), ".mydata.gpkg.sozip.idx");
    }

    #[test]
    fn index_filename_with_dir() {
        assert_eq!(
            index_filename("subdir/mydata.gpkg"),
            "subdir/.mydata.gpkg.sozip.idx"
        );
    }

    #[test]
    fn index_filename_nested_dir() {
        assert_eq!(
            index_filename("a/b/c.shp"),
            "a/b/.c.shp.sozip.idx"
        );
    }
}
