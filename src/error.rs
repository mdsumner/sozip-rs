use thiserror::Error;

#[derive(Debug, Error)]
pub enum SozipError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("invalid SOZip index: {reason}")]
    InvalidIndex { reason: String },

    #[error("no SOZip index found for entry '{name}'")]
    NoIndex { name: String },

    #[error("entry '{name}' not found in archive")]
    EntryNotFound { name: String },

    #[error("entry '{name}' is not Deflate-compressed (method {method})")]
    NotDeflate { name: String, method: u16 },

    #[error("seek to offset {offset} is beyond uncompressed size {size}")]
    SeekOutOfRange { offset: u64, size: u64 },

    #[error("decompression error: {0}")]
    Inflate(String),

    #[error("index/entry size mismatch: index says {index_size}, entry says {entry_size}")]
    SizeMismatch { index_size: u64, entry_size: u64 },
}
