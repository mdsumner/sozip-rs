use std::io::{Read, Seek, SeekFrom};
use std::fs::File;
use std::io::BufReader;

#[test]
fn inspect_foo_zip() {
  let path = "../sozip-examples/foo.zip";
  let file = File::open(path).expect("need ../sozip-examples/foo.zip");
  let mut archive = zip::ZipArchive::new(BufReader::new(file)).unwrap();

  println!("entries in central directory: {}", archive.len());
  for i in 0..archive.len() {
    let entry = archive.by_index_raw(i).unwrap();
    println!(
        "  [{}] name={:?} compressed={} uncompressed={} method={:?} data_start={}",
        i,
        entry.name(),
        entry.compressed_size(),
        entry.size(),
        entry.compression(),
        entry.data_start(),
    );
  }
}

#[test]
fn read_foo_zip_sozip() {
    let path = "../sozip-examples/foo.zip";
    let archive = sozip::SozipArchive::open(path).unwrap();

    assert!(archive.is_sozip("foo"));
    let info = archive.entry_info("foo").unwrap();
    assert_eq!(info.index.chunk_size, 2);
    assert_eq!(info.index.uncompressed_size, 3);
    // ceil(3/2) = 2 chunks
    assert_eq!(info.index.num_chunks(), 2);

    // Now read the actual content via SozipReader
    let file = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let mut reader = sozip::SozipReader::new(file, info.data_start, info.index.clone());

    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).unwrap();
    assert_eq!(&buf, b"foo");

    // Test seeking: read byte 2 (the 'o')
    reader.seek(SeekFrom::Start(2)).unwrap();
    let mut single = [0u8; 1];
    reader.read_exact(&mut single).unwrap();
    assert_eq!(single[0], b'o');
}

#[test]
fn foo_zip_roundtrip() {
    let path = "../sozip-examples/foo.zip";
    let archive = sozip::SozipArchive::open(path).unwrap();

    assert!(archive.is_sozip("foo"));
    let info = archive.entry_info("foo").unwrap();
    assert_eq!(info.index.chunk_size, 2);
    assert_eq!(info.index.uncompressed_size, 3);
    assert_eq!(info.index.num_chunks(), 2);

    // Read via SozipReader
    let file = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let mut reader = sozip::SozipReader::new(file, info.data_start, info.index.clone());

    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut buf).unwrap();
    assert_eq!(&buf, b"foo");
}


#[test]
fn shp_zip_discovery() {
    let path = "../sozip-examples/nz-building-outlines-extract.shp.zip";
    let archive = sozip::SozipArchive::open(path).unwrap();

    let sozip_names = archive.sozip_entries();
    println!("SOZip entries: {:?}", sozip_names);
    assert!(!sozip_names.is_empty());

    for result in archive.validate() {
        println!("{}", result);
    }
}

#[test]
fn shp_zip_seek_read() {
    let path = "../sozip-examples/nz-building-outlines-extract.shp.zip";
    let archive = sozip::SozipArchive::open(path).unwrap();

    // Pick the .shp entry — it'll be the biggest
    let shp_name = archive.sozip_entries().into_iter()
        .find(|n| n.ends_with(".shp"))
        .expect("no .shp entry found");

    let info = archive.entry_info(shp_name).unwrap();
    println!(
        "{}: chunk_size={}, chunks={}, uncompressed={}",
        shp_name, info.index.chunk_size,
        info.index.num_chunks(), info.uncompressed_size
    );

    let file = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let mut reader = sozip::SozipReader::new(
        file, info.data_start, info.index.clone()
    );

    // Read the shapefile header (first 100 bytes, always fixed format)
    let mut header = [0u8; 100];
    reader.read_exact(&mut header).unwrap();

    // Shapefile magic number: 0x0000270a (9994) in big-endian at offset 0
    let magic = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    assert_eq!(magic, 9994, "not a valid shapefile header");

    // Seek to middle of file and read — exercises cross-chunk seek
    let mid = info.uncompressed_size / 2;
    reader.seek(std::io::SeekFrom::Start(mid)).unwrap();
    let mut buf = [0u8; 4096];
    let n = reader.read(&mut buf).unwrap();
    assert!(n > 0);

    // Seek back to start, re-read header, should match
    reader.seek(std::io::SeekFrom::Start(0)).unwrap();
    let mut header2 = [0u8; 100];
    reader.read_exact(&mut header2).unwrap();
    assert_eq!(header, header2);
}
