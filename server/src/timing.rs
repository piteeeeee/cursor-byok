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
