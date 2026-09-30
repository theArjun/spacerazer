//! Copy-on-write clones: APFS `clonefile`, Linux `FICLONE`.

use std::io;
use std::path::Path;

/// Create `dst` as a copy-on-write clone of `src`. `dst` must not exist.
/// Returns `ErrorKind::Unsupported` where the platform or filesystem cannot
/// clone.
pub fn reflink(src: &Path, dst: &Path) -> io::Result<()> {
    imp(src, dst)
}

#[cfg(target_os = "macos")]
fn imp(src: &Path, dst: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let s = CString::new(src.as_os_str().as_bytes())?;
    let d = CString::new(dst.as_os_str().as_bytes())?;
    const CLONE_NOFOLLOW: u32 = 0x0001;
    // SAFETY: both arguments are valid NUL-terminated C strings that outlive
    // the call.
    let rc = unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), CLONE_NOFOLLOW) };
    if rc == 0 {
        Ok(())
    } else {
        let e = io::Error::last_os_error();
        if matches!(e.raw_os_error(), Some(libc::ENOTSUP) | Some(libc::EXDEV)) {
            Err(io::Error::new(io::ErrorKind::Unsupported, e))
        } else {
            Err(e)
        }
    }
}

#[cfg(target_os = "linux")]
fn imp(src: &Path, dst: &Path) -> io::Result<()> {
    use std::fs::{File, OpenOptions};
    use std::os::fd::AsRawFd;
    let s = File::open(src)?;
    let d = OpenOptions::new().write(true).create_new(true).open(dst)?;
    // SAFETY: both descriptors are open for the duration of the call; FICLONE
    // takes the source fd as its integer argument.
    let rc = unsafe { libc::ioctl(d.as_raw_fd(), libc::FICLONE as _, s.as_raw_fd()) };
    if rc == 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    drop(d);
    let _ = std::fs::remove_file(dst);
    if matches!(
        e.raw_os_error(),
        Some(libc::EOPNOTSUPP) | Some(libc::EXDEV) | Some(libc::EINVAL) | Some(libc::ENOTTY)
    ) {
        Err(io::Error::new(io::ErrorKind::Unsupported, e))
    } else {
        Err(e)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn imp(_src: &Path, _dst: &Path) -> io::Result<()> {
    // ReFS block cloning (Windows Dev Drive) is not implemented yet.
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "reflink not supported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_or_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"hello").unwrap();
        match reflink(&a, &b) {
            Ok(()) => assert_eq!(std::fs::read(&b).unwrap(), b"hello"),
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::Unsupported),
        }
    }
}
