//! `--trace`: every action, TX and RX byte with its time, on stderr and in a `.trace.txt` file
//! next to the capture. Transparent logs are a key requirement (docs/cli.md "Trace").

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::out::Sink;

/// Cloneable trace handle; disabled when built with [`Tracer::off`].
#[derive(Clone, Default)]
pub struct Tracer(Option<Arc<Mutex<Vec<TraceSink>>>>);

enum TraceSink {
    Sink(Sink),
    File(File),
}

impl Tracer {
    pub fn off() -> Tracer {
        Tracer(None)
    }

    pub fn to(sink: Sink) -> Tracer {
        Tracer(Some(Arc::new(Mutex::new(vec![TraceSink::Sink(sink)]))))
    }

    /// Also write the trace to `path` (ignored when tracing is off).
    pub fn add_file(&self, path: &Path) -> std::io::Result<()> {
        if let Some(t) = &self.0 {
            let f = File::create(path)?;
            t.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(TraceSink::File(f));
        }
        Ok(())
    }

    pub fn enabled(&self) -> bool {
        self.0.is_some()
    }

    /// One trace line at `t_us` (transport clock, microseconds).
    pub fn line(&self, t_us: u64, msg: impl AsRef<str>) {
        let Some(t) = &self.0 else { return };
        let s = format!(
            "[{:>6}.{:03} ms] {}",
            t_us / 1000,
            t_us % 1000,
            msg.as_ref()
        );
        for sink in t.lock().unwrap_or_else(|e| e.into_inner()).iter_mut() {
            match sink {
                TraceSink::Sink(k) => k.line(&s),
                TraceSink::File(f) => {
                    let _ = writeln!(f, "{s}");
                }
            }
        }
    }
}
