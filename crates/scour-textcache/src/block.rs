//! Blocks on disk: each one zstd frame of texts laid end to end, written
//! once, readable only by its owner.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

/// zstd's level 19: 6.60x on documents' text in 4 MiB blocks, and read back
/// at 2 GB/s; compressed once, by a thread nobody waits on.
const LEVEL: i32 = 19;

pub(crate) fn path_of(dir: &Path, block: u32) -> PathBuf {
    dir.join(format!("{block:08}.zst"))
}

pub(crate) fn write(dir: &Path, block: u32, text: &[u8]) -> io::Result<u64> {
    let level = structured_zstd::encoding::CompressionLevel::from_level(LEVEL);
    let packed = structured_zstd::encoding::compress_to_vec(text, level);
    write_private(&path_of(dir, block), &packed)?;
    Ok(packed.len() as u64)
}

/// The block's texts, uncompressed.
pub(crate) fn read(dir: &Path, block: u32) -> io::Result<Vec<u8>> {
    let packed = std::fs::read(path_of(dir, block))?;
    let mut decoder = structured_zstd::decoding::StreamingDecoder::new(packed.as_slice());
    let mut text = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut text)?;
    Ok(text)
}

/// Written beside, then renamed over: never half a file under its name.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut file = private_file(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

#[cfg(unix)]
fn private_file(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::File::options()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

/// Under the owner's profile, which only the owner can read by inheritance.
#[cfg(not(unix))]
fn private_file(path: &Path) -> io::Result<std::fs::File> {
    std::fs::File::create(path)
}

/// The cache's directory, made readable only by its owner.
pub(crate) fn private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
