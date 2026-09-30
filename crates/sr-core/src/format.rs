use serde::{Deserialize, Serialize};

/// Size display convention (UI-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SizeUnits {
    /// KiB, MiB, GiB (powers of 1024).
    Binary,
    /// kB, MB, GB (powers of 1000).
    Decimal,
}

impl Default for SizeUnits {
    /// Match the host OS convention: macOS reports decimal units, Windows and
    /// most Linux tools report binary.
    fn default() -> Self {
        if cfg!(target_os = "macos") {
            SizeUnits::Decimal
        } else {
            SizeUnits::Binary
        }
    }
}

pub fn format_size(bytes: u64, units: SizeUnits) -> String {
    let (base, suffixes): (f64, [&str; 7]) = match units {
        SizeUnits::Binary => (1024.0, ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"]),
        SizeUnits::Decimal => (1000.0, ["B", "kB", "MB", "GB", "TB", "PB", "EB"]),
    };
    if bytes < base as u64 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= base && i < suffixes.len() - 1 {
        v /= base;
        i += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", suffixes[i])
    } else if v >= 10.0 {
        format!("{v:.1} {}", suffixes[i])
    } else {
        format!("{v:.2} {}", suffixes[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(format_size(0, SizeUnits::Binary), "0 B");
        assert_eq!(format_size(1023, SizeUnits::Binary), "1023 B");
        assert_eq!(format_size(1024, SizeUnits::Binary), "1.00 KiB");
        assert_eq!(format_size(1_500_000, SizeUnits::Decimal), "1.50 MB");
        assert_eq!(format_size(27_300_000_000, SizeUnits::Decimal), "27.3 GB");
        assert_eq!(format_size(u64::MAX, SizeUnits::Binary), "16.0 EiB");
    }
}
