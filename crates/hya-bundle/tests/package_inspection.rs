use std::io::Cursor;

use hya_bundle::{
    BundleError, PackageFormat, PrivatePackageAuthentication, PrivatePackageInspection,
    PrivatePackagePayload, detect_package_format, inspect_private_package, inspect_public_package,
};
use sevenz_rust2::{ArchiveReader, ArchiveReaderOptions, EncoderMethod, Password};

const PRIVATE_V1_ZERO_CIPHERTEXT_DIGEST: [u8; 32] = [
    0x6e, 0x34, 0x0b, 0x9c, 0xff, 0xb3, 0x7a, 0x98, 0x9c, 0xa5, 0x44, 0xe6, 0xbb, 0x78, 0x0a, 0x2c,
    0x78, 0x90, 0x1d, 0x3f, 0xb3, 0x37, 0x38, 0x76, 0x85, 0x11, 0xa3, 0x06, 0x17, 0xaf, 0xa0, 0x1d,
];

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

fn private_v1_envelope(ciphertext: u8) -> Vec<u8> {
    let target = "x86_64-unknown-linux-gnu";
    let mut bytes = b"HYABNDL\0".to_vec();
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&24_u16.to_le_bytes());
    bytes.extend_from_slice(&12_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u64.to_le_bytes());
    bytes.extend_from_slice(&PRIVATE_V1_ZERO_CIPHERTEXT_DIGEST);
    bytes.extend_from_slice(target.as_bytes());
    bytes.extend_from_slice(&[0; 12]);
    bytes.push(ciphertext);
    bytes.extend_from_slice(&[0; 16]);
    bytes
}

#[test]
fn private_v1_is_detected_by_magic_and_version() {
    let mut bytes = b"HYABNDL\0".to_vec();
    bytes.extend_from_slice(&1_u16.to_le_bytes());

    assert_eq!(detect_package_format(&bytes), Ok(PackageFormat::PrivateV1));
}

#[test]
fn private_v1_prefix_only_is_rejected_as_truncated_envelope() {
    let mut bytes = b"HYABNDL\0".to_vec();
    bytes.extend_from_slice(&1_u16.to_le_bytes());

    assert!(matches!(
        inspect_private_package(&bytes),
        Err(BundleError::InvalidPackageFormat)
    ));
}

#[test]
fn public_v1_is_detected_by_standard_7z_magic() {
    let bytes = [0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c];

    assert_eq!(detect_package_format(&bytes), Ok(PackageFormat::PublicV1));
}

#[test]
fn public_archive_raw_size_limit_is_typed_before_decode() {
    let mut bytes = vec![0_u8; 128 * 1024 * 1024 + 1];
    bytes[..6].copy_from_slice(&[0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c]);

    assert!(matches!(
        inspect_public_package(&bytes),
        Err(BundleError::PackageLimitExceeded { limit, .. }) if limit == "archive bytes"
    ));
}

#[test]
fn public_next_header_limit_is_typed_before_allocation() {
    let next_header_size = 8 * 1024 * 1024 + 1;
    let mut bytes = vec![0_u8; 32 + next_header_size];
    bytes[..6].copy_from_slice(&[0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c]);
    bytes[6..8].copy_from_slice(&[0, 4]);
    bytes[8..12].copy_from_slice(&0x7275_dee3_u32.to_le_bytes());
    bytes[12..20].copy_from_slice(&0_u64.to_le_bytes());
    bytes[20..28].copy_from_slice(&(next_header_size as u64).to_le_bytes());
    bytes[28..32].copy_from_slice(&0_u32.to_le_bytes());

    assert_eq!(bytes.len(), 32 + next_header_size);
    assert!(matches!(
        inspect_public_package(&bytes),
        Err(BundleError::PackageLimitExceeded { limit }) if limit == "next header bytes"
    ));
}

#[test]
fn public_archive_without_root_bundle_manifest_is_rejected_before_prepare() {
    // Apache-2.0 fixture copied byte-for-byte from audited sevenz-rust2 0.20.2
    // commit 424ebdb8fa98b78b8e1c18f73c9add6972fe5496 tests/resources/copy.7z.
    let bytes = include_bytes!("fixtures/packages/non_bundle_root_copy.7z");

    assert_eq!(detect_package_format(bytes), Ok(PackageFormat::PublicV1));
    assert!(matches!(
        inspect_public_package(bytes),
        Err(BundleError::UnsafePackage)
    ));
}

#[test]
fn public_archive_with_extra_entry_is_typed_unsafe() {
    let bytes = include_bytes!("fixtures/packages/upstream_two_empty_file.7z");
    let strict_reader = ArchiveReader::new_with_options(
        Cursor::new(bytes),
        Password::empty(),
        ArchiveReaderOptions::strict(),
    );
    let Ok(reader) = strict_reader.as_ref() else {
        let Err(error) = strict_reader.as_ref() else {
            panic!("strict reader construction yielded neither Ok nor Err");
        };
        panic!("unmodified extra-entry fixture must parse in strict mode: {error}");
    };

    assert_eq!(reader.archive().files.len(), 2);
    assert!(matches!(
        inspect_public_package(bytes),
        Err(BundleError::UnsafePackage)
    ));
}

#[test]
fn public_root_manifest_is_fully_decoded_and_prepared() {
    let bytes = include_bytes!("fixtures/packages/valid_public_bundle_copy.7z");

    assert!(inspect_public_package(bytes).is_ok());
}

#[test]
fn public_archive_trailing_bytes_are_typed_corrupt() {
    let mut bytes = include_bytes!("fixtures/packages/valid_public_bundle_copy.7z").to_vec();
    bytes.push(0);

    assert!(matches!(
        inspect_public_package(&bytes),
        Err(BundleError::CorruptPackage)
    ));
}

#[test]
fn strict_public_reader_rejects_crc_covered_bytes_after_header_end() {
    let mut bytes = include_bytes!("fixtures/packages/valid_public_bundle_copy.7z").to_vec();
    bytes.push(0);
    bytes[20..28].copy_from_slice(&71_u64.to_le_bytes());
    bytes[28..32].copy_from_slice(&0x7bc7_9e5f_u32.to_le_bytes());
    bytes[8..12].copy_from_slice(&0x259c_eb47_u32.to_le_bytes());

    assert_eq!(bytes.len(), 397);
    assert!(matches!(
        inspect_public_package(&bytes),
        Err(BundleError::CorruptPackage)
    ));
}

#[test]
fn public_archive_stream_crc_failure_is_typed_corrupt() {
    let mut bytes = include_bytes!("fixtures/packages/valid_public_bundle_copy.7z").to_vec();
    let Some(pack_stream_byte) = bytes.get_mut(32) else {
        panic!("fixture must contain the first COPY pack-stream byte");
    };
    *pack_stream_byte ^= 0x01;

    assert_eq!(detect_package_format(&bytes), Ok(PackageFormat::PublicV1));
    assert!(matches!(
        inspect_public_package(&bytes),
        Err(BundleError::CorruptPackage)
    ));
}

#[test]
fn public_copy_stream_with_extra_decoded_byte_is_rejected() {
    let mut bytes = include_bytes!("fixtures/packages/valid_public_bundle_copy.7z").to_vec();
    bytes.insert(326, 0xff);
    bytes[12..20].copy_from_slice(&295_u64.to_le_bytes());
    bytes[334] = 0x27;
    bytes[28..32].copy_from_slice(&0x296c_eb30_u32.to_le_bytes());
    bytes[8..12].copy_from_slice(&0x22b7_847a_u32.to_le_bytes());

    assert_eq!(bytes.len(), 397);
    assert!(matches!(
        inspect_public_package(&bytes),
        Err(BundleError::CorruptPackage)
    ));
}

#[test]
fn public_root_manifest_accepts_audited_lzma2_copy_chain() {
    let bytes = include_bytes!("fixtures/packages/valid_public_bundle_lzma2_copy.7z");

    assert!(inspect_public_package(bytes).is_ok());
}

#[test]
fn public_metadata_preflight_uses_referenced_packinfo_bytes_for_ratio() {
    let bytes = PUBLIC_RATIO_PREFLIGHT_LZMA2.as_slice();
    let strict_reader = ArchiveReader::new_with_options(
        Cursor::new(bytes),
        Password::empty(),
        ArchiveReaderOptions::strict(),
    );
    let Ok(reader) = strict_reader.as_ref() else {
        let Err(error) = strict_reader.as_ref() else {
            panic!("strict reader construction yielded neither Ok nor Err");
        };
        panic!("ratio fixture must parse in strict mode: {error}");
    };
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
            .map(|coder| coder.encoder_method_id())
            .collect::<Vec<_>>(),
        vec![EncoderMethod::ID_LZMA2],
    );

    let Some(block_index) = archive
        .stream_map
        .file_block_index
        .first()
        .copied()
        .flatten()
    else {
        panic!("ratio fixture file must reference a block");
    };
    let pack_stream_starts = archive.stream_map.block_first_pack_stream_index();
    let Some(first_pack_stream_index) = pack_stream_starts.get(block_index).copied() else {
        panic!("ratio fixture block must reference a pack stream");
    };
    let next_pack_stream_index = match pack_stream_starts.get(block_index + 1) {
        Some(index) => *index,
        None => archive.pack_sizes().len(),
    };
    let Some(referenced_pack_sizes) = archive
        .pack_sizes()
        .get(first_pack_stream_index..next_pack_stream_index)
    else {
        panic!("ratio fixture pack-stream range must be valid");
    };
    let packed_bytes = referenced_pack_sizes.iter().copied().sum::<u64>();

    assert!(u128::from(entry.size) > u128::from(packed_bytes) * 1_000);
    assert!(u128::from(entry.size) <= (bytes.len() as u128) * 1_000);
    assert!(matches!(
        inspect_public_package(bytes),
        Err(BundleError::PackageLimitExceeded { limit }) if limit == "expansion ratio"
    ));
}

#[test]
fn strict_profile_accepts_audited_copy_lzma2_chain() {
    let bytes = include_bytes!("fixtures/packages/non_bundle_root_copy.7z");
    let strict_reader = ArchiveReader::new_with_options(
        Cursor::new(bytes),
        Password::empty(),
        ArchiveReaderOptions::strict(),
    );
    let Ok(reader) = strict_reader.as_ref() else {
        let Err(error) = strict_reader.as_ref() else {
            panic!("strict reader construction yielded neither Ok nor Err");
        };
        panic!("unmodified fixture must parse in strict mode: {error}");
    };
    let archive = reader.archive();

    assert_eq!(archive.blocks.len(), 1);
    assert_eq!(
        archive.blocks[0]
            .coders
            .iter()
            .map(|coder| coder.encoder_method_id())
            .collect::<Vec<_>>(),
        vec![EncoderMethod::ID_LZMA2, EncoderMethod::ID_COPY],
    );
    assert_eq!(archive.files.len(), 1);
    assert_eq!(archive.files[0].name, "copy.txt");
}

#[test]
fn strict_public_reader_rejects_trailing_bytes() {
    let mut bytes = include_bytes!("fixtures/packages/non_bundle_root_copy.7z").to_vec();
    bytes.push(0);

    assert!(
        ArchiveReader::new_with_options(
            Cursor::new(bytes),
            Password::empty(),
            ArchiveReaderOptions::strict(),
        )
        .is_err()
    );
}

#[test]
fn strict_public_reader_rejects_unreferenced_pack_stream() {
    let mut bytes = include_bytes!("fixtures/packages/non_bundle_root_copy.7z").to_vec();
    bytes.insert(0x3f, 0x00);
    bytes[0x3c] = 2;
    bytes[20..28].copy_from_slice(&91_u64.to_le_bytes());
    bytes[28..32].copy_from_slice(&[0x97, 0x54, 0x60, 0x46]);
    bytes[8..12].copy_from_slice(&[0x6b, 0xc8, 0x41, 0x8e]);

    assert_eq!(bytes.len(), 147);
    assert!(
        ArchiveReader::new_with_options(
            Cursor::new(bytes),
            Password::empty(),
            ArchiveReaderOptions::strict(),
        )
        .is_err()
    );
}

#[test]
fn strict_encoded_header_verifies_single_substream_crc() {
    let mut bytes = include_bytes!("fixtures/packages/upstream_two_empty_file.7z").to_vec();
    bytes.splice(
        136..143,
        [0x00, 0x08, 0x0a, 0x01, 0x55, 0x27, 0xa7, 0x25, 0x00],
    );
    bytes[20..28].copy_from_slice(&34_u64.to_le_bytes());
    bytes[28..32].copy_from_slice(&[0x24, 0x63, 0x49, 0x3f]);
    bytes[8..12].copy_from_slice(&[0xb8, 0x41, 0xfd, 0xdc]);

    assert_eq!(bytes.len(), 146);
    assert!(
        ArchiveReader::new_with_options(
            Cursor::new(bytes),
            Password::empty(),
            ArchiveReaderOptions::strict(),
        )
        .is_err()
    );
}

#[test]
fn private_v1_structurally_valid_envelope_is_opaque_and_unverified() {
    let target = "x86_64-unknown-linux-gnu";
    let ciphertext_digest = PRIVATE_V1_ZERO_CIPHERTEXT_DIGEST;
    let bytes = private_v1_envelope(0x00);

    assert_eq!(bytes.len(), 113);
    assert_eq!(
        inspect_private_package(&bytes),
        Ok(PrivatePackageInspection {
            target: target.to_owned(),
            protocol_minimum: 1,
            protocol_maximum: 1,
            authentication: PrivatePackageAuthentication::Unverified,
            payload: PrivatePackagePayload::Opaque,
            ciphertext_length: 1,
            ciphertext_digest,
        })
    );
}

#[test]
fn private_v1_empty_target_is_rejected() {
    let mut bytes = private_v1_envelope(0x00);
    bytes[14..16].copy_from_slice(&0_u16.to_le_bytes());
    drop(bytes.drain(60..84));

    assert_eq!(bytes.len(), 89);
    assert!(matches!(
        inspect_private_package(&bytes),
        Err(BundleError::InvalidPackageFormat)
    ));
}

#[test]
fn private_v1_ciphertext_digest_mismatch_is_rejected() {
    let bytes = private_v1_envelope(0x01);

    assert!(matches!(
        inspect_private_package(&bytes),
        Err(BundleError::PrivateCiphertextDigestMismatch)
    ));
}
