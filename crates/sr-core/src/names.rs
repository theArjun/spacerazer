//! Interned name storage.
//!
//! All node names live in one contiguous byte buffer, avoiding one heap
//! allocation per node (NFR-PERF-05). Names are stored in the platform's
//! `OsStr` encoded-bytes form so non-UTF-8 names round-trip exactly
//! (NFR-REL-01).

use std::ffi::OsStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NameId(u32);

#[derive(Debug, Default, Clone)]
pub struct NameArena {
    bytes: Vec<u8>,
    spans: Vec<(u32, u32)>,
}

impl NameArena {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, name: &OsStr) -> NameId {
        let raw = name.as_encoded_bytes();
        let start = self.bytes.len() as u32;
        self.bytes.extend_from_slice(raw);
        let id = NameId(self.spans.len() as u32);
        self.spans.push((start, raw.len() as u32));
        id
    }

    pub fn get(&self, id: NameId) -> &OsStr {
        let (start, len) = self.spans[id.0 as usize];
        let raw = &self.bytes[start as usize..(start + len) as usize];
        // SAFETY: every span in `bytes` was produced by `OsStr::as_encoded_bytes`
        // in `intern` during this process, and spans never overlap or split a
        // name, so each slice is a complete, valid encoded `OsStr`.
        unsafe { OsStr::from_encoded_bytes_unchecked(raw) }
    }

    pub fn heap_bytes(&self) -> usize {
        self.bytes.capacity() + self.spans.capacity() * std::mem::size_of::<(u32, u32)>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut a = NameArena::new();
        let x = a.intern(OsStr::new("hello"));
        let y = a.intern(OsStr::new(""));
        let z = a.intern(OsStr::new("wörld"));
        assert_eq!(a.get(x), "hello");
        assert_eq!(a.get(y), "");
        assert_eq!(a.get(z), "wörld");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_roundtrip() {
        use std::os::unix::ffi::OsStrExt;
        let mut a = NameArena::new();
        let raw = OsStr::from_bytes(&[0x66, 0xff, 0x6f]);
        let id = a.intern(raw);
        assert_eq!(a.get(id).as_bytes(), &[0x66, 0xff, 0x6f]);
    }
}
