//! Pass 2/3 hashing and paranoid comparison.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use memmap2::Mmap;
use sr_core::CancellationToken;
use xxhash_rust::xxh3::Xxh3;

use crate::DupError;

const READ_CHUNK: usize = 256 * 1024;
/// Slice fed to `update_rayon` between cancellation checks.
const MMAP_CHUNK: usize = 16 * 1024 * 1024;

fn check_size(file: &File, expected: u64) -> Result<(), DupError> {
    let actual = file.metadata()?.len();
    if actual != expected {
        return Err(DupError::SizeChanged { expected, actual });
    }
    Ok(())
}

fn open_checked(path: &Path, size: u64) -> Result<File, DupError> {
    let f = File::open(path)?;
    check_size(&f, size)?;
    Ok(f)
}

/// Read exactly `len` bytes, feeding them to `sink` in chunks and checking
/// `cancel` between chunks. A short read means the file shrank.
fn read_into(
    f: &mut File,
    file_size: u64,
    mut len: u64,
    cancel: &CancellationToken,
    on_bytes: &dyn Fn(u64),
    mut sink: impl FnMut(&[u8]),
) -> Result<(), DupError> {
    let mut buf = vec![0u8; (len.min(READ_CHUNK as u64) as usize).max(1)];
    while len > 0 {
        if cancel.is_cancelled() {
            return Err(DupError::Cancelled);
        }
        let n = len.min(buf.len() as u64) as usize;
        if let Err(e) = f.read_exact(&mut buf[..n]) {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                let actual = f.metadata()?.len();
                return Err(DupError::SizeChanged {
                    expected: file_size,
                    actual,
                });
            }
            return Err(e.into());
        }
        sink(&buf[..n]);
        on_bytes(n as u64);
        len -= n as u64;
    }
    Ok(())
}

/// xxh3-128 over `size ‖ head ‖ tail`, with `partial_bytes` from each end
/// (the whole file when it is at most `2 × partial_bytes`) (FR-DUP-04).
pub fn partial_hash(path: &Path, size: u64, partial_bytes: u64) -> io::Result<u128> {
    Ok(partial_hash_with(
        path,
        size,
        partial_bytes,
        &CancellationToken::new(),
        &|_| {},
    )?)
}

pub(crate) fn partial_hash_with(
    path: &Path,
    size: u64,
    partial_bytes: u64,
    cancel: &CancellationToken,
    on_bytes: &dyn Fn(u64),
) -> Result<u128, DupError> {
    let mut f = open_checked(path, size)?;
    let mut h = Xxh3::new();
    h.update(&size.to_le_bytes());
    if size <= partial_bytes.saturating_mul(2) {
        read_into(&mut f, size, size, cancel, on_bytes, |b| h.update(b))?;
    } else {
        read_into(&mut f, size, partial_bytes, cancel, on_bytes, |b| {
            h.update(b)
        })?;
        f.seek(SeekFrom::Start(size - partial_bytes))?;
        read_into(&mut f, size, partial_bytes, cancel, on_bytes, |b| {
            h.update(b)
        })?;
    }
    check_size(&f, size)?;
    Ok(h.digest128())
}

/// Full BLAKE3 digest (FR-DUP-05). Files of at least `mmap_threshold` bytes
/// are memory-mapped and hashed with rayon; smaller files use buffered reads.
/// A file whose size differs from `size` before or after hashing yields an
/// error so it can be dropped from its group and reported.
pub fn full_hash(path: &Path, size: u64, mmap_threshold: u64) -> io::Result<[u8; 32]> {
    Ok(full_hash_with(
        path,
        size,
        mmap_threshold,
        &CancellationToken::new(),
        &|_| {},
    )?)
}

pub(crate) fn full_hash_with(
    path: &Path,
    size: u64,
    mmap_threshold: u64,
    cancel: &CancellationToken,
    on_bytes: &dyn Fn(u64),
) -> Result<[u8; 32], DupError> {
    let mut f = open_checked(path, size)?;
    let mut h = blake3::Hasher::new();
    if size > 0 && size >= mmap_threshold {
        let map = map_readonly(&f, size)?;
        for chunk in map.chunks(MMAP_CHUNK) {
            if cancel.is_cancelled() {
                return Err(DupError::Cancelled);
            }
            h.update_rayon(chunk);
            on_bytes(chunk.len() as u64);
        }
    } else {
        read_into(&mut f, size, size, cancel, on_bytes, |b| {
            h.update(b);
        })?;
    }
    check_size(&f, size)?;
    Ok(*h.finalize().as_bytes())
}

/// Map `file` read-only after confirming it still has `expected` bytes.
#[allow(unsafe_code)]
fn map_readonly(file: &File, expected: u64) -> Result<Mmap, DupError> {
    check_size(file, expected)?;
    // SAFETY: `file` was opened read-only (`File::open`) and the mapping is
    // only ever read, never written. The size was checked just above and the
    // mapped length is checked again below; callers re-check the size after
    // hashing and discard the digest if the file changed. The residual risk —
    // another process truncating the file while it is mapped, which raises
    // SIGBUS — is inherent to mmap; callers avoid mapping on network file
    // systems where that is most likely (README §6.5).
    let map = unsafe { Mmap::map(file)? };
    if map.len() as u64 != expected {
        return Err(DupError::SizeChanged {
            expected,
            actual: map.len() as u64,
        });
    }
    Ok(map)
}

/// Byte-by-byte comparison of two files (paranoid mode, FR-DUP-06).
pub fn files_identical(a: &Path, b: &Path) -> io::Result<bool> {
    Ok(files_identical_with(
        a,
        b,
        &CancellationToken::new(),
        &|_| {},
    )?)
}

pub(crate) fn files_identical_with(
    a: &Path,
    b: &Path,
    cancel: &CancellationToken,
    on_bytes: &dyn Fn(u64),
) -> Result<bool, DupError> {
    let mut fa = File::open(a)?;
    let mut fb = File::open(b)?;
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let mut ba = vec![0u8; READ_CHUNK];
    let mut bb = vec![0u8; READ_CHUNK];
    loop {
        if cancel.is_cancelled() {
            return Err(DupError::Cancelled);
        }
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
        on_bytes(na as u64);
    }
}

/// Fill `buf` as far as possible; returns fewer bytes only at EOF.
fn read_full(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}
