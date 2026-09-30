//! Bulk directory listing with metadata.
//!
//! On macOS, `getattrlistbulk(2)` returns names and attributes for many
//! entries per system call, instead of one `lstat` per entry. It never
//! follows symlinks. Other platforms return `ErrorKind::Unsupported` and the
//! scanner falls back to `read_dir` + per-entry metadata.

use std::ffi::OsString;
use std::io;
use std::path::Path;

use crate::EntryMeta;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkKind {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Debug)]
pub struct BulkEntry {
    pub name: OsString,
    pub kind: BulkKind,
    pub meta: EntryMeta,
    pub mtime: i64,
    /// Per-entry error reported by the filesystem.
    pub error: Option<io::Error>,
}

pub fn read_dir_bulk(dir: &Path) -> io::Result<Vec<BulkEntry>> {
    imp::read_dir_bulk(dir)
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;
    pub fn read_dir_bulk(_dir: &Path) -> io::Result<Vec<BulkEntry>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "bulk listing unsupported",
        ))
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStringExt;

    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const SF_DATALESS: u32 = 0x4000_0000;
    // vnode types (sys/vnode.h)
    const VREG: u32 = 1;
    const VDIR: u32 = 2;
    const VLNK: u32 = 5;

    const COMMON: u32 = libc::ATTR_CMN_RETURNED_ATTRS
        | libc::ATTR_CMN_NAME
        | libc::ATTR_CMN_DEVID
        | libc::ATTR_CMN_OBJTYPE
        | libc::ATTR_CMN_MODTIME
        | libc::ATTR_CMN_FLAGS
        | libc::ATTR_CMN_FILEID
        | ATTR_CMN_ERROR;
    const FILE: u32 =
        libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_ALLOCSIZE | libc::ATTR_FILE_DATALENGTH;

    pub fn read_dir_bulk(dir: &Path) -> io::Result<Vec<BulkEntry>> {
        let f = std::fs::File::open(dir)?;
        let mut attrs = libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: COMMON,
            volattr: 0,
            dirattr: 0,
            fileattr: FILE,
            forkattr: 0,
        };
        let mut buf = vec![0u8; 256 * 1024];
        let mut out = Vec::new();
        loop {
            // SAFETY: `f` is an open directory descriptor for the duration of
            // the call; `attrs` is a valid attrlist; `buf` is a writable buffer
            // of exactly `buf.len()` bytes. The kernel writes at most that many
            // bytes and returns the number of entries packed.
            let n = unsafe {
                libc::getattrlistbulk(
                    f.as_raw_fd(),
                    (&mut attrs as *mut libc::attrlist).cast(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    libc::FSOPT_PACK_INVAL_ATTRS as u64,
                )
            };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            if n == 0 {
                break;
            }
            let mut off = 0usize;
            for _ in 0..n {
                let len = read_u32(&buf, off).ok_or_else(truncated)? as usize;
                if len == 0 || off + len > buf.len() {
                    return Err(truncated());
                }
                out.push(parse(&buf[off..off + len])?);
                off += len;
            }
        }
        Ok(out)
    }

    fn truncated() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "malformed getattrlistbulk record",
        )
    }

    fn read_u32(b: &[u8], at: usize) -> Option<u32> {
        b.get(at..at + 4)
            .map(|s| u32::from_ne_bytes(s.try_into().expect("4 bytes")))
    }
    fn read_i32(b: &[u8], at: usize) -> Option<i32> {
        b.get(at..at + 4)
            .map(|s| i32::from_ne_bytes(s.try_into().expect("4 bytes")))
    }
    fn read_u64(b: &[u8], at: usize) -> Option<u64> {
        b.get(at..at + 8)
            .map(|s| u64::from_ne_bytes(s.try_into().expect("8 bytes")))
    }
    fn read_i64(b: &[u8], at: usize) -> Option<i64> {
        b.get(at..at + 8)
            .map(|s| i64::from_ne_bytes(s.try_into().expect("8 bytes")))
    }

    /// Parse one record. After the returned-attribute set comes
    /// `ATTR_CMN_ERROR`, then the remaining common attributes in bitmap
    /// order, then file attributes (present only for non-directories), each
    /// 4-byte aligned; see getattrlistbulk(2).
    fn parse(rec: &[u8]) -> io::Result<BulkEntry> {
        let t = truncated;
        let mut p = 4; // skip record length
        // attribute_set_t: common, vol, dir, file, fork.
        let returned_common = read_u32(rec, p).ok_or_else(t)?;
        let returned_file = read_u32(rec, p + 12).ok_or_else(t)?;
        p += 20;

        let err = if returned_common & ATTR_CMN_ERROR != 0 {
            let e = read_u32(rec, p).ok_or_else(t)?;
            p += 4;
            e
        } else {
            0
        };

        // ATTR_CMN_NAME: attrreference_t { dataoffset: i32, length: u32 },
        // offset relative to the attrreference itself.
        let name_ref = p;
        let name_off = read_i32(rec, p).ok_or_else(t)?;
        let name_len = read_u32(rec, p + 4).ok_or_else(t)? as usize;
        p += 8;
        let start = usize::try_from(name_ref as i64 + name_off as i64).map_err(|_| t())?;
        let raw = rec.get(start..start + name_len).ok_or_else(t)?;
        let raw = raw.split(|&c| c == 0).next().unwrap_or(raw);
        let name = OsString::from_vec(raw.to_vec());

        let dev = read_i32(rec, p).ok_or_else(t)? as u32 as u64;
        p += 4;
        let objtype = read_u32(rec, p).ok_or_else(t)?;
        p += 4;
        let mtime = read_i64(rec, p).ok_or_else(t)?; // timespec.tv_sec
        p += 16;
        let flags = read_u32(rec, p).ok_or_else(t)?;
        p += 4;
        let fileid = read_u64(rec, p).ok_or_else(t)?;
        p += 8;

        let kind = match objtype {
            VREG => BulkKind::File,
            VDIR => BulkKind::Dir,
            VLNK => BulkKind::Symlink,
            _ => BulkKind::Other,
        };
        let (mut nlink, mut alloc, mut len) = (1u64, 0u64, 0u64);
        if returned_file & libc::ATTR_FILE_LINKCOUNT != 0 {
            nlink = read_u32(rec, p).ok_or_else(t)?.max(1) as u64;
            p += 4;
        }
        if returned_file & libc::ATTR_FILE_ALLOCSIZE != 0 {
            alloc = read_i64(rec, p).ok_or_else(t)?.max(0) as u64;
            p += 8;
        }
        if returned_file & libc::ATTR_FILE_DATALENGTH != 0 {
            len = read_i64(rec, p).ok_or_else(t)?.max(0) as u64;
        }
        let has_id = returned_common & libc::ATTR_CMN_FILEID != 0;

        Ok(BulkEntry {
            name,
            kind,
            meta: EntryMeta {
                apparent: len,
                allocated: alloc,
                nlink,
                device: Some(dev),
                file_id: has_id.then_some((dev, fileid)),
                cloud_placeholder: flags & SF_DATALESS != 0,
            },
            mtime,
            error: (err != 0).then(|| io::Error::from_raw_os_error(err as i32)),
        })
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn matches_lstat() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), vec![7u8; 12_345]).unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::os::unix::fs::symlink("a.txt", d.path().join("link")).unwrap();
        std::fs::hard_link(d.path().join("a.txt"), d.path().join("hl")).unwrap();
        let mut got = read_dir_bulk(d.path()).unwrap();
        got.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<_> = got
            .iter()
            .map(|e| e.name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.txt", "hl", "link", "sub"]);
        for e in &got {
            let m = std::fs::symlink_metadata(d.path().join(&e.name)).unwrap();
            assert_eq!(e.mtime, m.mtime(), "{:?}", e.name);
            assert_eq!(
                e.meta.file_id,
                Some((m.dev() as u64, m.ino())),
                "{:?}",
                e.name
            );
            match e.kind {
                BulkKind::File => {
                    assert_eq!(e.meta.apparent, m.len());
                    assert_eq!(e.meta.allocated, m.blocks() * 512);
                    assert_eq!(e.meta.nlink, m.nlink());
                }
                BulkKind::Dir => assert!(m.is_dir()),
                BulkKind::Symlink => assert!(m.file_type().is_symlink()),
                BulkKind::Other => panic!("unexpected kind"),
            }
        }
    }

    #[test]
    fn large_directory() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..3000 {
            std::fs::write(d.path().join(format!("file-with-a-long-name-{i:05}")), b"x").unwrap();
        }
        assert_eq!(read_dir_bulk(d.path()).unwrap().len(), 3000);
    }
}
