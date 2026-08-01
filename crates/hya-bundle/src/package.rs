use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{BundleError, BundleSource, PreparedCatalog, SourceFile, prepare_package};
use sevenz_rust2::{
    ArchiveReader, ArchiveReaderOptions, EncoderMethod, Error as ArchiveError, Password,
};
use sha2::{Digest, Sha256};

const PUBLIC_MAGIC: &[u8; 6] = &[0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c];
const PRIVATE_MAGIC: &[u8; 8] = b"HYABNDL\0";
const PRIVATE_VERSION: u16 = 1;
const PRIVATE_PROTOCOL_MINIMUM_OFFSET: usize = 10;
const PRIVATE_PROTOCOL_MAXIMUM_OFFSET: usize = 12;
const PRIVATE_TARGET_LENGTH_OFFSET: usize = 14;
const PRIVATE_NONCE_LENGTH_OFFSET: usize = 16;
const PRIVATE_TAG_LENGTH_OFFSET: usize = 18;
const PRIVATE_CIPHERTEXT_LENGTH_OFFSET: usize = 20;
const PRIVATE_CIPHERTEXT_DIGEST_OFFSET: usize = 28;
const PRIVATE_FIXED_HEADER_LENGTH: usize = 60;
const PRIVATE_NONCE_LENGTH: u16 = 12;
const PRIVATE_TAG_LENGTH: u16 = 16;
const PUBLIC_PACKAGE_MAX_BYTES: usize = 128 * 1024 * 1024;
const PUBLIC_MANIFEST_MAX_BYTES: u64 = 64 * 1024 * 1024;
const PUBLIC_MANIFEST_MAX_BYTES_USIZE: usize = 64 * 1024 * 1024;
const PUBLIC_EXPANDED_MAX_BYTES: usize = 256 * 1024 * 1024;
const MAX_EXPANSION_RATIO: u128 = 1_000;
const PUBLIC_MANIFEST_MAX_PATH_BYTES: usize = 1024;
const PUBLIC_MANIFEST_MAX_PATH_DEPTH: usize = 32;
const WINDOWS_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const WINDOWS_ATTRIBUTE_DEVICE: u32 = 0x40;
const WINDOWS_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const UNIX_FILE_TYPE_MASK: u32 = 0o170000;
const UNIX_REGULAR_FILE_TYPE: u32 = 0o100000;
const STAGING_DIRECTORY_PREFIX: &str = "hya-bundle-stage-";
const STAGING_BUILDING_PREFIX: &str = "hya-bundle-building-";
const STAGED_PACKAGE_FILE_NAME: &str = "package";

static NEXT_STAGED_DIRECTORY: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageFormat {
    PublicV1,
    PrivateV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivatePackageAuthentication {
    Unverified,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivatePackagePayload {
    Opaque,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrivatePackageInspection {
    pub target: String,
    pub protocol_minimum: u16,
    pub protocol_maximum: u16,
    pub authentication: PrivatePackageAuthentication,
    pub payload: PrivatePackagePayload,
    pub ciphertext_length: u64,
    pub ciphertext_digest: [u8; 32],
}

#[derive(Debug)]
pub struct PublicPackageInspection {
    pub prepared: PreparedCatalog,
    pub source_digest: [u8; 32],
}

#[derive(Debug)]
pub enum PackageInspection {
    Public(PublicPackageInspection),
    Private(PrivatePackageInspection),
}

pub struct StagedPackage {
    directory: PathBuf,
    file_path: PathBuf,
    file: Option<File>,
}

pub fn cleanup_orphaned_staging(staging_root: &Path) -> Result<(), BundleError> {
    let entries = match fs::read_dir(staging_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(staging_root, error)),
    };
    for entry in entries {
        let entry = entry.map_err(|error| io_error(staging_root, error))?;
        let directory = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(STAGING_DIRECTORY_PREFIX) {
            continue;
        }
        let directory_metadata =
            fs::symlink_metadata(&directory).map_err(|error| io_error(&directory, error))?;
        if !directory_metadata.file_type().is_dir() {
            continue;
        }
        let Some(file_path) = lone_staged_package_file(&directory)? else {
            continue;
        };

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&file_path)
            .map_err(|error| io_error(&file_path, error))?;
        match file.try_lock() {
            Ok(()) => {
                drop(file);
                fs::remove_file(&file_path).map_err(|error| io_error(&file_path, error))?;
                fs::remove_dir(&directory).map_err(|error| io_error(&directory, error))?;
            }
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(error)) => return Err(io_error(&file_path, error)),
        }
    }
    Ok(())
}

pub fn stage_package(
    source_path: &Path,
    staging_root: &Path,
) -> Result<StagedPackage, BundleError> {
    fs::create_dir_all(staging_root).map_err(|error| io_error(staging_root, error))?;
    let (building_directory, directory) = create_staging_directories(staging_root)?;
    let file_path = building_directory.join(STAGED_PACKAGE_FILE_NAME);
    let file = match OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&file_path)
    {
        Ok(file) => file,
        Err(error) => {
            let _ = fs::remove_dir(&building_directory);
            return Err(io_error(&file_path, error));
        }
    };
    let mut staged = StagedPackage {
        directory: building_directory,
        file_path,
        file: Some(file),
    };
    let staged_path = staged.file_path.clone();
    staged
        .file_mut()?
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| io_error(&staged_path, error))?;

    staged.lock()?;
    staged.publish(directory)?;

    let mut source = File::open(source_path).map_err(|error| io_error(source_path, error))?;
    let staged_path = staged.file_path.clone();
    copy_source_to_staging(&mut source, staged.file_mut()?, source_path, &staged_path)?;
    Ok(staged)
}

impl StagedPackage {
    pub fn inspect(mut self) -> Result<PackageInspection, BundleError> {
        let (bytes, source_digest) = self.read_staged_bytes()?;
        match detect_package_format(&bytes)? {
            PackageFormat::PublicV1 => Ok(PackageInspection::Public(PublicPackageInspection {
                prepared: inspect_public_package(&bytes)?,
                source_digest,
            })),
            PackageFormat::PrivateV1 => {
                Ok(PackageInspection::Private(inspect_private_package(&bytes)?))
            }
        }
    }

    fn file_mut(&mut self) -> Result<&mut File, BundleError> {
        let file_path = self.file_path.clone();
        self.file.as_mut().ok_or_else(|| {
            io_error(
                &file_path,
                std::io::Error::other("staged file handle is closed"),
            )
        })
    }

    fn lock(&mut self) -> Result<(), BundleError> {
        let file_path = self.file_path.clone();
        match self.file_mut()?.try_lock() {
            Ok(()) => Ok(()),
            Err(TryLockError::WouldBlock) => Err(io_error(
                &file_path,
                std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "new staged package file is locked",
                ),
            )),
            Err(TryLockError::Error(error)) => Err(io_error(&file_path, error)),
        }
    }

    fn publish(&mut self, directory: PathBuf) -> Result<(), BundleError> {
        match fs::symlink_metadata(&directory) {
            Ok(_) => {
                return Err(io_error(
                    &directory,
                    std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "staged package directory already exists",
                    ),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(&directory, error)),
        }

        let building_directory = self.directory.clone();
        fs::rename(&building_directory, &directory).map_err(|error| io_error(&directory, error))?;
        self.directory = directory;
        self.file_path = self.directory.join(STAGED_PACKAGE_FILE_NAME);
        Ok(())
    }

    fn read_staged_bytes(&mut self) -> Result<(Vec<u8>, [u8; 32]), BundleError> {
        let file_path = self.file_path.clone();
        let file = self.file_mut()?;
        file.seek(SeekFrom::Start(0))
            .map_err(|error| io_error(&file_path, error))?;

        let mut bytes = Vec::new();
        let mut digest = Sha256::new();
        let mut total = 0_usize;
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|error| io_error(&file_path, error))?;
            if read == 0 {
                break;
            }
            let next_total = total
                .checked_add(read)
                .ok_or(BundleError::PackageLimitExceeded {
                    limit: "archive bytes",
                })?;
            if next_total > PUBLIC_PACKAGE_MAX_BYTES {
                return Err(BundleError::PackageLimitExceeded {
                    limit: "archive bytes",
                });
            }
            let chunk = buffer.get(..read).ok_or_else(|| {
                io_error(
                    &file_path,
                    std::io::Error::other("staged file read exceeded buffer"),
                )
            })?;
            digest.update(chunk);
            bytes.extend_from_slice(chunk);
            total = next_total;
        }

        Ok((bytes, digest.finalize().into()))
    }
}

impl Drop for StagedPackage {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.file_path);
        let _ = fs::remove_dir(&self.directory);
    }
}

pub fn detect_package_format(bytes: &[u8]) -> Result<PackageFormat, BundleError> {
    if bytes.get(..PUBLIC_MAGIC.len()) == Some(PUBLIC_MAGIC.as_slice()) {
        return Ok(PackageFormat::PublicV1);
    }
    if bytes.get(..PRIVATE_MAGIC.len()) != Some(PRIVATE_MAGIC.as_slice()) {
        return Err(BundleError::InvalidPackageFormat);
    }

    let Some(version_bytes) = bytes.get(PRIVATE_MAGIC.len()..PRIVATE_MAGIC.len() + 2) else {
        return Err(BundleError::InvalidPackageFormat);
    };
    let version = u16::from_le_bytes(
        version_bytes
            .try_into()
            .map_err(|_| BundleError::InvalidPackageFormat)?,
    );
    if version != PRIVATE_VERSION {
        return Err(BundleError::UnsupportedPackageVersion { found: version });
    }

    Ok(PackageFormat::PrivateV1)
}

pub fn inspect_private_package(bytes: &[u8]) -> Result<PrivatePackageInspection, BundleError> {
    if !matches!(detect_package_format(bytes)?, PackageFormat::PrivateV1) {
        return Err(BundleError::InvalidPackageFormat);
    }

    let protocol_minimum =
        u16::from_le_bytes(private_bytes(bytes, PRIVATE_PROTOCOL_MINIMUM_OFFSET)?);
    let protocol_maximum =
        u16::from_le_bytes(private_bytes(bytes, PRIVATE_PROTOCOL_MAXIMUM_OFFSET)?);
    let target_length = usize::from(u16::from_le_bytes(private_bytes(
        bytes,
        PRIVATE_TARGET_LENGTH_OFFSET,
    )?));
    let nonce_length = u16::from_le_bytes(private_bytes(bytes, PRIVATE_NONCE_LENGTH_OFFSET)?);
    let tag_length = u16::from_le_bytes(private_bytes(bytes, PRIVATE_TAG_LENGTH_OFFSET)?);
    let ciphertext_length =
        u64::from_le_bytes(private_bytes(bytes, PRIVATE_CIPHERTEXT_LENGTH_OFFSET)?);
    let ciphertext_length_usize =
        usize::try_from(ciphertext_length).map_err(|_| BundleError::InvalidPackageFormat)?;
    let ciphertext_digest = private_bytes(bytes, PRIVATE_CIPHERTEXT_DIGEST_OFFSET)?;

    if target_length == 0
        || nonce_length != PRIVATE_NONCE_LENGTH
        || tag_length != PRIVATE_TAG_LENGTH
        || protocol_minimum > protocol_maximum
    {
        return Err(BundleError::InvalidPackageFormat);
    }

    let target_end = private_end(PRIVATE_FIXED_HEADER_LENGTH, target_length)?;
    let nonce_end = private_end(target_end, usize::from(nonce_length))?;
    let ciphertext_end = private_end(nonce_end, ciphertext_length_usize)?;
    let envelope_end = private_end(ciphertext_end, usize::from(tag_length))?;
    if envelope_end != bytes.len() {
        return Err(BundleError::InvalidPackageFormat);
    }
    let ciphertext = bytes
        .get(nonce_end..ciphertext_end)
        .ok_or(BundleError::InvalidPackageFormat)?;
    let actual_ciphertext_digest: [u8; 32] = Sha256::digest(ciphertext).into();
    if actual_ciphertext_digest != ciphertext_digest {
        return Err(BundleError::PrivateCiphertextDigestMismatch);
    }

    let target = std::str::from_utf8(
        bytes
            .get(PRIVATE_FIXED_HEADER_LENGTH..target_end)
            .ok_or(BundleError::InvalidPackageFormat)?,
    )
    .map_err(|_| BundleError::InvalidPackageFormat)?
    .to_owned();

    Ok(PrivatePackageInspection {
        target,
        protocol_minimum,
        protocol_maximum,
        authentication: PrivatePackageAuthentication::Unverified,
        payload: PrivatePackagePayload::Opaque,
        ciphertext_length,
        ciphertext_digest,
    })
}

/// Strictly validates, decodes, and prepares an untrusted public package.
pub fn inspect_public_package(bytes: &[u8]) -> Result<PreparedCatalog, BundleError> {
    if bytes.len() > PUBLIC_PACKAGE_MAX_BYTES {
        return Err(BundleError::PackageLimitExceeded {
            limit: "archive bytes",
        });
    }
    if !matches!(detect_package_format(bytes), Ok(PackageFormat::PublicV1)) {
        return Err(BundleError::InvalidPackageFormat);
    }

    let mut reader = ArchiveReader::new_with_options(
        Cursor::new(bytes),
        Password::empty(),
        ArchiveReaderOptions::strict(),
    )
    .map_err(map_archive_error)?;
    let (
        archive_file_count,
        expected_entry_name,
        expected_entry_size,
        expected_entry_crc,
        packed_bytes,
    ) = {
        let archive = reader.archive();
        if archive.files.len() != 1 {
            return Err(BundleError::UnsafePackage);
        }
        if archive.is_solid || archive.blocks.len() != 1 {
            return Err(BundleError::InvalidPackageFormat);
        }

        let entry = archive
            .files
            .first()
            .ok_or(BundleError::InvalidPackageFormat)?;
        if entry.name != "bundle.hya.md" {
            return Err(BundleError::UnsafePackage);
        }
        if !entry.has_stream
            || entry.is_directory
            || entry.is_anti_item
            || !entry.has_crc
            || entry.name.len() > PUBLIC_MANIFEST_MAX_PATH_BYTES
            || entry.name.split('/').count() > PUBLIC_MANIFEST_MAX_PATH_DEPTH
            || entry.size > PUBLIC_MANIFEST_MAX_BYTES
        {
            return Err(BundleError::InvalidPackageFormat);
        }

        if entry.has_windows_attributes {
            let attributes = entry.windows_attributes;
            if attributes
                & (WINDOWS_ATTRIBUTE_DIRECTORY
                    | WINDOWS_ATTRIBUTE_DEVICE
                    | WINDOWS_ATTRIBUTE_REPARSE_POINT)
                != 0
            {
                return Err(BundleError::InvalidPackageFormat);
            }

            let unix_file_type = (attributes >> 16) & UNIX_FILE_TYPE_MASK;
            if unix_file_type != 0 && unix_file_type != UNIX_REGULAR_FILE_TYPE {
                return Err(BundleError::InvalidPackageFormat);
            }
        }

        let block = archive
            .blocks
            .first()
            .ok_or(BundleError::InvalidPackageFormat)?;
        if block.coders.is_empty() || block.coders.len() > 2 {
            return Err(BundleError::InvalidPackageFormat);
        }
        for coder in &block.coders {
            let coder_id = coder.encoder_method_id();
            if coder_id != EncoderMethod::ID_COPY
                && coder_id != EncoderMethod::ID_LZMA
                && coder_id != EncoderMethod::ID_LZMA2
            {
                return Err(BundleError::InvalidPackageFormat);
            }
        }

        let block_index = archive
            .stream_map
            .file_block_index
            .first()
            .copied()
            .flatten()
            .ok_or(BundleError::InvalidPackageFormat)?;
        if block_index != 0 {
            return Err(BundleError::InvalidPackageFormat);
        }
        let pack_stream_starts = archive.stream_map.block_first_pack_stream_index();
        let first_pack_stream_index = pack_stream_starts
            .get(block_index)
            .copied()
            .ok_or(BundleError::InvalidPackageFormat)?;
        let next_block_index = block_index
            .checked_add(1)
            .ok_or(BundleError::InvalidPackageFormat)?;
        let next_pack_stream_index = pack_stream_starts
            .get(next_block_index)
            .copied()
            .unwrap_or_else(|| archive.pack_sizes().len());
        let referenced_pack_sizes = archive
            .pack_sizes()
            .get(first_pack_stream_index..next_pack_stream_index)
            .ok_or(BundleError::InvalidPackageFormat)?;
        if referenced_pack_sizes.is_empty() {
            return Err(BundleError::InvalidPackageFormat);
        }
        let packed_bytes = referenced_pack_sizes
            .iter()
            .try_fold(0_u64, |total, packed_size| {
                total
                    .checked_add(*packed_size)
                    .ok_or(BundleError::InvalidPackageFormat)
            })?;
        if !within_expansion_ratio(entry.size, packed_bytes) {
            return Err(BundleError::PackageLimitExceeded {
                limit: "expansion ratio",
            });
        }

        (
            archive.files.len(),
            entry.name.clone(),
            entry.size,
            entry.crc,
            packed_bytes,
        )
    };

    let mut callback_count = 0_usize;
    let mut expanded_bytes = 0_usize;
    let mut invalid_layout = false;
    let mut callback_error = None;
    let mut manifest_bytes = Vec::new();
    reader
        .for_each_entries(|entry, stream| {
            callback_count = callback_count
                .checked_add(1)
                .ok_or_else(|| std::io::Error::other("public package callback count overflow"))?;
            if callback_count != 1
                || entry.name.as_str() != expected_entry_name.as_str()
                || !entry.has_stream
                || entry.is_directory
                || entry.is_anti_item
                || !entry.has_crc
                || entry.size != expected_entry_size
                || entry.crc != expected_entry_crc
            {
                invalid_layout = true;
                return Ok(false);
            }

            let declared_size = usize::try_from(entry.size).map_err(|_| {
                std::io::Error::other("public package manifest size does not fit usize")
            })?;
            let mut buffer = [0_u8; 8 * 1024];
            loop {
                let read = stream.read(&mut buffer)?;
                if read == 0 {
                    break;
                }

                let chunk = buffer.get(..read).ok_or_else(|| {
                    std::io::Error::other("public package stream read exceeded buffer")
                })?;
                if let Err(error) = retain_public_chunk(
                    &mut manifest_bytes,
                    &mut expanded_bytes,
                    chunk,
                    packed_bytes,
                ) {
                    callback_error = Some(error);
                    return Ok(false);
                }
            }

            if manifest_bytes.len() != declared_size {
                invalid_layout = true;
                return Ok(false);
            }

            Ok(true)
        })
        .map_err(map_archive_error)?;

    if let Some(error) = callback_error {
        return Err(error);
    }
    if invalid_layout || callback_count != 1 || callback_count != archive_file_count {
        return Err(BundleError::InvalidPackageFormat);
    }

    prepare_package(BundleSource::new(
        "public-package",
        vec![SourceFile::new("bundle.hya.md", manifest_bytes)],
    ))
}

fn private_bytes<const LENGTH: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; LENGTH], BundleError> {
    let end = private_end(offset, LENGTH)?;
    bytes
        .get(offset..end)
        .ok_or(BundleError::InvalidPackageFormat)?
        .try_into()
        .map_err(|_| BundleError::InvalidPackageFormat)
}

fn private_end(offset: usize, length: usize) -> Result<usize, BundleError> {
    offset
        .checked_add(length)
        .ok_or(BundleError::InvalidPackageFormat)
}

fn create_staging_directories(staging_root: &Path) -> Result<(PathBuf, PathBuf), BundleError> {
    loop {
        let sequence = NEXT_STAGED_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let directory = staging_root.join(format!(
            "{STAGING_DIRECTORY_PREFIX}{}-{sequence}",
            std::process::id(),
        ));
        match fs::symlink_metadata(&directory) {
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(&directory, error)),
        }
        let building_directory = staging_root.join(format!(
            "{STAGING_BUILDING_PREFIX}{}-{sequence}",
            std::process::id(),
        ));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&building_directory) {
            Ok(()) => {
                if let Err(error) =
                    fs::set_permissions(&building_directory, fs::Permissions::from_mode(0o700))
                {
                    let _ = fs::remove_dir(&building_directory);
                    return Err(io_error(&building_directory, error));
                }
                return Ok((building_directory, directory));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error(&building_directory, error)),
        }
    }
}

fn lone_staged_package_file(directory: &Path) -> Result<Option<PathBuf>, BundleError> {
    let mut entries = fs::read_dir(directory).map_err(|error| io_error(directory, error))?;
    let Some(entry) = entries.next() else {
        return Ok(None);
    };
    let entry = entry.map_err(|error| io_error(directory, error))?;
    match entries.next() {
        None => {}
        Some(Ok(_)) => return Ok(None),
        Some(Err(error)) => return Err(io_error(directory, error)),
    }

    let name = entry.file_name();
    if name.to_str() != Some(STAGED_PACKAGE_FILE_NAME) {
        return Ok(None);
    }
    let file_path = entry.path();
    let metadata = fs::symlink_metadata(&file_path).map_err(|error| io_error(&file_path, error))?;
    if !metadata.file_type().is_file() {
        return Ok(None);
    }
    Ok(Some(file_path))
}

fn copy_source_to_staging(
    source: &mut File,
    staged: &mut File,
    source_path: &Path,
    staged_path: &Path,
) -> Result<(), BundleError> {
    let mut total = 0_usize;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = source
            .read(&mut buffer)
            .map_err(|error| io_error(source_path, error))?;
        if read == 0 {
            return Ok(());
        }
        let next_total = total
            .checked_add(read)
            .ok_or(BundleError::PackageLimitExceeded {
                limit: "archive bytes",
            })?;
        if next_total > PUBLIC_PACKAGE_MAX_BYTES {
            return Err(BundleError::PackageLimitExceeded {
                limit: "archive bytes",
            });
        }
        let chunk = buffer.get(..read).ok_or_else(|| {
            io_error(
                source_path,
                std::io::Error::other("source file read exceeded buffer"),
            )
        })?;
        staged
            .write_all(chunk)
            .map_err(|error| io_error(staged_path, error))?;
        total = next_total;
    }
}

fn io_error(path: &Path, error: std::io::Error) -> BundleError {
    BundleError::Io {
        path: path.display().to_string(),
        detail: error.to_string(),
    }
}

fn map_archive_error(error: ArchiveError) -> BundleError {
    match error {
        ArchiveError::StructuralLimitExceeded { field, .. } => {
            BundleError::PackageLimitExceeded { limit: field }
        }
        ArchiveError::MaxMemLimited { .. } => BundleError::PackageLimitExceeded {
            limit: "decoder memory bytes",
        },
        _ => BundleError::CorruptPackage,
    }
}

fn retain_public_chunk(
    retained: &mut Vec<u8>,
    actual: &mut usize,
    chunk: &[u8],
    packed_bytes: u64,
) -> Result<(), BundleError> {
    let next_manifest_length = retained
        .len()
        .checked_add(chunk.len())
        .ok_or(BundleError::InvalidPackageFormat)?;
    let next_actual = (*actual)
        .checked_add(chunk.len())
        .ok_or(BundleError::InvalidPackageFormat)?;
    if next_manifest_length > PUBLIC_MANIFEST_MAX_BYTES_USIZE
        || next_actual > PUBLIC_EXPANDED_MAX_BYTES
    {
        return Err(BundleError::InvalidPackageFormat);
    }

    let next_actual_u64 =
        u64::try_from(next_actual).map_err(|_| BundleError::InvalidPackageFormat)?;
    if !within_expansion_ratio(next_actual_u64, packed_bytes) {
        return Err(BundleError::PackageLimitExceeded {
            limit: "expansion ratio",
        });
    }

    retained.extend_from_slice(chunk);
    *actual = next_actual;
    Ok(())
}

fn within_expansion_ratio(expanded_bytes: u64, packed_bytes: u64) -> bool {
    if packed_bytes == 0 {
        return expanded_bytes == 0;
    }

    u128::from(expanded_bytes) <= u128::from(packed_bytes) * MAX_EXPANSION_RATIO
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expansion_ratio_boundary_and_zero_semantics() {
        assert!(within_expansion_ratio(1_024_000, 1_024));
        assert!(!within_expansion_ratio(1_024_001, 1_024));
        assert!(within_expansion_ratio(0, 0));
        assert!(!within_expansion_ratio(1, 0));
        assert!(within_expansion_ratio(0, 1_024));
    }

    #[test]
    fn streaming_ratio_rejects_before_retaining_crossing_chunk() {
        let mut retained = vec![0x61; 999];
        let unchanged = retained.clone();
        let mut actual = 999_usize;
        let chunk = [0x62; 2];

        let result = retain_public_chunk(&mut retained, &mut actual, &chunk, 1_u64);

        assert!(matches!(
            result,
            Err(BundleError::PackageLimitExceeded { limit }) if limit == "expansion ratio"
        ));
        assert_eq!(retained, unchanged);
        assert_eq!(actual, 999);
    }
}
