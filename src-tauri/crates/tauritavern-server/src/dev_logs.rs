//! In-memory development logs behind `/backendlog` and `/llmlog`.
//!
//! The desktop host keeps the same data in `tauritavern::infrastructure::logging`
//! and pushes it through Tauri events; the browser has no event channel, so the
//! viewers poll these stores by monotonically increasing `id`.

use std::collections::VecDeque;
use std::fmt;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::Subscriber;
use tracing_subscriber::layer::{Context, Layer};
use tt_application::dto::dev_observability_dto::{
    BackendLogEntryDto, LlmApiLogIndexEntryDto, LlmApiLogPreviewDto, LlmApiLogRawDto,
};

const BACKEND_LOG_BUFFER_LIMIT: usize = 800;
const BACKEND_LOG_MAX_MESSAGE_BYTES: usize = 3072;

/// Process-wide like the tracing subscriber that feeds it.
pub static BACKEND_LOGS: BackendLogStore = BackendLogStore {
    next_id: AtomicU64::new(1),
    entries: Mutex::new(VecDeque::new()),
};

pub struct BackendLogStore {
    next_id: AtomicU64,
    entries: Mutex<VecDeque<BackendLogEntryDto>>,
}

impl BackendLogStore {
    pub fn tail(&self, limit: usize) -> Vec<BackendLogEntryDto> {
        let entries = self.entries.lock();
        entries.iter().skip(entries.len().saturating_sub(limit)).cloned().collect()
    }

    fn push(&self, level: String, target: String, mut message: String) {
        truncate_utf8(&mut message, BACKEND_LOG_MAX_MESSAGE_BYTES);
        let mut entries = self.entries.lock();
        // Id allocation under the lock keeps the buffer ordered by id, which
        // the polling viewer relies on.
        entries.push_back(BackendLogEntryDto {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            level,
            target,
            message,
        });
        if entries.len() > BACKEND_LOG_BUFFER_LIMIT {
            entries.pop_front();
        }
    }
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes - 1;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.push('…');
}

pub struct BackendLogLayer;

impl<S: Subscriber> Layer<S> for BackendLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        // Forwarded browser logs belong to the frontend viewer.
        if metadata.target() == "frontend" {
            return;
        }
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        BACKEND_LOGS.push(
            metadata.level().to_string(),
            metadata.target().to_string(),
            visitor.into_message(),
        );
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
    fields: Vec<String>,
}

impl MessageVisitor {
    fn into_message(self) -> String {
        let fields = self.fields.join(" ");
        match (self.message, fields.is_empty()) {
            (Some(message), true) => message,
            (Some(message), false) => format!("{message} ({fields})"),
            (None, _) => fields,
        }
    }
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        } else {
            self.fields.push(format!("{}={value:?}", field.name()));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }
}

pub struct LlmApiLogEntry {
    pub preview: LlmApiLogPreviewDto,
    pub raw: LlmApiLogRawDto,
}

/// Last `keep` LLM API exchanges (the user's `dev.llm_api_keep` setting).
// ponytail: memory-only, lost on restart; the desktop host persists to its log dir.
pub struct LlmApiLogStore {
    next_id: AtomicU64,
    keep: AtomicU64,
    entries: Mutex<VecDeque<LlmApiLogEntry>>,
}

impl LlmApiLogStore {
    pub fn new(keep: u32) -> Self {
        Self {
            next_id: AtomicU64::new(1),
            keep: AtomicU64::new(u64::from(keep)),
            entries: Mutex::new(VecDeque::new()),
        }
    }

    pub fn set_keep(&self, keep: u32) {
        self.keep.store(u64::from(keep), Ordering::Relaxed);
        self.enforce_keep(&mut self.entries.lock());
    }

    fn enforce_keep(&self, entries: &mut VecDeque<LlmApiLogEntry>) {
        let keep = self.keep.load(Ordering::Relaxed) as usize;
        while entries.len() > keep {
            entries.pop_front();
        }
    }

    /// Assigns the id under the lock so stored entries stay ordered by id.
    pub fn record(&self, mut entry: LlmApiLogEntry) {
        let mut entries = self.entries.lock();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        entry.preview.id = id;
        entry.raw.id = id;
        entries.push_back(entry);
        self.enforce_keep(&mut entries);
    }

    pub fn index(&self, limit: usize) -> Vec<LlmApiLogIndexEntryDto> {
        let entries = self.entries.lock();
        entries
            .iter()
            .skip(entries.len().saturating_sub(limit))
            .map(|entry| {
                let p = &entry.preview;
                LlmApiLogIndexEntryDto {
                    id: p.id,
                    timestamp_ms: p.timestamp_ms,
                    level: p.level.clone(),
                    ok: p.ok,
                    source: p.source.clone(),
                    model: p.model.clone(),
                    endpoint: p.endpoint.clone(),
                    duration_ms: p.duration_ms,
                    stream: p.stream,
                }
            })
            .collect()
    }

    pub fn preview(&self, id: u64) -> Option<LlmApiLogPreviewDto> {
        self.find(id, |entry| entry.preview.clone())
    }

    pub fn raw(&self, id: u64) -> Option<LlmApiLogRawDto> {
        self.find(id, |entry| entry.raw.clone())
    }

    fn find<T>(&self, id: u64, map: impl FnOnce(&LlmApiLogEntry) -> T) -> Option<T> {
        self.entries.lock().iter().find(|entry| entry.preview.id == id).map(map)
    }
}
