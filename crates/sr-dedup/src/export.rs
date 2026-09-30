//! CSV / JSON export (FR-DUP-21).

use std::fmt::Write as _;

use serde::Serialize;

use crate::{DupGroup, DupResult, GroupKind, Stats};

#[derive(Serialize)]
struct ExportView<'a> {
    total_wasted: u64,
    groups: &'a [DupGroup],
    similar: &'a [DupGroup],
    errors: Vec<ExportError<'a>>,
    stats: &'a Stats,
}

#[derive(Serialize)]
struct ExportError<'a> {
    path: std::borrow::Cow<'a, str>,
    error: &'a str,
}

/// Pretty-printed JSON with identical groups, similar groups, errors and
/// stats.
pub fn export_json(result: &DupResult) -> String {
    let view = ExportView {
        total_wasted: result.total_wasted(),
        groups: &result.groups,
        similar: &result.similar,
        errors: result
            .errors
            .iter()
            .map(|(p, e)| ExportError {
                path: p.to_string_lossy(),
                error: e,
            })
            .collect(),
        stats: &result.stats,
    };
    serde_json::to_string_pretty(&view).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// One row per file:
/// `group,kind,hash,distance,group_wasted,path,size,mtime,width,height`.
/// Identical groups are numbered first, then similar groups.
pub fn export_csv(result: &DupResult) -> String {
    let mut out =
        String::from("group,kind,hash,distance,group_wasted,path,size,mtime,width,height\n");
    for (gi, g) in result.groups.iter().chain(&result.similar).enumerate() {
        let (kind, distance) = match g.kind {
            GroupKind::Identical => ("identical", String::new()),
            GroupKind::Similar { max_distance } => ("similar", max_distance.to_string()),
        };
        for (fi, f) in g.files.iter().enumerate() {
            let (w, h) = g
                .image_dims
                .get(fi)
                .copied()
                .flatten()
                .map_or((String::new(), String::new()), |(w, h)| {
                    (w.to_string(), h.to_string())
                });
            let _ = writeln!(
                out,
                "{},{},{},{},{},{},{},{},{},{}",
                gi + 1,
                kind,
                g.hash.as_deref().unwrap_or(""),
                distance,
                g.wasted(),
                csv_field(&f.path.to_string_lossy()),
                f.size,
                f.mtime,
                w,
                h
            );
        }
    }
    out
}
