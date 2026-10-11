//! Progress display: bars on a terminal, plain percentage lines otherwise.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bak_core::{ProgressKind, ProgressReporter, ProgressTask};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

#[derive(Debug)]
pub struct TermProgress {
    tty: bool,
}

impl TermProgress {
    pub fn new() -> Self {
        Self {
            tty: std::io::stderr().is_terminal(),
        }
    }
}

impl Default for TermProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressReporter for TermProgress {
    fn start(&self, kind: ProgressKind, title: &str) -> Arc<dyn ProgressTask> {
        if self.tty {
            Arc::new(BarTask::new(kind, title))
        } else {
            Arc::new(PlainTask {
                kind,
                title: Mutex::new(title.to_string()),
                len: AtomicU64::new(0),
                pos: AtomicU64::new(0),
                printed_step: AtomicU64::new(0),
            })
        }
    }
}

#[derive(Debug)]
struct BarTask(ProgressBar);

impl BarTask {
    fn new(kind: ProgressKind, title: &str) -> Self {
        let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
        let template = match kind {
            ProgressKind::Bytes => {
                "{prefix:20} [{elapsed_precise}] {wide_bar} {bytes}/{total_bytes} {bytes_per_sec} eta {eta}"
            }
            ProgressKind::Counter => "{prefix:20} [{elapsed_precise}] {wide_bar} {pos}/{len}",
            ProgressKind::Spinner => "{prefix:20} [{elapsed_precise}] {spinner}",
        };
        if let Ok(style) = ProgressStyle::with_template(template) {
            bar.set_style(style);
        }
        bar.set_prefix(title.to_string());
        bar.enable_steady_tick(Duration::from_millis(120));
        Self(bar)
    }
}

impl ProgressTask for BarTask {
    fn set_length(&self, len: u64) {
        self.0.set_length(len);
    }
    fn set_title(&self, title: &str) {
        self.0.set_prefix(title.to_string());
    }
    fn inc(&self, n: u64) {
        self.0.inc(n);
    }
    fn finish(&self) {
        self.0.finish_and_clear();
    }
}

/// Prints `<title> <n>%` to stderr at each 10% step of a byte progress.
#[derive(Debug)]
struct PlainTask {
    kind: ProgressKind,
    title: Mutex<String>,
    len: AtomicU64,
    pos: AtomicU64,
    printed_step: AtomicU64,
}

impl ProgressTask for PlainTask {
    fn set_length(&self, len: u64) {
        self.len.store(len, Ordering::Relaxed);
    }
    fn set_title(&self, title: &str) {
        if let Ok(mut t) = self.title.lock() {
            *t = title.to_string();
        }
    }
    fn inc(&self, n: u64) {
        if self.kind != ProgressKind::Bytes {
            return;
        }
        let pos = self.pos.fetch_add(n, Ordering::Relaxed) + n;
        let len = self.len.load(Ordering::Relaxed);
        if len == 0 {
            return;
        }
        let step = (pos.min(len) * 10 / len).min(10);
        let prev = self.printed_step.fetch_max(step, Ordering::Relaxed);
        if step > prev && step < 10 {
            let title = self.title.lock().map(|t| t.clone()).unwrap_or_default();
            eprintln!("progress: {} {}%", title.trim(), step * 10);
        }
    }
    fn finish(&self) {
        if self.kind == ProgressKind::Bytes && self.len.load(Ordering::Relaxed) > 0 {
            let title = self.title.lock().map(|t| t.clone()).unwrap_or_default();
            eprintln!("progress: {} 100%", title.trim());
        }
    }
}
