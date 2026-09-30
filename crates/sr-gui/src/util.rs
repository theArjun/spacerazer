//! Small GUI helpers: background jobs, date formatting, log.

use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};
use sr_core::CancellationToken;

/// Sender that wakes the UI whenever a message is sent (§6.2).
pub struct Notifier<M> {
    tx: Sender<M>,
    ctx: egui::Context,
}

impl<M> Notifier<M> {
    pub fn send(&self, m: M) {
        let _ = self.tx.send(m);
        self.ctx.request_repaint();
    }
}

impl<M> Clone for Notifier<M> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            ctx: self.ctx.clone(),
        }
    }
}

/// A background job producing messages of type `M`.
pub struct Job<M> {
    pub rx: Receiver<M>,
    pub cancel: CancellationToken,
    pub panic: Arc<Mutex<Option<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl<M: Send + 'static> Job<M> {
    pub fn spawn(
        ctx: &egui::Context,
        name: &str,
        f: impl FnOnce(Notifier<M>, CancellationToken) + Send + 'static,
    ) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let cancel = CancellationToken::new();
        let panic = Arc::new(Mutex::new(None));
        let notifier = Notifier {
            tx,
            ctx: ctx.clone(),
        };
        let c = cancel.clone();
        let p = panic.clone();
        let ctx2 = ctx.clone();
        let handle = std::thread::Builder::new()
            .name(format!("sr-job-{name}"))
            .spawn(move || {
                // A panic in a worker is reported in the UI rather than
                // taking the app down (NFR-REL-03).
                if let Err(e) = std::panic::catch_unwind(AssertUnwindSafe(|| f(notifier, c))) {
                    let msg = e
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| e.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "background job panicked".into());
                    if let Ok(mut g) = p.lock() {
                        *g = Some(msg);
                    }
                }
                ctx2.request_repaint();
            })
            .expect("spawn job thread");
        Self {
            rx,
            cancel,
            panic,
            handle: Some(handle),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished()) && self.rx.is_empty()
    }

    pub fn take_panic(&self) -> Option<String> {
        self.panic.lock().ok().and_then(|mut g| g.take())
    }

    /// Drain up to `max` pending messages (bounded work per frame).
    pub fn drain(&self, max: usize) -> Vec<M> {
        self.rx.try_iter().take(max).collect()
    }
}

impl<M> Drop for Job<M> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// `YYYY-MM-DD` for Unix seconds (UTC).
pub fn format_date(secs: i64) -> String {
    if secs <= 0 {
        return "—".into();
    }
    let days = secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn format_age_days(days: u64) -> String {
    match days {
        0 => "today".into(),
        1 => "1 day".into(),
        d if d < 60 => format!("{d} days"),
        d if d < 730 => format!("{} months", d / 30),
        d => format!("{} years", d / 365),
    }
}

pub fn format_duration(d: std::time::Duration) -> String {
    let s = d.as_secs_f64();
    if s < 1.0 {
        format!("{} ms", d.as_millis())
    } else if s < 60.0 {
        format!("{s:.1} s")
    } else {
        format!("{}m {:02}s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub at: i64,
    pub level: Level,
    pub msg: String,
}

/// In-app log (FR-SET-05). Never records file contents.
#[derive(Default)]
pub struct Log {
    pub lines: Vec<LogLine>,
    /// Transient notifications: (message, level, shown-until).
    pub toasts: Vec<(String, Level, f64)>,
}

impl Log {
    pub fn push(&mut self, level: Level, msg: impl Into<String>, now: f64) {
        let msg = msg.into();
        self.toasts.push((msg.clone(), level, now + 5.0));
        self.lines.push(LogLine {
            at: sr_core::now_secs(),
            level,
            msg,
        });
        if self.lines.len() > 5000 {
            self.lines.drain(..1000);
        }
    }
    pub fn info(&mut self, msg: impl Into<String>, now: f64) {
        self.push(Level::Info, msg, now);
    }
    pub fn warn(&mut self, msg: impl Into<String>, now: f64) {
        self.push(Level::Warn, msg, now);
    }
    pub fn error(&mut self, msg: impl Into<String>, now: f64) {
        self.push(Level::Error, msg, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(format_date(0), "—");
        assert_eq!(format_date(86_400), "1970-01-02");
        assert_eq!(format_date(1_790_640_000), "2026-09-29");
        assert_eq!(format_date(951_782_400), "2000-02-29");
    }
}
