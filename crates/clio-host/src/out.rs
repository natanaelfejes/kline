//! Output: one JSON object per line with `--json`, human text otherwise. Shared sinks so tests
//! can capture stdout/stderr in-process.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use serde_json::Value;

/// A shared writer (stdout, stderr, or an in-memory buffer in tests).
#[derive(Clone)]
pub struct Sink(Arc<Mutex<Box<dyn Write + Send>>>);

impl Sink {
    pub fn new(w: impl Write + Send + 'static) -> Sink {
        Sink(Arc::new(Mutex::new(Box::new(w))))
    }

    pub fn stdout() -> Sink {
        Sink::new(io::stdout())
    }

    pub fn stderr() -> Sink {
        Sink::new(io::stderr())
    }

    /// In-memory sink and the buffer it writes to.
    pub fn buffer() -> (Sink, Arc<Mutex<Vec<u8>>>) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        (Sink::new(SharedBuf(buf.clone())), buf)
    }

    pub fn line(&self, s: &str) {
        let mut w = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // Output errors (closed pipe) must not abort a session mid-way; the capture is the record.
        let _ = writeln!(w, "{s}");
        let _ = w.flush();
    }
}

struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Command output in the selected mode.
#[derive(Clone)]
pub struct Out {
    pub json: bool,
    pub stdout: Sink,
    pub stderr: Sink,
}

impl Out {
    /// Emit one event: the JSON object in `--json` mode, else `human` (skipped when empty).
    pub fn emit(&self, event: Value, human: impl AsRef<str>) {
        if self.json {
            self.stdout.line(&event.to_string());
        } else if !human.as_ref().is_empty() {
            self.stdout.line(human.as_ref());
        }
    }

    /// A note for humans on stderr (both modes; stdout stays pure JSON with `--json`).
    pub fn note(&self, s: impl AsRef<str>) {
        self.stderr.line(s.as_ref());
    }
}
