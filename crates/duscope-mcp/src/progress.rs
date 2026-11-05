//! Progress: what a running tool call tells the client about how far it has
//! got, as `notifications/progress`.
//!
//! A client asks for progress by naming a `progressToken` in the request's
//! `_meta`; a request without one gets none. While the call runs, the worker
//! looks every [`PROGRESS_EVERY`] at the entries its verb has counted so far
//! (a scan counts each entry its walk delivers, the other verbs count
//! nothing) and, when the count has grown since the last notification, sends
//! one with the token and the count. The protocol has a notification's
//! `progress` grow every time, so a call that counted nothing new, or nothing
//! at all, sends nothing, and the count is never known in advance, so there
//! is no `total`. No notification follows the call's response.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Map, Value, json};

/// How often a running call reports its progress: every two seconds.
pub const PROGRESS_EVERY: Duration = Duration::from_secs(2);

/// The notification's method.
pub const PROGRESS: &str = "notifications/progress";

/// The token a request's `_meta.progressToken` names, which every
/// notification about the request carries back: a string or an integer, as
/// the protocol has it. A request naming none, or something else, asks for
/// no progress.
pub(crate) fn token(params: Option<&Map<String, Value>>) -> Option<Value> {
    let token = params?.get("_meta")?.as_object()?.get("progressToken")?;
    let valid = token.is_string() || token.is_i64() || token.is_u64();
    valid.then(|| token.clone())
}

/// One call's progress: its token, the count its verb adds to, and the
/// count the last notification carried.
#[derive(Debug)]
pub(crate) struct Reporter {
    token: Value,
    entries: Arc<AtomicU64>,
    sent: u64,
}

impl Reporter {
    /// The progress of the call that named `token`, whose verb counts into
    /// `entries`.
    pub(crate) fn new(token: Value, entries: Arc<AtomicU64>) -> Reporter {
        Reporter {
            token,
            entries,
            sent: 0,
        }
    }

    /// The params of the next notification, when the count has grown since
    /// the last one; `None` when it has not.
    pub(crate) fn next(&mut self) -> Option<Value> {
        let counted = self.entries.load(Ordering::Relaxed);
        if counted <= self.sent {
            return None;
        }
        self.sent = counted;
        Some(json!({
            "progressToken": self.token,
            "progress": counted,
            "message": format!("{counted} entries counted"),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(meta: &Value) -> Map<String, Value> {
        json!({ "name": "duscope_scan", "_meta": meta })
            .as_object()
            .unwrap()
            .clone()
    }

    #[test]
    fn p3_mcp_05_a_progress_token_is_a_string_or_an_integer() {
        assert_eq!(
            token(Some(&params(&json!({"progressToken": "scan-1"})))),
            Some(json!("scan-1"))
        );
        assert_eq!(
            token(Some(&params(&json!({"progressToken": 7})))),
            Some(json!(7))
        );
        for refused in [json!(1.5), json!(null), json!({"a": 1}), json!([1])] {
            assert_eq!(
                token(Some(&params(&json!({ "progressToken": refused })))),
                None,
                "{refused}"
            );
        }
        assert_eq!(token(Some(&params(&json!({})))), None);
        assert_eq!(token(None), None);
    }

    #[test]
    fn p3_mcp_05_progress_is_sent_only_when_the_count_grows() {
        let entries = Arc::new(AtomicU64::new(0));
        let mut reporter = Reporter::new(json!("t"), Arc::clone(&entries));
        assert_eq!(reporter.next(), None, "nothing counted yet");
        entries.store(1_204, Ordering::Relaxed);
        assert_eq!(
            reporter.next(),
            Some(json!({
                "progressToken": "t",
                "progress": 1_204,
                "message": "1204 entries counted",
            }))
        );
        assert_eq!(reporter.next(), None, "the count has not grown");
        entries.fetch_add(1, Ordering::Relaxed);
        assert_eq!(reporter.next().unwrap()["progress"], 1_205);
    }
}
