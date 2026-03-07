//! # sozip — Seek-Optimized ZIP reader
//!
//! A Rust implementation of the [SOZip specification](https://github.com/sozip/sozip-spec),
//! enabling random access within Deflate-compressed files inside ZIP archives.
//!
//! SOZip is a profile of the ZIP format: a SOZip file is a valid ZIP readable by
//! any standard tool. The optimisation is additive — a hidden chunk index stored
//! inside the archive allows SOZip-aware readers to seek to an arbitrary
//! uncompressed byte offset by fetching and decompressing only the relevant chunk.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────┐
//! │   zip crate   │  ← central directory parsing, file enumeration
//! └──────┬───────┘
//!        │
//! ┌──────┴───────┐
//! │  sozip::index │  ← parse 32-byte header + offset array (pure, no I/O)
//! └──────┬───────┘
//!        │
//! ┌──────┴───────┐
//! │ sozip::reader │  ← chunk seek arithmetic, inflate, cached reads
//! └──────┬───────┘
//!        │
//! ┌──────┴───────┐
//! │sozip::archive │  ← high-level: open ZIP, detect SOZip, return readers
//! └──────────────┘
//! ```
//!
//! ## Phase 1 (current): local file, synchronous
//!
//! Uses the `zip` crate for ZIP structure parsing and provides synchronous
//! `Read + Seek` over SOZip-enabled entries.
//!
//! ## Phase 2 (planned): async over object_store
//!
//! Will add `AsyncRead + AsyncSeek` backed by `object_store::GetRange`,
//! enabling range-request access to SOZip files on S3/GCS/Azure/HTTP.

pub mod error;
pub mod index;
pub mod reader;
pub mod archive;

pub use error::SozipError;
pub use index::SozipIndex;
pub use archive::SozipArchive;
pub use reader::SozipReader;
