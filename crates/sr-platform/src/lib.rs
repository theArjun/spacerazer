//! OS-specific functionality behind one portable API (NFR-PORT-01):
//! file identity, allocated size, cloud placeholders, reflinks, device type,
//! volume listing, protected paths and file-manager integration.
//!
//! `unsafe` is confined to the FFI calls in `reflink` and the Windows
//! helpers; each call documents its invariants.

use std::fs::Metadata;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

mod bulk;
mod protected;
mod reflink;

pub use bulk::{BulkEntry, BulkKind, read_dir_bulk};
pub use protected::{ProtectedPaths, builtin_protected_paths};
pub use reflink::reflink;

/// Stable identity of a physical file: (device, inode) on Unix,
/// (volume serial, file index) on Windows.
pub type FileId = (u64, u64);

/// Platform metadata extracted once per entry by the scanner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryMeta {
    pub apparent: u64,
    pub allocated: u64,
    pub nlink: u64,
    pub device: Option<u64>,
    pub file_id: Option<FileId>,
    pub cloud_placeholder: bool,
}

/// Collect platform metadata for an entry. `meta` must come from
/// `symlink_metadata` (never follows links).
pub fn entry_meta(path: &Path, meta: &Metadata) -> EntryMeta {
    imp::entry_meta(path, meta)
}

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[cfg(target_os = "macos")]
    const SF_DATALESS: u32 = 0x4000_0000;

    pub fn entry_meta(_path: &Path, meta: &Metadata) -> EntryMeta {
        let apparent = meta.len();
        let allocated = meta.blocks().saturating_mul(512);
        #[cfg(target_os = "macos")]
        let cloud = {
            use std::os::macos::fs::MetadataExt as _;
            meta.st_flags() & SF_DATALESS != 0
        };
        #[cfg(not(target_os = "macos"))]
        let cloud = false;
        EntryMeta {
            apparent,
            allocated,
            nlink: meta.nlink(),
            device: Some(meta.dev()),
            file_id: Some((meta.dev(), meta.ino())),
            cloud_placeholder: cloud,
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        GetCompressedFileSizeW, GetFileInformationByHandle, INVALID_FILE_SIZE, OPEN_EXISTING,
    };

    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    pub fn entry_meta(path: &Path, meta: &Metadata) -> EntryMeta {
        let attrs = meta.file_attributes();
        let cloud = attrs
            & (FILE_ATTRIBUTE_OFFLINE
                | FILE_ATTRIBUTE_RECALL_ON_OPEN
                | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
            != 0;
        let apparent = meta.len();
        let w = wide(path);
        let allocated = if meta.is_file() {
            let mut high: u32 = 0;
            // SAFETY: `w` is a NUL-terminated UTF-16 path that outlives the call;
            // `high` is a valid out-pointer.
            let low = unsafe { GetCompressedFileSizeW(w.as_ptr(), &mut high) };
            if low == INVALID_FILE_SIZE && io::Error::last_os_error().raw_os_error() != Some(0) {
                apparent
            } else {
                ((high as u64) << 32) | low as u64
            }
        } else {
            0
        };
        let (nlink, device, file_id) = if cloud {
            // Opening a placeholder handle may trigger hydration; skip identity.
            (1, None, None)
        } else {
            by_handle(&w).unwrap_or((1, None, None))
        };
        EntryMeta {
            apparent,
            allocated,
            nlink,
            device,
            file_id,
            cloud_placeholder: cloud,
        }
    }

    fn by_handle(w: &[u16]) -> Option<(u64, Option<u64>, Option<FileId>)> {
        // SAFETY: `w` is NUL-terminated; we request no access rights (metadata
        // only) and close the handle before returning.
        unsafe {
            let h = CreateFileW(
                w.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            );
            if h == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            let ok = GetFileInformationByHandle(h, &mut info);
            CloseHandle(h);
            if ok == 0 {
                return None;
            }
            let serial = info.dwVolumeSerialNumber as u64;
            let index = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
            Some((
                info.nNumberOfLinks as u64,
                Some(serial),
                Some((serial, index)),
            ))
        }
    }
}

/// Storage device class, used to tune I/O concurrency (FR-DUP-07).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DeviceKind {
    Ssd,
    Hdd,
    Network,
    #[default]
    Unknown,
}

impl DeviceKind {
    /// Recommended number of concurrent readers for hashing.
    pub fn io_concurrency(self) -> usize {
        let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
        match self {
            DeviceKind::Ssd => cpus.clamp(4, 16),
            DeviceKind::Hdd => 1,
            DeviceKind::Network => 2,
            DeviceKind::Unknown => cpus.clamp(2, 8),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeInfo {
    pub name: String,
    pub mount_point: PathBuf,
    pub file_system: String,
    pub total: u64,
    pub available: u64,
    pub removable: bool,
    pub kind: DeviceKind,
}

impl VolumeInfo {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }
}

const NETWORK_FS: &[&str] = &[
    "nfs",
    "nfs4",
    "smbfs",
    "cifs",
    "smb2",
    "afpfs",
    "webdav",
    "sshfs",
    "fuse.sshfs",
    "9p",
];

/// Mounted volumes with capacity information (FR-SCAN-01).
pub fn volumes() -> Vec<VolumeInfo> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut out: Vec<VolumeInfo> = disks
        .list()
        .iter()
        .filter(|d| d.total_space() > 0)
        .map(|d| {
            let fs = d.file_system().to_string_lossy().into_owned();
            let kind = if NETWORK_FS.iter().any(|n| fs.eq_ignore_ascii_case(n)) {
                DeviceKind::Network
            } else {
                match d.kind() {
                    sysinfo::DiskKind::SSD => DeviceKind::Ssd,
                    sysinfo::DiskKind::HDD => DeviceKind::Hdd,
                    _ => DeviceKind::Unknown,
                }
            };
            VolumeInfo {
                name: d.name().to_string_lossy().into_owned(),
                mount_point: d.mount_point().to_path_buf(),
                file_system: fs,
                total: d.total_space(),
                available: d.available_space(),
                removable: d.is_removable(),
                kind,
            }
        })
        .collect();
    // macOS lists APFS system sub-volumes; hide the read-only internals.
    out.retain(|v| {
        let m = v.mount_point.to_string_lossy();
        !(m.starts_with("/System/Volumes/") && m != "/System/Volumes/Data")
    });
    out.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    out.dedup_by(|a, b| a.mount_point == b.mount_point);
    out
}

/// The volume containing `path` (longest matching mount point).
pub fn volume_for(path: &Path, vols: &[VolumeInfo]) -> Option<VolumeInfo> {
    vols.iter()
        .filter(|v| path.starts_with(&v.mount_point))
        .max_by_key(|v| v.mount_point.as_os_str().len())
        .cloned()
}

/// Device class of the volume holding `path`.
pub fn device_kind(path: &Path) -> DeviceKind {
    volume_for(path, &volumes()).map_or(DeviceKind::Unknown, |v| v.kind)
}

/// Reveal a path in the platform file manager.
pub fn reveal(path: &Path) -> io::Result<()> {
    opener::reveal(path).map_err(io::Error::other)
}

/// Open a path with its default application.
pub fn open(path: &Path) -> io::Result<()> {
    opener::open(path).map_err(io::Error::other)
}

/// Per-user application directories.
#[derive(Debug, Clone)]
pub struct AppDirs {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
}

pub fn app_dirs() -> Option<AppDirs> {
    let d = directories::ProjectDirs::from("app", "SpaceRazer", "SpaceRazer")?;
    Some(AppDirs {
        config: d.config_dir().to_path_buf(),
        data: d.data_dir().to_path_buf(),
        cache: d.cache_dir().to_path_buf(),
    })
}

pub fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
}

/// Default exclusions: virtual filesystems that must never be walked
/// (FR-SCAN-11).
pub fn default_exclusions() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "linux") {
        v.extend(["/proc", "/sys", "/dev", "/run"].map(PathBuf::from));
    }
    if cfg!(target_os = "macos") {
        v.extend(
            [
                "/dev",
                "/System/Volumes/VM",
                "/private/var/vm",
                "/Volumes/.timemachine",
            ]
            .map(PathBuf::from),
        );
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_of_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, vec![0u8; 10_000]).unwrap();
        let m = std::fs::symlink_metadata(&p).unwrap();
        let e = entry_meta(&p, &m);
        assert_eq!(e.apparent, 10_000);
        assert!(e.allocated >= 4096);
        assert_eq!(e.nlink, 1);
        assert!(e.file_id.is_some());
        assert!(!e.cloud_placeholder);
    }

    #[cfg(unix)]
    #[test]
    fn hardlinks_share_id() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"x").unwrap();
        std::fs::hard_link(&a, &b).unwrap();
        let ea = entry_meta(&a, &std::fs::symlink_metadata(&a).unwrap());
        let eb = entry_meta(&b, &std::fs::symlink_metadata(&b).unwrap());
        assert_eq!(ea.file_id, eb.file_id);
        assert_eq!(ea.nlink, 2);
    }

    #[test]
    fn lists_volumes() {
        let v = volumes();
        assert!(!v.is_empty());
    }
}
