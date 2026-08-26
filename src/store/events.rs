//! Fetching and formatting Kubernetes `Event`s for the detail pane's Events
//! tab — the tab someone actually opens to find out why a pod will not
//! start (`FailedScheduling`, `ImagePullBackOff`, `OOMKilled`).
//!
//! `Event` (`k8s-openapi` 0.28, feature `latest` = `v1_36`) carries THREE
//! independently-`Option` timestamps — `event_time` (modern,
//! microsecond-precision), `first_timestamp`/`last_timestamp` (legacy,
//! second-precision) — populated depending on which API path produced the
//! event, never guaranteed to be all three at once. See
//! `docs/superpowers/plan3-api-reference.md` section C8 for the field-level
//! detail this module is built against, and C7 for the field-selector list
//! form (`involvedObject.name=...,involvedObject.namespace=...`).
//!
//! This is a one-shot list (`fetch_events`), not a watch. C9 established
//! that `Event` CAN be watched exactly like any other `kube::Resource`
//! kind — nothing here forecloses that — but the interface this task owns
//! returns a plain `Vec<EventRow>`, matching the "fetch on demand when the
//! object changes" shape `store::table::fetch_table` already uses for the
//! Overview/YAML tabs' server-rendered columns. Wiring a live watch instead
//! is a `store::watch`-shaped addition Task 10 can make if it chooses to.

use crate::store::columns::format_age;
use crate::store::rbac::{WatchFailure, classify_kube_error};
use anyhow::anyhow;
use chrono::{DateTime, Utc};
use k8s_openapi::api::core::v1::Event;
use kube::Client;
use kube::api::{Api, ListParams};

/// One row the Events tab shows for one object's events, already formatted
/// for display (age computed, kind/reason/message defaulted for the
/// `Option` fields `Event` itself leaves unset).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub kind: String,
    pub reason: String,
    pub message: String,
    pub age: String,
    pub count: i32,
}

/// Build the field selector that scopes an Events list to one object.
///
/// A cluster-scoped object (a Node, a PersistentVolume) has no namespace at
/// all — appending an empty `involvedObject.namespace=` term would not be
/// merely redundant, it would ask the apiserver to match events whose
/// `involvedObject.namespace` equals the empty string, which is a
/// different (and wrong) query than "don't filter on namespace." So the
/// term is omitted entirely for `None`, never sent empty.
pub fn field_selector_for(name: &str, namespace: Option<&str>) -> String {
    match namespace {
        Some(ns) => format!("involvedObject.name={name},involvedObject.namespace={ns}"),
        None => format!("involvedObject.name={name}"),
    }
}

/// Compact age for one event, preferring the most recently-updated
/// timestamp available.
///
/// Precedence: `event_time` (the modern field; when the events.k8s.io API
/// path populated it, it IS the most recent occurrence by construction) →
/// `last_timestamp` (what `kubectl get events` shows for a legacy-path
/// event: the most recent occurrence of a repeating event) →
/// `first_timestamp` (better than nothing). Falling back to
/// `first_timestamp` ahead of `last_timestamp` would make a repeating event
/// look stale — exactly the wrong direction for the tab someone opens to
/// find out if something is still failing right now.
///
/// An event with none of the three timestamps set renders `"?"`, never a
/// fabricated `"0s"` — a wrong age is worse than an honest unknown here,
/// since `"0s"` reads as "just happened," the opposite of "we don't know."
fn event_age(event: &Event, now: DateTime<Utc>) -> String {
    let ts = event
        .event_time
        .as_ref()
        .map(|t| t.0)
        .or_else(|| event.last_timestamp.as_ref().map(|t| t.0))
        .or_else(|| event.first_timestamp.as_ref().map(|t| t.0));

    match ts {
        Some(t) => format_age(&format!("{t}"), now),
        None => "?".to_string(),
    }
}

/// Format raw `Event`s into display-ready rows. Pure and synchronous —
/// callers (the render path) must never do the network fetch themselves;
/// see `fetch_events`.
pub fn event_rows(events: &[Event], now: DateTime<Utc>) -> Vec<EventRow> {
    events
        .iter()
        .map(|e| EventRow {
            kind: e.type_.clone().unwrap_or_else(|| "Normal".to_string()),
            reason: e.reason.clone().unwrap_or_default(),
            message: e.message.clone().unwrap_or_default(),
            age: event_age(e, now),
            // Kubernetes omits `count` on a singleton (never-repeated) event
            // rather than sending 1 explicitly, so an absent count means it
            // happened once, not zero times.
            count: e.count.unwrap_or(1),
        })
        .collect()
}

/// Turn a failed events list into an error that says what actually
/// happened, using the SAME 403/404-vs-transient classification
/// `store::rbac::classify_kube_error` already uses for watches — corporate
/// RBAC can forbid listing events same as anything else, and re-deriving
/// that check here would be the two-sources-of-truth pattern this project
/// has already paid for once (see `store::rbac`'s own doc comment).
fn classify_fetch_error(name: &str, err: kube::Error) -> anyhow::Error {
    match classify_kube_error(&err) {
        WatchFailure::Forbidden { detail } => {
            anyhow!("events forbidden for {name}: {detail}")
        }
        WatchFailure::NotFound { detail } => {
            anyhow!(
                "events not found for {name} (it may have been removed from the cluster): {detail}"
            )
        }
        WatchFailure::Retryable => {
            anyhow::Error::new(err).context(format!("listing events for {name}"))
        }
    }
}

/// Fetch and format the events for one object.
///
/// A request, not a render-path call: this performs I/O and must only be
/// invoked when the active object changes (or on an explicit refresh),
/// exactly like `store::table::fetch_table`. `ns` is the object's
/// namespace, or an empty string for a cluster-scoped object — the empty
/// string, not `Option`, because that is the shape the object's own
/// `metadata.namespace` already comes to callers in throughout this
/// codebase (`DynamicObject::namespace()` returns `Option<String>`, but the
/// call site typically already has a `&str` scope value; an empty string
/// here reads the same way `Api::all` vs `Api::namespaced` already does
/// elsewhere).
pub async fn fetch_events(client: &Client, ns: &str, name: &str) -> anyhow::Result<Vec<EventRow>> {
    let namespace = if ns.is_empty() { None } else { Some(ns) };
    let selector = field_selector_for(name, namespace);
    let lp = ListParams::default().fields(&selector);

    let api: Api<Event> = match namespace {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    };

    let list = api
        .list(&lp)
        .await
        .map_err(|e| classify_fetch_error(name, e))?;

    Ok(event_rows(&list.items, Utc::now()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{MicroTime, Time};
    use kube::core::Status;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 25, 12, 0, 0).unwrap()
    }

    fn hours_ago(n: i64) -> DateTime<Utc> {
        now() - Duration::hours(n)
    }

    fn jiff_ts(dt: DateTime<Utc>) -> k8s_openapi::jiff::Timestamp {
        k8s_openapi::jiff::Timestamp::from_second(dt.timestamp()).expect("valid timestamp")
    }

    fn event(kind: &str, reason: &str) -> Event {
        Event {
            type_: Some(kind.to_string()),
            reason: Some(reason.to_string()),
            message: Some(format!("{reason} happened")),
            ..Default::default()
        }
    }

    fn event_without_timestamps() -> Event {
        event("Normal", "Scheduled")
    }

    fn event_with_times(first: DateTime<Utc>, last: DateTime<Utc>) -> Event {
        let mut e = event("Warning", "BackOff");
        e.first_timestamp = Some(Time(jiff_ts(first)));
        e.last_timestamp = Some(Time(jiff_ts(last)));
        e
    }

    fn api_kube_error(code: u16, reason: &str, message: &str) -> kube::Error {
        kube::Error::Api(Box::new(Status {
            code,
            reason: reason.to_string(),
            message: message.to_string(),
            ..Default::default()
        }))
    }

    // --- field_selector_for ---

    #[test]
    fn the_field_selector_scopes_to_one_object() {
        let s = field_selector_for("api-x2k", Some("payments"));
        assert!(s.contains("involvedObject.name=api-x2k"));
        assert!(s.contains("involvedObject.namespace=payments"));
    }

    #[test]
    fn a_cluster_scoped_object_omits_the_namespace_term() {
        let s = field_selector_for("node-1", None);
        assert!(s.contains("involvedObject.name=node-1"));
        assert!(
            !s.contains("namespace"),
            "cluster-scoped objects have no namespace to match"
        );
    }

    // --- event_rows: kind ---

    #[test]
    fn warnings_are_distinguishable_from_normal_events() {
        let rows = event_rows(
            &[event("Normal", "Scheduled"), event("Warning", "BackOff")],
            now(),
        );
        assert_eq!(rows[0].kind, "Normal");
        assert_eq!(rows[1].kind, "Warning");
    }

    // --- event_rows: age ---

    #[test]
    fn an_event_with_no_timestamps_still_renders() {
        // event_time, first_timestamp and last_timestamp are all Option.
        let rows = event_rows(&[event_without_timestamps()], now());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].age, "?");
    }

    #[test]
    fn age_prefers_the_most_recent_timestamp_available() {
        // last_timestamp is what kubectl shows; falling back to first would
        // make a repeating event look stale.
        let rows = event_rows(&[event_with_times(hours_ago(5), hours_ago(1))], now());
        assert_eq!(rows[0].age, "1h");
    }

    #[test]
    fn age_prefers_event_time_over_last_timestamp_when_both_are_set() {
        // event_time is the modern, events.k8s.io-path field. Times differ
        // by enough (1h vs 3h vs 5h) that a wrong precedence produces a
        // DIFFERENT formatted age, not merely an unobserved tie.
        let mut e = event_with_times(hours_ago(5), hours_ago(3));
        e.event_time = Some(MicroTime(jiff_ts(hours_ago(1))));
        let rows = event_rows(&[e], now());
        assert_eq!(rows[0].age, "1h");
    }

    #[test]
    fn event_row_count_defaults_to_one_when_absent() {
        let rows = event_rows(&[event("Normal", "Pulled")], now());
        assert_eq!(rows[0].count, 1);
    }

    #[test]
    fn event_row_carries_the_real_repeat_count() {
        let mut e = event("Warning", "BackOff");
        e.count = Some(7);
        let rows = event_rows(&[e], now());
        assert_eq!(rows[0].count, 7);
    }

    #[test]
    fn event_row_carries_reason_and_message() {
        let rows = event_rows(&[event("Warning", "FailedScheduling")], now());
        assert_eq!(rows[0].reason, "FailedScheduling");
        assert_eq!(rows[0].message, "FailedScheduling happened");
    }

    // --- classify_fetch_error: reuse classify_kube_error, don't re-derive ---

    #[test]
    fn a_forbidden_listing_says_so_explicitly() {
        let err = classify_fetch_error(
            "web-1",
            api_kube_error(403, "Forbidden", "events is forbidden"),
        );
        let msg = err.to_string();
        assert!(
            msg.to_lowercase().contains("forbidden"),
            "a forbidden listing must say so, not render as empty: {msg}"
        );
        assert!(msg.contains("web-1"), "got {msg}");
    }

    #[test]
    fn a_transient_error_is_not_mislabeled_forbidden() {
        let err = classify_fetch_error(
            "web-1",
            api_kube_error(500, "InternalError", "etcdserver: request timed out"),
        );
        let msg = format!("{err:#}");
        assert!(
            !msg.to_lowercase().contains("forbidden"),
            "a 500 must not be mislabeled forbidden: {msg}"
        );
        assert!(msg.contains("timed out"), "got {msg}");
    }
}
