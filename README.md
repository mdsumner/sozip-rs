# sozip

Seek-Optimized ZIP (SOZip) reader for Rust.

A Rust implementation of the [SOZip specification](https://github.com/sozip/sozip-spec)
(v0.5), enabling random access within Deflate-compressed files inside ZIP archives
without prior decompression.

## What is SOZip?

A SOZip file is a standard ZIP file with an additive optimisation: a hidden chunk
index that allows seeking to any uncompressed byte offset by fetching and
decompressing only the relevant chunk. Non-SOZip-aware tools read the file normally.

The canonical implementations are in [GDAL](https://gdal.org) (C++) and
[sozipfile](https://github.com/sozip/sozipfile) (Python). 

## Status

**Phase 1**: local file, synchronous `Read + Seek`. Works today.

**Phase 2** (planned): async `AsyncRead + AsyncSeek` over
[`object_store`](https://docs.rs/object_store/), enabling range-request access
to SOZip files on S3/GCS/Azure/HTTP.

## Usage

```rust
use sozip::{SozipArchive, SozipReader, SozipIndex};
use std::io::{Read, Seek, SeekFrom, BufReader};
use std::fs::File;

// Open archive, discover SOZip-enabled entries
let archive = SozipArchive::open("data.zip")?;
for name in archive.sozip_entries() {
    println!("{name} is SOZip-enabled");
}

// Get entry info and create a reader for direct access
let info = archive.entry_info("mydata.gpkg").unwrap();
let file = BufReader::new(File::open("data.zip")?);
let mut reader = SozipReader::new(file, info.data_start, info.index.clone());

// Seek + read like any file
reader.seek(SeekFrom::Start(1_000_000))?;
let mut buf = [0u8; 4096];
reader.read(&mut buf)?;
```

## Architecture

```
zip crate        ← central directory parsing, file enumeration
    │
sozip::index     ← parse 32-byte header + offset array (pure, no I/O)
    │
sozip::reader    ← chunk seek arithmetic, inflate, cached reads
    │
sozip::archive   ← open ZIP, detect SOZip entries, return readers
```

## Relationship to the broader stack

```
object_store  (Rust — cloud I/O)
    ├── async-tiff       →  rustycogs (R) — COG tiles
    ├── sozip (this)     →  robstore (R)  — ZIP-wrapped anything
    └── robstore (R)     — generic object store
```

## Reference implementations

| Implementation        | Language | Async | Status     |
|-----------------------|----------|-------|------------|
| GDAL `/vsizip/`      | C++      | No    | Production |
| sozipfile             | Python   | No    | Production |
| **sozip (this crate)**| **Rust** | Phase 2 | **Active** |

## License

MIT
