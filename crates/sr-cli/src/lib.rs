//! Headless command-line front end (FR-SET-04) and shared settings.

pub mod settings;

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use sr_core::{CancellationToken, Module, SizeMode, SizeUnits, format_size, now_secs};

use crate::settings::Settings;

#[derive(Parser, Debug)]
#[command(name = "spacerazer", version, about = "Analyse and reclaim disk space")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// List mounted volumes with capacity.
    Volumes {
        #[arg(long)]
        json: bool,
    },
    /// Scan folders and print a size summary.
    Scan(ScanArgs),
    /// Find developer build artifacts and caches.
    Dev(DevArgs),
    /// Find duplicate files.
    Dup(DupArgs),
    /// Delete paths through the safety pipeline (dry run unless --yes).
    Delete(DeleteArgs),
    /// Show the operation journal.
    Journal {
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}

/// Names that route `spacerazer <name>` to the CLI instead of the GUI.
pub const SUBCOMMANDS: &[&str] = &[
    "volumes",
    "scan",
    "dev",
    "dup",
    "delete",
    "journal",
    "help",
    "--help",
    "-h",
    "--version",
    "-V",
];

#[derive(Args, Debug)]
pub struct ScanArgs {
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
    /// Emit JSON instead of a table.
    #[arg(long)]
    pub json: bool,
    /// Emit CSV instead of a table.
    #[arg(long, conflicts_with = "json")]
    pub csv: bool,
    /// Depth of the summary.
    #[arg(long, default_value_t = 1)]
    pub depth: usize,
    /// Children shown per directory.
    #[arg(long, default_value_t = 20)]
    pub top: usize,
    /// Report apparent instead of allocated sizes.
    #[arg(long)]
    pub apparent: bool,
    #[arg(long)]
    pub follow_symlinks: bool,
    #[arg(long)]
    pub cross_filesystems: bool,
    /// Glob pattern to exclude (repeatable).
    #[arg(long = "exclude", value_name = "GLOB")]
    pub exclude: Vec<String>,
    /// Also list the N largest files.
    #[arg(long, value_name = "N")]
    pub largest: Option<usize>,
    /// Worker threads (default: all cores).
    #[arg(long)]
    pub threads: Option<usize>,
}

#[derive(Args, Debug)]
pub struct DevArgs {
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
    #[arg(long)]
    pub json: bool,
    /// Only projects inactive for at least this many days.
    #[arg(long)]
    pub stale_days: Option<u64>,
    /// Include global toolchain caches.
    #[arg(long)]
    pub caches: bool,
    /// Skip git queries.
    #[arg(long)]
    pub no_git: bool,
}

#[derive(Args, Debug)]
pub struct DupArgs {
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
    #[arg(long)]
    pub json: bool,
    #[arg(long, conflicts_with = "json")]
    pub csv: bool,
    /// Minimum file size in bytes.
    #[arg(long, default_value_t = 1 << 20)]
    pub min_size: u64,
    #[arg(long)]
    pub paranoid: bool,
    /// Also group visually similar images.
    #[arg(long)]
    pub similar: bool,
    #[arg(long = "include", value_name = "GLOB")]
    pub include: Vec<String>,
    #[arg(long = "exclude", value_name = "GLOB")]
    pub exclude: Vec<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum DeleteMethod {
    Trash,
    Permanent,
}

#[derive(Args, Debug)]
pub struct DeleteArgs {
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
    #[arg(long, value_enum, default_value_t = DeleteMethod::Trash)]
    pub method: DeleteMethod,
    /// Actually perform the operation. Without it, a dry run is printed.
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub json: bool,
}

pub fn run_from<I, T>(args: I) -> Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::parse_from(args);
    let (settings, err) = Settings::load_default();
    if let Some(e) = err {
        eprintln!("warning: settings not loaded ({e}); using defaults");
    }
    run(cli, &settings)
}

pub fn run(cli: Cli, settings: &Settings) -> Result<()> {
    let units = settings.size_units;
    let fmt = |b: u64| format_size(b, units);
    let mut out = std::io::stdout().lock();
    match cli.command {
        Command::Volumes { json } => {
            let v = sr_platform::volumes();
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&v)?)?;
            } else {
                for v in v {
                    writeln!(
                        out,
                        "{:<30} {:>10} used {:>10} free {:>10} total  {} {:?}",
                        v.mount_point.display(),
                        fmt(v.used()),
                        fmt(v.available),
                        fmt(v.total),
                        v.file_system,
                        v.kind
                    )?;
                }
            }
        }
        Command::Scan(a) => scan(a, settings, &mut out)?,
        Command::Dev(a) => dev(a, settings, &mut out)?,
        Command::Dup(a) => dup(a, settings, &mut out)?,
        Command::Delete(a) => delete(a, settings, &mut out)?,
        Command::Journal { json, limit } => {
            let j = sr_ops::Journal::default_location()
                .map(sr_ops::Journal::open)
                .context("no data directory")?;
            let recs = j.read_all()?;
            let recs: Vec<_> = recs.into_iter().rev().take(limit).collect();
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&recs)?)?;
            } else {
                for r in recs {
                    writeln!(
                        out,
                        "{} {:<12} {:>10} {} {}",
                        r.timestamp,
                        r.operation,
                        fmt(r.size),
                        r.result,
                        r.path.display()
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn scan(a: ScanArgs, settings: &Settings, out: &mut impl Write) -> Result<()> {
    let mut opts = settings.scan_options(a.paths);
    opts.follow_symlinks |= a.follow_symlinks;
    opts.cross_filesystems |= a.cross_filesystems;
    opts.exclude_globs.extend(a.exclude);
    if a.threads.is_some() {
        opts.threads = a.threads;
    }
    let mode = if a.apparent {
        SizeMode::Apparent
    } else {
        settings.size_mode
    };
    let (tree, progress) = sr_scan::scan_blocking(opts)?;
    if a.json {
        writeln!(
            out,
            "{}",
            sr_scan::export::to_json(&tree, a.depth, a.top, mode)
        )?;
        return Ok(());
    }
    if a.csv {
        write!(out, "{}", sr_scan::export::to_csv(&tree, a.depth, mode))?;
        return Ok(());
    }
    let units = settings.size_units;
    let s = sr_scan::export::summarize(&tree, tree.root(), a.depth, a.top, mode);
    fn print(
        out: &mut impl Write,
        n: &sr_scan::export::NodeSummary,
        indent: usize,
        parent: u64,
        mode: SizeMode,
        units: SizeUnits,
    ) -> std::io::Result<()> {
        let size = match mode {
            SizeMode::Allocated => n.allocated,
            SizeMode::Apparent => n.apparent,
        };
        let pct = if parent > 0 {
            size as f64 * 100.0 / parent as f64
        } else {
            100.0
        };
        writeln!(
            out,
            "{:>10} {:>5.1}%  {}{}",
            format_size(size, units),
            pct,
            "  ".repeat(indent),
            n.path
        )?;
        for c in &n.children {
            print(out, c, indent + 1, size, mode, units)?;
        }
        Ok(())
    }
    print(out, &s, 0, 0, mode, units)?;
    use std::sync::atomic::Ordering::Relaxed;
    writeln!(
        out,
        "\n{} files, {} dirs, {} issues in {:.2}s",
        progress.files.load(Relaxed),
        progress.dirs.load(Relaxed),
        progress.errors.load(Relaxed),
        progress.elapsed().as_secs_f64()
    )?;
    if let Some(n) = a.largest {
        writeln!(out, "\nLargest files:")?;
        for id in tree.largest_files(n, mode) {
            writeln!(
                out,
                "{:>10}  {}",
                format_size(tree.node(id).size(mode), units),
                tree.path(id).display()
            )?;
        }
    }
    Ok(())
}

fn dev(a: DevArgs, settings: &Settings, out: &mut impl Write) -> Result<()> {
    let mut opts = sr_devsweep::DevOptions::new(a.paths);
    opts.rules = sr_devsweep::RuleSet::builtin().with_custom(settings.dev_custom_rules.clone());
    opts.check_git = settings.dev_check_git && !a.no_git;
    opts.exclude = settings.exclude_paths.clone();
    let cancel = CancellationToken::new();
    let report = sr_devsweep::analyze(&opts, &cancel, &|_| {});
    let now = now_secs();
    let projects: Vec<_> = report
        .projects
        .iter()
        .filter(|p| a.stale_days.is_none_or(|d| p.is_stale(d, now)))
        .collect();
    let caches = if a.caches {
        sr_devsweep::scan_global_caches(&cancel)
    } else {
        Vec::new()
    };
    if a.json {
        #[derive(serde::Serialize)]
        struct Doc<'a> {
            projects: Vec<&'a sr_devsweep::Project>,
            caches: &'a [sr_devsweep::GlobalCache],
        }
        writeln!(
            out,
            "{}",
            serde_json::to_string_pretty(&Doc {
                projects,
                caches: &caches
            })?
        )?;
        return Ok(());
    }
    let fmt = |b| format_size(b, settings.size_units);
    let mut total = 0;
    for p in &projects {
        total += p.artifact_size;
        writeln!(
            out,
            "{:>10}  {} [{}] — inactive {} days",
            fmt(p.artifact_size),
            p.path.display(),
            p.types.join(", "),
            p.inactive_days(now)
        )?;
        for art in &p.artifacts {
            writeln!(
                out,
                "            {:>10} {:<8} {}  (↻ {})",
                fmt(art.allocated),
                art.risk.label(),
                art.path.display(),
                art.regenerate
            )?;
        }
    }
    for c in &caches {
        writeln!(
            out,
            "{:>10}  {} {} [{}]",
            fmt(c.allocated),
            c.name,
            c.path.display(),
            c.risk.label()
        )?;
    }
    writeln!(
        out,
        "\n{} projects, {} in artifacts",
        projects.len(),
        fmt(total)
    )?;
    Ok(())
}

fn dup(a: DupArgs, settings: &Settings, out: &mut impl Write) -> Result<()> {
    let opts = sr_dedup::DupOptions {
        roots: a.paths,
        min_size: a.min_size.max(1),
        include: a.include,
        exclude: a.exclude,
        paranoid: a.paranoid || settings.dup_paranoid,
        io_threads: settings.dup_io_threads,
        hash_cache: settings.hash_cache.clone(),
        similar_images: a.similar,
        similarity_threshold: settings.dup_similarity_threshold,
        ..Default::default()
    };
    let cancel = CancellationToken::new();
    let r = sr_dedup::find_duplicates(&opts, &cancel, &|_| {});
    if a.json {
        writeln!(out, "{}", sr_dedup::export_json(&r))?;
        return Ok(());
    }
    if a.csv {
        write!(out, "{}", sr_dedup::export_csv(&r))?;
        return Ok(());
    }
    let fmt = |b| format_size(b, settings.size_units);
    for g in r.groups.iter().chain(&r.similar) {
        let tag = match g.kind {
            sr_dedup::GroupKind::Identical => "identical",
            sr_dedup::GroupKind::Similar { .. } => "similar",
        };
        writeln!(
            out,
            "{} × {} ({tag}, {} wasted)",
            g.files.len(),
            fmt(g.size),
            fmt(g.wasted())
        )?;
        for f in &g.files {
            writeln!(out, "    {}", f.path.display())?;
        }
    }
    writeln!(
        out,
        "\n{} groups, {} reclaimable, {} files scanned, {} hashed",
        r.groups.len(),
        fmt(r.total_wasted()),
        r.stats.files_scanned,
        fmt(r.stats.bytes_hashed)
    )?;
    for (p, e) in &r.errors {
        eprintln!("error: {}: {e}", p.display());
    }
    Ok(())
}

fn delete(a: DeleteArgs, settings: &Settings, out: &mut impl Write) -> Result<()> {
    let protected = settings.protected();
    let mut drawer = sr_ops::Drawer::new();
    for p in &a.paths {
        if protected.is_protected(p) {
            bail!("{} is a protected path and cannot be deleted", p.display());
        }
        let item = sr_ops::snapshot(p, Module::SpaceMap, "CLI")
            .with_context(|| format!("staging {}", p.display()))?;
        drawer.stage(item, &protected)?;
    }
    let method = match (a.yes, a.method) {
        (false, _) => sr_ops::Method::DryRun,
        (true, DeleteMethod::Trash) => sr_ops::Method::Trash,
        (true, DeleteMethod::Permanent) => sr_ops::Method::Permanent,
    };
    if method == sr_ops::Method::Permanent
        && sr_ops::permanent_delete_threshold_exceeded(
            drawer.items(),
            settings.confirm_bytes_threshold,
            settings.confirm_count_threshold,
        )
    {
        eprint!(
            "This permanently deletes {} items ({}). Type DELETE to confirm: ",
            drawer.len(),
            format_size(drawer.reclaimable(), settings.size_units)
        );
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if line.trim() != "DELETE" {
            bail!("not confirmed");
        }
    }
    let journal = sr_ops::Journal::default_location().map(sr_ops::Journal::open);
    let opts = sr_ops::ExecOptions {
        method,
        dry_run_as: match a.method {
            DeleteMethod::Trash => sr_ops::Method::Trash,
            DeleteMethod::Permanent => sr_ops::Method::Permanent,
        },
        ..Default::default()
    };
    let report = sr_ops::execute(
        drawer.items(),
        &opts,
        &protected,
        journal.as_ref(),
        &CancellationToken::new(),
        &mut |_| {},
    );
    if a.json {
        writeln!(out, "{}", serde_json::to_string_pretty(&report)?)?;
        return Ok(());
    }
    for e in &report.entries {
        writeln!(out, "{:?}  {}", e.outcome, e.path.display())?;
    }
    writeln!(
        out,
        "{}: {} ok, {} skipped, {} failed, {} {}",
        if method == sr_ops::Method::DryRun {
            "dry run"
        } else {
            "done"
        },
        report.succeeded,
        report.skipped,
        report.failed,
        format_size(report.bytes_freed, settings.size_units),
        if method == sr_ops::Method::DryRun {
            "would be freed (re-run with --yes)"
        } else {
            "freed"
        }
    )?;
    Ok(())
}
