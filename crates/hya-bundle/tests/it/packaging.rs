//! Public package inspection, strict archive validation, deterministic writing, and staging.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU64, Ordering};

use hya_bundle::{
    BundleError, BundleSource, PackageFormat, PreparedCatalog, SourceFile,
    cleanup_orphaned_staging, detect_package_format, inspect_public_package, prepare_package,
    stage_package, write_public_package,
};
use sevenz_rust2::{
    ArchiveEntry, ArchiveReader, ArchiveReaderOptions, ArchiveWriter, EncoderConfiguration,
    EncoderMethod, Password,
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

// Deterministically generated with vendored sevenz-rust2 0.20.2: one non-solid
// LZMA2+CRC bundle.hya.md over a 320,000-byte repetitive prepare-valid manifest.
const PUBLIC_RATIO_PREFLIGHT_LZMA2: [u8; 409] = [
    0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c, 0x00, 0x04, 0x65, 0xbc, 0xe4, 0xd6, 0x30, 0x01, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x49, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xaf, 0x8b, 0x81, 0x17,
    0xe4, 0xe1, 0xff, 0x01, 0x28, 0x5d, 0x00, 0x16, 0xe7, 0xfe, 0x8c, 0xa7, 0xf9, 0x94, 0x7d, 0xf6,
    0xef, 0x5a, 0x36, 0x8e, 0x58, 0x0a, 0xc4, 0xe4, 0xdf, 0x06, 0x7b, 0x8e, 0x8d, 0x3f, 0x31, 0xe3,
    0xc4, 0xc5, 0x98, 0x4e, 0x6b, 0x48, 0x3e, 0xcb, 0x8c, 0x16, 0xd2, 0x2c, 0x2b, 0x69, 0x1f, 0x0f,
    0xc2, 0x74, 0x83, 0x8c, 0xc1, 0xcb, 0x3b, 0xb3, 0x5e, 0x8f, 0x1f, 0xa5, 0xbd, 0x27, 0xad, 0xe3,
    0x0c, 0xbf, 0xd2, 0x12, 0xf1, 0x5f, 0x79, 0x5b, 0xde, 0x93, 0xc2, 0xe5, 0xbf, 0xbb, 0xd8, 0x04,
    0x8c, 0x06, 0x0e, 0xea, 0x2e, 0xb1, 0x73, 0x0a, 0x7f, 0x98, 0x4d, 0x11, 0x32, 0xd2, 0xba, 0x59,
    0x24, 0x1c, 0x90, 0xdc, 0xce, 0xb5, 0x7a, 0xfc, 0x9d, 0x22, 0x22, 0xb7, 0xc2, 0xec, 0x2c, 0xd2,
    0xc8, 0x10, 0xd6, 0xfb, 0xcc, 0xfb, 0x7d, 0x78, 0xaf, 0x39, 0x66, 0x57, 0xf7, 0xea, 0xaa, 0x1e,
    0x82, 0x8c, 0x2c, 0x6f, 0x2e, 0xf0, 0x66, 0x73, 0x4a, 0x11, 0x1f, 0xba, 0xf5, 0x74, 0x32, 0x6a,
    0xbf, 0xa9, 0x96, 0xd5, 0xbf, 0xc4, 0x71, 0x09, 0x65, 0x9a, 0x29, 0xc8, 0x0d, 0x17, 0x44, 0xe8,
    0x3a, 0x1d, 0xb8, 0xdf, 0x66, 0x56, 0x74, 0x2b, 0x4a, 0x0d, 0x2d, 0xf1, 0x12, 0xa8, 0x5f, 0xe9,
    0x84, 0x8b, 0xd7, 0xc2, 0x2e, 0xc4, 0x8f, 0x26, 0x7d, 0x0f, 0xc6, 0x41, 0xf1, 0xfa, 0x39, 0xfa,
    0xb6, 0xdf, 0x3b, 0x78, 0x6b, 0x19, 0xe5, 0x21, 0xa7, 0xf1, 0x4b, 0xd3, 0xec, 0xae, 0x80, 0x5b,
    0x62, 0x94, 0x1d, 0x7e, 0xc5, 0xb8, 0x29, 0xc2, 0x07, 0x07, 0xd6, 0x5a, 0x4e, 0x05, 0xfb, 0x1f,
    0xf9, 0x95, 0xe0, 0xcb, 0xca, 0x11, 0x4c, 0x27, 0x77, 0x87, 0x62, 0x03, 0x70, 0xb1, 0xfb, 0x9f,
    0x4c, 0x0f, 0x9b, 0xac, 0x5b, 0x7c, 0x36, 0x2b, 0x6e, 0xdc, 0x52, 0x4c, 0x5e, 0xd0, 0x91, 0x01,
    0xd9, 0x3f, 0x5d, 0xec, 0x27, 0xc5, 0xca, 0xe4, 0x37, 0x92, 0x3a, 0x6b, 0x4c, 0x42, 0x2c, 0xcc,
    0x43, 0x49, 0xc7, 0x5f, 0xfa, 0xfb, 0xc8, 0x4f, 0xe8, 0x13, 0x3d, 0xb7, 0xe3, 0x80, 0xba, 0x23,
    0xa8, 0xf8, 0x06, 0x8b, 0xd9, 0xf5, 0x7f, 0xd6, 0x0d, 0x1b, 0x06, 0xaa, 0xf5, 0x76, 0x00, 0x00,
    0x01, 0x04, 0x06, 0x00, 0x01, 0x09, 0x81, 0x30, 0x0a, 0x01, 0xe5, 0x92, 0x3a, 0x92, 0x00, 0x07,
    0x0b, 0x01, 0x00, 0x01, 0x21, 0x21, 0x01, 0x16, 0x0c, 0xc4, 0x00, 0xe2, 0x00, 0x08, 0x0a, 0x01,
    0xaa, 0x5e, 0x60, 0x05, 0x00, 0x00, 0x05, 0x01, 0x11, 0x1d, 0x00, 0x62, 0x00, 0x75, 0x00, 0x6e,
    0x00, 0x64, 0x00, 0x6c, 0x00, 0x65, 0x00, 0x2e, 0x00, 0x68, 0x00, 0x79, 0x00, 0x61, 0x00, 0x2e,
    0x00, 0x6d, 0x00, 0x64, 0x00, 0x00, 0x00, 0x00, 0x00,
];

const PUBLIC_ARCHIVE_MANIFEST: &[u8] = br#"---
kind: Bundle
schema_version: 2
identity:
  id: hya/archive-js
  version: 1.0.0
  publisher: hya
runtime:
  kind: js
  mode: managed
  entry: extensions/runtime.js
resources:
  tools:
    - id: echo
      path: extensions/runtime.js
      aliases:
        - say
  hooks:
    - id: event
      path: extensions/runtime.js
agents:
  - id: lead
    role: main
    resource_view:
      allow:
        - echo
        - runtime
    hook_refs:
      - bundle:hya/archive-js/hook/event
---
Archive JS lead.
"#;

const FIXTURE_MANIFEST: &[u8] = br#"---
kind: Bundle
schema_version: 2
identity:
  id: hya/valid-public
  version: 1.0.0
  publisher: hya
agents:
  - id: valid-public-lead
    role: main
---

You are the valid public bundle lead.
"#;

const FIXTURE_JS_MANIFEST: &[u8] = br#"---
kind: Bundle
schema_version: 2
identity:
  id: hya/archive-js
  version: 1.0.0
  publisher: hya
runtime:
  kind: js
  mode: managed
  entry: extensions/runtime.js
resources:
  tools:
    - id: echo
      path: extensions/runtime.js
      aliases:
        - say
  hooks:
    - id: event
      path: extensions/runtime.js
agents:
  - id: archive-js-lead
    role: main
    resource_view:
      allow:
        - echo
    hook_refs:
      - event
---

Archive JS lead.
"#;

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "hya-bundle-pkg-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn build_archive(methods: Vec<EncoderConfiguration>, entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = ArchiveWriter::new(Cursor::new(Vec::new())).expect("writer");
    writer.set_encrypt_header(false);
    writer.set_content_methods(methods);
    for (name, bytes) in entries {
        writer
            .push_archive_entry(ArchiveEntry::new_file(name), Some(Cursor::new(*bytes)))
            .expect("push entry");
    }
    writer.finish().expect("finish").into_inner()
}

fn public_copy_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    build_archive(
        vec![EncoderConfiguration::new(EncoderMethod::COPY)],
        entries,
    )
}

#[test]
fn package_format_detection_and_size_header_limits() {
    let private = b"HYABNDL\0\x01\0";
    assert_eq!(
        detect_package_format(private),
        Err(BundleError::InvalidPackageFormat)
    );
    assert!(matches!(
        inspect_public_package(private),
        Err(BundleError::InvalidPackageFormat)
    ));

    let magic = [0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c];
    assert_eq!(detect_package_format(&magic), Ok(PackageFormat::PublicV1));

    let mut huge = vec![0_u8; 128 * 1024 * 1024 + 1];
    huge[..6].copy_from_slice(&magic);
    assert!(matches!(
        inspect_public_package(&huge),
        Err(BundleError::PackageLimitExceeded { limit, .. }) if limit == "archive bytes"
    ));

    let next_header_size = 8 * 1024 * 1024 + 1;
    let mut bad_hdr = vec![0_u8; 32 + next_header_size];
    bad_hdr[..6].copy_from_slice(&magic);
    bad_hdr[6..8].copy_from_slice(&[0, 4]);
    bad_hdr[8..12].copy_from_slice(&0x7275_dee3_u32.to_le_bytes());
    bad_hdr[12..20].copy_from_slice(&0_u64.to_le_bytes());
    bad_hdr[20..28].copy_from_slice(&(next_header_size as u64).to_le_bytes());
    bad_hdr[28..32].copy_from_slice(&0_u32.to_le_bytes());
    assert!(matches!(
        inspect_public_package(&bad_hdr),
        Err(BundleError::PackageLimitExceeded { limit }) if limit == "next header bytes"
    ));
}

#[test]
fn public_archive_valid_fixtures_and_exact_referenced_closure() {
    assert!(
        inspect_public_package(include_bytes!(
            "../fixtures/packages/valid_public_bundle_copy.7z"
        ))
        .is_ok()
    );
    assert!(
        inspect_public_package(include_bytes!(
            "../fixtures/packages/valid_public_bundle_lzma2_copy.7z"
        ))
        .is_ok()
    );

    let bytes = public_copy_archive(&[
        ("bundle.hya.md", PUBLIC_ARCHIVE_MANIFEST),
        ("extensions/runtime.js", b"export const runtime = true;\n"),
    ]);
    let prepared = inspect_public_package(&bytes).expect("inspect archive");
    let bundle = &prepared.bundles()[0];
    assert_eq!(bundle.tools().len(), 1);
    assert_eq!(bundle.hooks().len(), 1);
    assert_eq!(bundle.extensions().len(), 1);
    assert_eq!(bundle.tools()[0].source_path, "extensions/runtime.js");
    assert_eq!(bundle.tools()[0].content, "export const runtime = true;\n");
    assert_eq!(bundle.hooks()[0].digest, bundle.tools()[0].digest);
    assert_eq!(bundle.extensions()[0].digest, bundle.tools()[0].digest);
}

#[test]
fn public_archive_matches_directory_source_and_ignores_undeclared_helper() {
    let archive_bytes = public_copy_archive(&[
        ("bundle.hya.md", PUBLIC_ARCHIVE_MANIFEST),
        ("extensions/runtime.js", b"export const runtime = true;\n"),
    ]);
    let archive_prepared = inspect_public_package(&archive_bytes).expect("archive");

    let temp = TempDir::new("parity");
    fs::create_dir_all(temp.0.join("extensions")).expect("mkdir");
    fs::write(temp.0.join("bundle.hya.md"), PUBLIC_ARCHIVE_MANIFEST).expect("write manifest");
    fs::write(
        temp.0.join("extensions/runtime.js"),
        b"export const runtime = true;\n",
    )
    .expect("write runtime");
    fs::write(
        temp.0.join("extensions/helper.js"),
        b"export const helper = true;\n",
    )
    .expect("write helper");
    let dir_prepared = prepare_package(BundleSource::read_directory(&temp.0).expect("read dir"))
        .expect("prep dir");

    for prepared in [&archive_prepared, &dir_prepared] {
        let bundle = &prepared.bundles()[0];
        for resource in bundle
            .tools()
            .iter()
            .chain(bundle.skills().iter())
            .chain(bundle.mcp().iter())
            .chain(bundle.hooks().iter())
            .chain(bundle.extensions().iter())
        {
            assert_ne!(resource.source_path, "extensions/helper.js");
        }
        assert_ne!(
            bundle.agents()[0].prompt_source.as_deref(),
            Some("extensions/helper.js")
        );
    }
    assert_eq!(archive_prepared.bytes(), dir_prepared.bytes());
    assert_eq!(archive_prepared.digest(), dir_prepared.digest());
    assert_eq!(archive_prepared.bundles(), dir_prepared.bundles());
    assert_eq!(archive_prepared.index(), dir_prepared.index());
}

#[test]
fn public_archive_rejects_unsafe_layouts_and_missing_files() {
    // Non-bundle root copy fixture and upstream extra-entry fixture.
    let non_bundle = include_bytes!("../fixtures/packages/non_bundle_root_copy.7z");
    assert_eq!(
        detect_package_format(non_bundle),
        Ok(PackageFormat::PublicV1)
    );
    assert!(matches!(
        inspect_public_package(non_bundle),
        Err(BundleError::UnsafePackage)
    ));

    let two_empty = include_bytes!("../fixtures/packages/upstream_two_empty_file.7z");
    let reader = ArchiveReader::new_with_options(
        Cursor::new(two_empty),
        Password::empty(),
        ArchiveReaderOptions::strict(),
    )
    .expect("strict parses extra-entry fixture");
    assert_eq!(reader.archive().files.len(), 2);
    assert!(matches!(
        inspect_public_package(two_empty),
        Err(BundleError::UnsafePackage)
    ));

    // Unreferenced regular file, wrapper root, and ASCII case-colliding paths.
    let unreferenced = public_copy_archive(&[
        ("bundle.hya.md", PUBLIC_ARCHIVE_MANIFEST),
        ("extensions/runtime.js", b"export const runtime = true;\n"),
        ("notes.txt", b"unreferenced\n"),
    ]);
    assert!(matches!(
        inspect_public_package(&unreferenced),
        Err(BundleError::UnsafePackage)
    ));

    let wrapper = public_copy_archive(&[("wrapper/bundle.hya.md", PUBLIC_ARCHIVE_MANIFEST)]);
    assert!(matches!(
        inspect_public_package(&wrapper),
        Err(BundleError::UnsafePackage)
    ));

    let case_manifest = b"---\nkind: Bundle\nidentity: { id: hya/archive-case, version: 1.0.0, publisher: hya }\nresources:\n  tools:\n    - { id: echo, path: tools/echo.js }\n    - { id: upper-echo, path: Tools/Echo.js }\nagents: [{ id: lead, role: main, resource_view: { allow: [echo, upper-echo] } }]\n---\nCase lead.\n";
    let case_clash = public_copy_archive(&[
        ("bundle.hya.md", case_manifest),
        ("tools/echo.js", b"export const echo = true;\n"),
        ("Tools/Echo.js", b"export const upperEcho = true;\n"),
    ]);
    assert!(matches!(
        inspect_public_package(&case_clash),
        Err(BundleError::UnsafePackage)
    ));

    // Missing referenced file.
    let missing = public_copy_archive(&[("bundle.hya.md", PUBLIC_ARCHIVE_MANIFEST)]);
    assert!(matches!(
        inspect_public_package(&missing),
        Err(BundleError::MissingReference { path, .. }) if path == "extensions/runtime.js"
    ));
}

#[test]
fn public_archive_rejects_traversal_duplicate_and_directory_entries() {
    let traversal = public_copy_archive(&[
        (
            "bundle.hya.md",
            b"---\nkind: Bundle\nidentity: { id: hya/archive-traversal, version: 1.0.0, publisher: hya }\nresources:\n  tools: [{ id: escape, path: ../escape.js }]\nagents: [{ id: lead, role: main, resource_view: { allow: [escape] } }]\n---\nLead.\n",
        ),
        ("../escape.js", b"export const escape = true;\n"),
    ]);
    assert!(matches!(
        inspect_public_package(&traversal),
        Err(BundleError::InvalidPackageFormat)
    ));

    let duplicate = public_copy_archive(&[
        (
            "bundle.hya.md",
            b"---\nkind: Bundle\nidentity: { id: hya/archive-duplicate, version: 1.0.0, publisher: hya }\nresources:\n  tools: [{ id: echo, path: tools/echo.js }]\nagents: [{ id: lead, role: main, resource_view: { allow: [echo] } }]\n---\nLead.\n",
        ),
        ("tools/echo.js", b"export const echo = true;\n"),
        ("tools/echo.js", b"export const dup = true;\n"),
    ]);
    assert!(matches!(
        inspect_public_package(&duplicate),
        Err(BundleError::InvalidPackageFormat)
    ));

    let mut dir_writer = ArchiveWriter::new(Cursor::new(Vec::new())).expect("writer");
    dir_writer.set_encrypt_header(false);
    dir_writer.set_content_methods(vec![EncoderConfiguration::new(EncoderMethod::COPY)]);
    dir_writer
        .push_archive_entry(
            ArchiveEntry::new_file("bundle.hya.md"),
            Some(Cursor::new(PUBLIC_ARCHIVE_MANIFEST)),
        )
        .expect("manifest");
    dir_writer
        .push_archive_entry(ArchiveEntry::new_directory("tools"), None::<Cursor<&[u8]>>)
        .expect("dir");
    let with_dir = dir_writer.finish().expect("finish").into_inner();
    assert!(matches!(
        inspect_public_package(&with_dir),
        Err(BundleError::InvalidPackageFormat)
    ));
}

#[test]
fn public_archive_rejects_corrupt_streams_and_trailing_bytes() {
    let mut trailing = include_bytes!("../fixtures/packages/valid_public_bundle_copy.7z").to_vec();
    trailing.push(0);
    assert!(matches!(
        inspect_public_package(&trailing),
        Err(BundleError::CorruptPackage)
    ));

    let mut crc_flip = include_bytes!("../fixtures/packages/valid_public_bundle_copy.7z").to_vec();
    crc_flip[32] ^= 0x01;
    assert_eq!(
        detect_package_format(&crc_flip),
        Ok(PackageFormat::PublicV1)
    );
    assert!(matches!(
        inspect_public_package(&crc_flip),
        Err(BundleError::CorruptPackage)
    ));

    let mut after_hdr = include_bytes!("../fixtures/packages/strict_reader_copy.7z").to_vec();
    after_hdr.push(0);
    after_hdr[20..28].copy_from_slice(&71_u64.to_le_bytes());
    after_hdr[28..32].copy_from_slice(&0x7bc7_9e5f_u32.to_le_bytes());
    after_hdr[8..12].copy_from_slice(&0x259c_eb47_u32.to_le_bytes());
    assert_eq!(after_hdr.len(), 397);
    assert!(matches!(
        inspect_public_package(&after_hdr),
        Err(BundleError::CorruptPackage)
    ));

    let mut extra_decoded = include_bytes!("../fixtures/packages/strict_reader_copy.7z").to_vec();
    extra_decoded.insert(326, 0xff);
    extra_decoded[12..20].copy_from_slice(&295_u64.to_le_bytes());
    extra_decoded[334] = 0x27;
    extra_decoded[28..32].copy_from_slice(&0x296c_eb30_u32.to_le_bytes());
    extra_decoded[8..12].copy_from_slice(&0x22b7_847a_u32.to_le_bytes());
    assert_eq!(extra_decoded.len(), 397);
    assert!(matches!(
        inspect_public_package(&extra_decoded),
        Err(BundleError::CorruptPackage)
    ));
}

#[test]
fn public_metadata_preflight_enforces_referenced_packinfo_expansion_ratio() {
    let bytes = PUBLIC_RATIO_PREFLIGHT_LZMA2.as_slice();
    let reader = ArchiveReader::new_with_options(
        Cursor::new(bytes),
        Password::empty(),
        ArchiveReaderOptions::strict(),
    )
    .expect("ratio fixture parses in strict mode");
    let archive = reader.archive();
    assert_eq!(archive.files.len(), 1);
    assert_eq!(archive.blocks.len(), 1);
    assert!(!archive.is_solid);
    let entry = &archive.files[0];
    assert_eq!(entry.name, "bundle.hya.md");
    assert!(entry.has_crc);
    assert_eq!(
        archive.blocks[0]
            .coders
            .iter()
            .map(|c| c.encoder_method_id())
            .collect::<Vec<_>>(),
        vec![EncoderMethod::ID_LZMA2]
    );
    let block_idx = archive.stream_map.file_block_index[0].expect("block");
    let starts = archive.stream_map.block_first_pack_stream_index();
    let start = starts[block_idx];
    let end = starts
        .get(block_idx + 1)
        .copied()
        .unwrap_or(archive.pack_sizes().len());
    let packed_bytes: u64 = archive.pack_sizes()[start..end].iter().copied().sum();
    assert!(u128::from(entry.size) > u128::from(packed_bytes) * 1_000);
    assert!(u128::from(entry.size) <= (bytes.len() as u128) * 1_000);
    assert!(matches!(
        inspect_public_package(bytes),
        Err(BundleError::PackageLimitExceeded { limit }) if limit == "expansion ratio"
    ));
}

#[test]
fn strict_reader_profile_accepts_copy_lzma2_and_rejects_malformed_headers() {
    let base = include_bytes!("../fixtures/packages/non_bundle_root_copy.7z");
    let reader = ArchiveReader::new_with_options(
        Cursor::new(base),
        Password::empty(),
        ArchiveReaderOptions::strict(),
    )
    .expect("audited copy+lzma2 chain parses");
    assert_eq!(
        reader.archive().blocks[0]
            .coders
            .iter()
            .map(|c| c.encoder_method_id())
            .collect::<Vec<_>>(),
        vec![EncoderMethod::ID_LZMA2, EncoderMethod::ID_COPY]
    );
    assert_eq!(reader.archive().files[0].name, "copy.txt");

    let mut trailing = base.to_vec();
    trailing.push(0);
    assert!(
        ArchiveReader::new_with_options(
            Cursor::new(trailing),
            Password::empty(),
            ArchiveReaderOptions::strict()
        )
        .is_err()
    );

    let mut unref_pack = base.to_vec();
    unref_pack.insert(0x3f, 0x00);
    unref_pack[0x3c] = 2;
    unref_pack[20..28].copy_from_slice(&91_u64.to_le_bytes());
    unref_pack[28..32].copy_from_slice(&[0x97, 0x54, 0x60, 0x46]);
    unref_pack[8..12].copy_from_slice(&[0x6b, 0xc8, 0x41, 0x8e]);
    assert_eq!(unref_pack.len(), 147);
    assert!(
        ArchiveReader::new_with_options(
            Cursor::new(unref_pack),
            Password::empty(),
            ArchiveReaderOptions::strict()
        )
        .is_err()
    );

    let mut bad_substream =
        include_bytes!("../fixtures/packages/upstream_two_empty_file.7z").to_vec();
    bad_substream.splice(
        136..143,
        [0x00, 0x08, 0x0a, 0x01, 0x55, 0x27, 0xa7, 0x25, 0x00],
    );
    bad_substream[20..28].copy_from_slice(&34_u64.to_le_bytes());
    bad_substream[28..32].copy_from_slice(&[0x24, 0x63, 0x49, 0x3f]);
    bad_substream[8..12].copy_from_slice(&[0xb8, 0x41, 0xfd, 0xdc]);
    assert_eq!(bad_substream.len(), 146);
    assert!(
        ArchiveReader::new_with_options(
            Cursor::new(bad_substream),
            Password::empty(),
            ArchiveReaderOptions::strict()
        )
        .is_err()
    );
}

#[test]
fn public_writer_round_trips_agent_and_workflow_packages_deterministically() {
    let agent_src = BundleSource::new(
        "public-package",
        vec![SourceFile::new(
            "bundle.hya.md",
            b"---\nkind: Bundle\nschema_version: 2\nidentity: { id: hya/public-package, version: 1.0.0, publisher: hya }\nagents: [{ id: public-package-lead, role: main }]\n---\nYou are the public package lead.\n".as_slice(),
        )],
    );
    let first = write_public_package(&agent_src).expect("write 1");
    assert_eq!(first, write_public_package(&agent_src).expect("write 2"));
    let inspected = inspect_public_package(&first).expect("inspect");
    let prepared = prepare_package(agent_src).expect("prepare");
    assert_eq!(inspected.bytes(), prepared.bytes());
    assert_eq!(inspected.digest(), prepared.digest());
    assert_eq!(
        PreparedCatalog::decode(prepared.bytes(), prepared.digest())
            .expect("decode")
            .bundles()
            .len(),
        1
    );

    let wf_src = BundleSource::new(
        "workflow-public",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"kind: Bundle\nschema_version: 2\nidentity: { id: hya/workflow-public, version: 1.0.0, publisher: hya }\nworkflow: { id: demo, path: workflows/demo.hya.md }\nagents: [{ id: worker, role: subagent, prompt: prompts/worker.md }]\n".as_slice(),
            ),
            SourceFile::new(
                "workflows/demo.hya.md",
                b"---\nkind: Workflow\nname: demo\ndescription: Public Workflow.\nnodes:\n  run:\n    agent: worker\n    directive: Run the stage.\n---\nflowchart TD\n  run\n".as_slice(),
            ),
            SourceFile::new("prompts/worker.md", b"Run the stage.\n".as_slice()),
        ],
    );
    let wf_ins = inspect_public_package(&write_public_package(&wf_src).expect("wf write"))
        .expect("wf inspect");
    let [bundle] = wf_ins.bundles() else {
        panic!("one workflow bundle");
    };
    assert_eq!(bundle.kind().as_str(), "Bundle");
    assert_eq!(bundle.workflow().map(|w| w.id.as_str()), Some("demo"));
    assert_eq!(bundle.agents()[0].id.as_str(), "worker");
}

#[test]
fn e2e_schema_demo_fixture_matches_the_production_package_bytes() {
    let source = BundleSource::new(
        "schema-demo",
        vec![
            SourceFile::new(
                "bundle.yaml",
                b"schema_version: 2\nkind: Bundle\nidentity:\n  id: hya/schema-demo\n  version: 1.0.0\n  publisher: hya\nschemas:\n  - scheme: db\n    tool: query\n    writable: false\nresources:\n  tools:\n    - id: query\n      path: extensions/runtime.js\n  mcp:\n    - id: vecdb\n      path: mcp/vecdb.json\nruntime:\n  kind: js\n  mode: process\n  entry: extensions/runtime.js\n  command: [bun, schema-demo-worker]\nagents:\n  - id: schema-lead\n    role: main\n    resource_view:\n      allow:\n        - query\n        - runtime\n".as_slice(),
            ),
            SourceFile::new("extensions/runtime.js", b"export default {}".as_slice()),
            SourceFile::new("mcp/vecdb.json", b"{\"command\": [\"python3\", \"vecdb.py\"]}".as_slice()),
        ],
    );
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../hya-e2e/tests/fixtures/schema_demo.hyabundle");
    let bytes = write_public_package(&source).expect("schema demo package builds");
    let checked_in = fs::read(&fixture).expect("read checked-in schema_demo.hyabundle");
    assert_eq!(
        checked_in,
        bytes,
        "fixture {} drifted from production package bytes",
        fixture.display()
    );
}

#[test]
fn staged_public_package_permissions_declared_files_and_conservative_cleanup() {
    let fixture = include_bytes!("../fixtures/packages/valid_public_bundle_copy.7z");

    // 1. Private 0o700/0o600 staging, source independence, and foreign preservation.
    let temp = TempDir::new("stage-private");
    let source_path = temp.0.join("source.any");
    fs::write(&source_path, fixture.as_slice()).expect("write src");
    let staging_root = temp.0.join("staging");
    fs::create_dir(&staging_root).expect("mkdir");
    let foreign = staging_root.join("foreign");
    fs::create_dir(&foreign).expect("mkdir foreign");

    let staged = stage_package(&source_path, &staging_root).expect("stage");
    fs::remove_file(&source_path).expect("remove source");
    let owned_children: Vec<_> = fs::read_dir(&staging_root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p != &foreign)
        .collect();
    let [owned_dir] = owned_children.as_slice() else {
        panic!("one owned child");
    };
    assert_eq!(
        fs::symlink_metadata(owned_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let staged_files: Vec<_> = fs::read_dir(owned_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    let [staged_file] = staged_files.as_slice() else {
        panic!("one staged file");
    };
    assert_eq!(
        fs::symlink_metadata(staged_file)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(fs::read(staged_file).unwrap(), fixture.as_slice());
    staged.inspect().expect("inspect staged");
    assert!(!owned_dir.exists() && foreign.is_dir());

    // 2. Conservative cleanup removes only unlocked orphans, ignores active/foreign/missing root.
    let missing_root = temp.0.join("nonexistent-staging");
    cleanup_orphaned_staging(&missing_root).expect("missing root noop");
    assert!(!missing_root.exists());

    let croot = temp.0.join("cleanup-staging");
    fs::create_dir(&croot).expect("mkdir");
    let src2 = temp.0.join("src2.any");
    fs::write(&src2, fixture.as_slice()).expect("write");
    let active = stage_package(&src2, &croot).expect("stage active");
    let active_dir = fs::read_dir(&croot)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let orphan_dir = croot.join(format!("hya-bundle-stage-{}-orphan", std::process::id()));
    fs::create_dir(&orphan_dir).unwrap();
    fs::set_permissions(&orphan_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let orphan_file = orphan_dir.join("package");
    fs::write(&orphan_file, fixture.as_slice()).unwrap();
    fs::set_permissions(&orphan_file, fs::Permissions::from_mode(0o600)).unwrap();
    let cforeign = croot.join("foreign");
    fs::create_dir(&cforeign).unwrap();

    cleanup_orphaned_staging(&croot).expect("cleanup");
    assert!(!orphan_dir.exists());
    assert!(active_dir.is_dir() && active_dir.join("package").is_file() && cforeign.is_dir());
    drop(active);

    // 3. Public inspection returns exactly the declared closure sorted by path.
    let manifest = b"kind: Bundle\nschema_version: 2\nidentity: { id: hya/files, version: 1.0.0, publisher: hya }\nagents: [{ id: files-lead, role: main, prompt: prompts/lead.md }]\n";
    let pkg_src = BundleSource::new(
        "files",
        vec![
            SourceFile::new("bundle.yaml", manifest.as_slice()),
            SourceFile::new("prompts/lead.md", b"Lead.\n".as_slice()),
            SourceFile::new("notes/undeclared.txt", b"not packaged\n".as_slice()),
        ],
    );
    let pkg_path = temp.0.join("files.hyabundle");
    fs::write(&pkg_path, write_public_package(&pkg_src).expect("pkg")).expect("write");
    let public = stage_package(&pkg_path, &staging_root)
        .expect("stage")
        .inspect()
        .expect("inspect");
    let files: Vec<_> = public
        .files
        .iter()
        .map(|f| (f.path().to_string(), f.bytes().to_vec()))
        .collect();
    assert_eq!(
        files,
        vec![
            ("bundle.yaml".to_string(), manifest.to_vec()),
            ("prompts/lead.md".to_string(), b"Lead.\n".to_vec()),
        ]
    );
    let reprepared = prepare_package(BundleSource::new("files", public.files)).expect("reprepare");
    assert_eq!(reprepared.digest(), public.prepared.digest());
}

#[test]
#[ignore = "regenerates checked-in fixtures; run explicitly after a format change"]
fn regenerate_valid_public_package_fixtures() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packages");
    let write =
        |name: &str, bytes: &[u8]| fs::write(root.join(name), bytes).expect("write fixture");

    let plain = public_copy_archive(&[("bundle.hya.md", FIXTURE_MANIFEST)]);
    assert!(inspect_public_package(&plain).is_ok());
    write("valid_public_bundle_copy.7z", &plain);

    let js = public_copy_archive(&[
        ("bundle.hya.md", FIXTURE_JS_MANIFEST),
        ("extensions/runtime.js", b"export const runtime = true;\n"),
    ]);
    assert!(inspect_public_package(&js).is_ok());
    write("valid_public_bundle_js_copy.7z", &js);

    let lzma2 = build_archive(
        vec![
            EncoderConfiguration::new(EncoderMethod::LZMA2),
            EncoderConfiguration::new(EncoderMethod::COPY),
        ],
        &[("bundle.hya.md", FIXTURE_MANIFEST)],
    );
    assert!(inspect_public_package(&lzma2).is_ok());
    write("valid_public_bundle_lzma2_copy.7z", &lzma2);
}
