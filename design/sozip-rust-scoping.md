# SOZip in Rust: a brief on prospects and roadmap

*March 2026*

---

## Background

SOZip (Seek-Optimized ZIP) is a profile of the ZIP format designed by Even
Rouault (Spatialys, 2023) that enables random access within a Deflate-compressed
file without prior decompression. It is not a new format — a SOZip file is a
valid ZIP readable by any standard ZIP tool. The optimisation is additive: a
hidden chunk index stored in the ZIP extra field allows SOZip-aware readers to
seek to an arbitrary uncompressed byte offset by fetching and decompressing only
the relevant chunk.

The canonical implementation is in GDAL (C++, GDAL >= 3.7), with a Python
`sozipfile` module as a drop-in replacement for the standard library `zipfile`.
Both are synchronous and neither uses object_store or any cloud-native I/O layer.

**There is currently no Rust implementation.** The only reference to SOZip in the
Rust crate ecosystem is a comment in the `geomedea` crate (Michael Kirk, 2023),
noting SOZip as a potential solution to the compressed-FlatGeobuf range-access
problem but observing it had not yet been demonstrated over the network.

---

## Why this matters

The TIF-in-ZIP convention is one of the most persistent anti-patterns in
geospatial data distribution. It predates cloud storage, originates from the
sidecar-file era (.tab, .tfw, .prj alongside .tif), and has been reinforced by
decades of toolchain inertia. Enormous legacy archives ship data this way.
Converting them to COG is the right long-term answer but is not always feasible:
files may be in institutional custody, conversion pipelines are expensive at
scale, and consumers may need to support both old and new formats simultaneously.

SOZip is the upgrade path that requires no format conversion and no consumer
breakage. A `sozip` tool re-wraps an existing ZIP in place; the file remains a
valid ZIP; existing consumers are unaffected; SOZip-aware consumers get
range access. For geospatial data specifically this unlocks:

- **FlatGeobuf in ZIP** — bbox queries over HTTP without full download, closing
  the gap identified by michaelkirk in flatgeobuf/flatgeobuf#94
- **GeoPackage in ZIP** — direct SQLite access over HTTP via SOZip chunking
- **Shapefile in ZIP** — the `.shp`/`.dbf`/`.shx` bundle, still ubiquitous in
  government data portals, becomes range-accessible
- **Legacy TIFF in ZIP** — not COG, but with SOZip at least the TIFF bytes are
  seekable; combine with a stripped-TIFF reader for partial access
- **Any large file in ZIP** — SOZip is deliberately format-agnostic

The cloud-native geospatial stack (object_store → async-tiff → rustycogs,
robstore) currently has no answer for ZIP-wrapped data. A Rust SOZip crate fills
that gap and sits naturally in the same dependency graph.

---

## What a Rust port has to take on

### 1. ZIP structure parsing

ZIP's central directory is at the *end* of the file — a deliberate historical
choice for streaming append that is the root of the cloud-access problem. Reading
a ZIP therefore requires:

- Fetching the last ~64KB to locate the End of Central Directory (EOCD) record
- Parsing the central directory to enumerate files and their local header offsets
- For ZIP64, handling the ZIP64 EOCD locator and extended fields

The `zip` crate on crates.io covers this for local files. For async range-request
access the ZIP parsing itself needs to be async and built on object_store, which
the existing `zip` crate does not support. This is the first non-trivial piece of
work: an async ZIP reader backed by object_store.

### 2. SOZip index parsing

The SOZip chunk index is stored as a hidden file inside the ZIP, named after the
target file with a `.sozip.idx` suffix (e.g. `mydata.gpkg.sozip.idx`). The index
is a binary array of `uint64` values giving the compressed byte offset of each
chunk boundary within the local file data. Parsing it is straightforward once the
ZIP structure is readable.

The extra field in the local file header also carries a SOZip marker
(`0x564b` KeyValuePairs extension) that identifies the file as SOZip-optimised
and records the chunk size. This is the fast-path check before fetching the index.

### 3. Chunk seek arithmetic

Given an uncompressed byte offset `pos`:

```
chunk_index  = pos / chunk_size          // which chunk
chunk_offset = pos % chunk_size          // offset within decompressed chunk
compressed_start = index[chunk_index]    // from the SOZip index
compressed_end   = index[chunk_index+1]  // or EOF of compressed data
```

Fetch `compressed_start..compressed_end` via `object_store.get_range()`, then
decompress with `flate2` (or `libdeflate` for better performance), then seek to
`chunk_offset` within the decompressed output. Each chunk is independently
decompressible because SOZip inserts full deflate flushes at chunk boundaries.

### 4. Async streaming read

The consumer interface needs to look like an `AsyncRead` + `AsyncSeek` over the
logical uncompressed byte stream. This is the API that downstream crates
(FlatGeobuf, GeoPackage reader, etc.) would program against. Under the hood it
maintains:

- current uncompressed position
- cached decompressed chunk (avoid re-fetching for sequential reads within a chunk)
- the SOZip index (fetched once, kept in memory — it is small)

### 5. Validation and fallback

A SOZip-aware reader must gracefully handle non-SOZip ZIPs — fall back to full
download if no SOZip index is present. The `--validate` functionality from the
GDAL `sozip` tool (checking index integrity, reporting chunk size) is useful for
diagnostics but not required for an initial read-only implementation.

### 6. Write support (later)

Writing a SOZip-enabled ZIP requires inserting full deflate flushes at chunk
boundaries during compression, building the index array, and writing the hidden
`.sozip.idx` file into the archive before the central directory. This is
separable from read support and can be deferred.

---

## Proposed roadmap

**Phase 1 — async ZIP reader on object_store** *(the foundation)*

- Async central directory parsing via tail-fetch + range requests
- File enumeration: list files, get local header offsets and compressed sizes
- `get_range` on a named file within the ZIP (fetch + decompress a slice)
- This is independently useful regardless of SOZip

**Phase 2 — SOZip index read and chunk seek**

- Detect SOZip marker in extra field
- Fetch and parse `.sozip.idx`
- Implement chunk seek arithmetic
- `AsyncRead` + `AsyncSeek` impl over the uncompressed stream

**Phase 3 — integration**

- R bindings via robstore: `ob_get_range` on a `.zip` path transparently uses
  SOZip if available
- FlatGeobuf integration: demonstrate bbox query over HTTP on a SOZip-enabled
  `.fgb.zip`
- Benchmark against GDAL `/vsizip/` on the same files

**Phase 4 — write support**

- SOZip-enabled compression with configurable chunk size
- `--optimize-from` equivalent: re-wrap an existing ZIP as SOZip

Finally, this would then seek interest in inclusion in the sozip org: https://github.com/sozip
but that will only be explored once full validation has been performed on the software in many
contexts.

---

## Relationship to the broader stack

```
object_store  (Rust crate — I/O)
    ├── async-tiff       →  rustycogs (R) — COG tiles
    ├── sozip (proposed) →  robstore (R)  — ZIP-wrapped anything
    └── robstore (R)     — generic object store
```

SOZip sits at the same level as async-tiff: a format-aware layer on top of
object_store, exposing a seekable byte stream rather than decoded arrays. It is
deliberately format-agnostic at the Rust level; the geospatial interpretation
(FlatGeobuf features, GeoPackage rows) is the responsibility of whatever reads
the byte stream.

The existing Python `sozipfile` module and GDAL implementation are the reference
for correctness. The GDAL implementation in particular (`cpl_vsil_sozip.cpp`) is
well-commented and is the authoritative guide to the index format and chunk
boundary arithmetic.

---

## Prior art summary

| Implementation | Language | Async | object_store | Status |
|---|---|---|---|---|
| GDAL `/vsizip/` + sozip | C++ | No | No (VSI layer) | Production |
| sozipfile | Python | No | No | Production |
| geomedea (mention only) | Rust | — | — | Not implemented |
| **proposed** | **Rust** | **Yes** | **Yes** | **To be written** |

The gap is real, the design is clear, and the pieces are all available. The GDAL
implementation is the existence proof; the work is translating it into async Rust
on a modern I/O foundation.
