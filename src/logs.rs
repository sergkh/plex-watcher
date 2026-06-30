use serde::Serialize;
use std::{
    collections::VecDeque,
    fmt,
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};
use tracing::{Event, Subscriber};
use tracing_subscriber::{layer::Context, Layer};

const MAX_LOG_ENTRIES: usize = 500;

static LOGS: OnceLock<Mutex<VecDeque<LogEntry>>> = OnceLock::new();

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub timestamp_ms: u64,
    pub level: String,
    pub target: String,
    pub message: String,
}

pub struct LogLayer;

impl<S> Layer<S> for LogLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let mut visitor = LogVisitor::default();
        event.record(&mut visitor);

        push(LogEntry {
            timestamp_ms: now_ms(),
            level: metadata.level().to_string(),
            target: metadata.target().to_string(),
            message: visitor.finish(),
        });
    }
}

pub fn entries() -> Vec<LogEntry> {
    logs()
        .lock()
        .map(|entries| entries.iter().cloned().collect())
        .unwrap_or_default()
}

fn push(entry: LogEntry) {
    if let Ok(mut entries) = logs().lock() {
        entries.push_back(entry);
        while entries.len() > MAX_LOG_ENTRIES {
            entries.pop_front();
        }
    }
}

fn logs() -> &'static Mutex<VecDeque<LogEntry>> {
    LOGS.get_or_init(|| Mutex::new(VecDeque::with_capacity(MAX_LOG_ENTRIES)))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

#[derive(Default)]
struct LogVisitor {
    message: Option<String>,
    fields: Vec<String>,
}

impl LogVisitor {
    fn finish(self) -> String {
        let mut parts = Vec::new();
        if let Some(message) = self.message {
            parts.push(message);
        }
        parts.extend(self.fields);
        parts.join(" ")
    }
}

impl tracing::field::Visit for LogVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        let value = format!("{value:?}");
        if field.name() == "message" {
            self.message = Some(value.trim_matches('"').to_string());
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }
}
