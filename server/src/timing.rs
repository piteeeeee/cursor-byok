//! Times the assistant-internal Run pipeline.

use std::time::{Duration, Instant};

/// Logs the elapsed time of each Run pipeline stage at info level.
///
/// The first token of a Run is dominated by these stages, and the database is
/// user-sized, so every stage reports its own duration: a single total cannot
/// say whether the wait was a query, a Blob round trip, or Cursor itself.
pub(crate) struct Stages {
    subject: &'static str,
    previous: Instant,
    started: Instant,
}

impl Stages {
    pub(crate) fn new(subject: &'static str) -> Self {
        let now = Instant::now();
        Self {
            subject,
            previous: now,
            started: now,
        }
    }

    pub(crate) fn restart_interval(&mut self) {
        self.previous = Instant::now();
    }

    pub(crate) fn mark(&mut self, stage: &'static str) {
        let now = Instant::now();
        tracing::info!(
            subject = self.subject,
            stage,
            elapsed_ms = millis(now.duration_since(self.previous)),
            "run pipeline stage"
        );
        self.previous = now;
    }

    pub(crate) fn finish(&self, stage: &'static str) {
        tracing::info!(
            subject = self.subject,
            stage,
            total_ms = millis(self.started.elapsed()),
            "run pipeline total"
        );
    }
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Collects the `info!` lines this module emits, so the stage and total
    /// records can be asserted without a live run.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<String>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(bytes).into_owned());
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn each_stage_reports_its_own_elapsed_time_and_a_total() {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter("cursor_server::timing=info")
            .with_ansi(false)
            .with_writer(capture.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let mut stages = Stages::new("prepare");
        std::thread::sleep(Duration::from_millis(2));
        stages.mark("load checkpoint messages");
        stages.restart_interval();
        stages.mark("project messages");
        stages.finish("prepare complete");

        let output = capture.0.lock().unwrap().join("");
        assert!(
            output.contains("stage=\"load checkpoint messages\""),
            "{output}"
        );
        assert!(output.contains("stage=\"project messages\""), "{output}");
        assert!(output.contains("stage=\"prepare complete\""), "{output}");
        assert!(output.contains("elapsed_ms="), "{output}");
        assert!(output.contains("total_ms="), "{output}");
    }
}
