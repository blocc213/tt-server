//! Background generation: the provider call outlives the browser connection.
//!
//! Mobile browsers suspend hidden pages and drop their sockets. With the
//! user's opt-in (`background: true` on the generate request) the server keeps
//! the upstream call running, buffers every SSE payload, and lets the page
//! re-attach later from the last event it saw. Only an explicit Stop cancels.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use axum::response::sse::Event;
use futures_util::{Stream, StreamExt};
use tokio::sync::watch;

/// How long a finished job stays readable. A returning phone must find its
/// result; without an explicit ack there is no earlier safe point to drop it.
pub const RETENTION: Duration = Duration::from_secs(60 * 60);

#[derive(Default)]
struct JobState {
    /// SSE `data` payloads in order; the index is the event id.
    events: Vec<String>,
    done: bool,
}

pub struct Job {
    pub stream: bool,
    state: watch::Sender<JobState>,
}

impl Job {
    pub fn push(&self, data: String) {
        self.state.send_modify(|state| state.events.push(data));
    }

    pub fn finish(&self) {
        self.state.send_modify(|state| state.done = true);
    }

    /// Waits for completion and returns the single payload of a
    /// non-streaming job.
    pub async fn result(&self) -> String {
        let mut rx = self.state.subscribe();
        let state = rx
            .wait_for(|state| state.done)
            .await
            .expect("job sender lives as long as the job");
        state.events.first().cloned().unwrap_or_default()
    }

    /// Replays events from index `from`, then follows live until the job ends.
    pub fn replay(&self, from: usize) -> impl Stream<Item = Result<Event, Infallible>> + use<> {
        let rx = self.state.subscribe();
        futures_util::stream::unfold((rx, from), |(mut rx, next)| async move {
            loop {
                let (batch, done) = {
                    let state = rx.borrow_and_update();
                    let batch = state.events.get(next..).map(<[_]>::to_vec).unwrap_or_default();
                    (batch, state.done)
                };
                if !batch.is_empty() {
                    let after = next + batch.len();
                    let events = batch.into_iter().enumerate().map(move |(offset, data)| {
                        Ok(Event::default().id((next + offset).to_string()).data(data))
                    });
                    return Some((futures_util::stream::iter(events), (rx, after)));
                }
                if done || rx.changed().await.is_err() {
                    return None;
                }
            }
        })
        .flatten()
    }
}

// ponytail: process-local map; jobs die with the server process, which the
// client reports as an expired generation instead of silently regenerating.
static JOBS: LazyLock<Mutex<HashMap<String, Arc<Job>>>> = LazyLock::new(Default::default);

/// Returns the job for `id`, and whether this call created it. A retried
/// generate request with the same id attaches instead of paying twice.
pub fn get_or_insert(id: &str, stream: bool) -> (Arc<Job>, bool) {
    let mut jobs = JOBS.lock().expect("background job map poisoned");
    if let Some(job) = jobs.get(id) {
        return (job.clone(), false);
    }
    let job = Arc::new(Job {
        stream,
        state: watch::Sender::new(JobState::default()),
    });
    jobs.insert(id.to_string(), job.clone());
    (job, true)
}

pub fn get(id: &str) -> Option<Arc<Job>> {
    JOBS.lock().expect("background job map poisoned").get(id).cloned()
}

pub fn remove(id: &str) {
    JOBS.lock().expect("background job map poisoned").remove(id);
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn ids_and_data(job: &Job, from: usize) -> Vec<String> {
        // Event has no accessors; its wire form carries both id and data.
        job.replay(from)
            .map(|event| format!("{:?}", event.unwrap()))
            .collect()
            .await
    }

    #[tokio::test]
    async fn replay_resumes_from_offset_and_follows_live_events_until_done() {
        let (job, created) = get_or_insert("test-replay", true);
        assert!(created);
        assert!(!get_or_insert("test-replay", true).1, "same id must attach");

        job.push("a".into());
        job.push("b".into());
        let reader = tokio::spawn({
            let job = job.clone();
            async move { ids_and_data(&job, 1).await }
        });
        tokio::task::yield_now().await;
        job.push("c".into());
        job.finish();

        let events = reader.await.unwrap();
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(events[0].contains("id: 1") && events[0].contains("data: b"));
        assert!(events[1].contains("id: 2") && events[1].contains("data: c"));
        assert!(ids_and_data(&job, 3).await.is_empty(), "caught-up finished job ends");
        remove("test-replay");
        assert!(get("test-replay").is_none());
    }

    #[tokio::test]
    async fn non_stream_result_waits_for_completion() {
        let (job, _) = get_or_insert("test-result", false);
        let waiter = tokio::spawn({
            let job = job.clone();
            async move { job.result().await }
        });
        job.push("{\"ok\":true}".into());
        job.finish();
        assert_eq!(waiter.await.unwrap(), "{\"ok\":true}");
        remove("test-result");
    }
}
