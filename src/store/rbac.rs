//! Classifying `watcher::Error` into "permanent" (RBAC-forbidden, kind gone)
//! vs. "retryable" (a blip, a restart, a rolling apiserver).
//!
//! `kube::runtime::watcher::Error`'s own doc comment says all five variants
//! are "considered retryable from a watcher's point of view" — that is true
//! at the watcher's own level, but a 403 wrapped inside one of those variants
//! is not a blip: the identity will never gain the permission by waiting.
//! This module unwraps one level further, into `kube::Error::Api(Status)`,
//! to tell the two apart. See `docs/superpowers/plan3-api-reference.md`
//! section B6 for the verified error shape this is built against.

use kube::core::Status;
use kube::runtime::watcher;

/// What a failed watch means for the caller: keep retrying, or give up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchFailure {
    /// 403 — permanent for this identity. Do not retry.
    Forbidden { detail: String },
    /// 404 — the kind is gone (e.g. a CRD was uninstalled). Do not retry.
    NotFound { detail: String },
    /// Anything else — a blip, a restart, a rolling apiserver. Retry.
    Retryable,
}

/// Classify a `watcher::Error` as permanent or retryable.
///
/// Defaults to `Retryable` for anything that isn't unambiguously a 403/404
/// `Status` from the apiserver: wrongly retrying a permanent failure only
/// wastes requests, while wrongly giving up on a transient one hides data
/// the user actually has access to. The former is the safer direction.
pub fn classify(err: &watcher::Error) -> WatchFailure {
    match err {
        watcher::Error::InitialListFailed(kube::Error::Api(status))
        | watcher::Error::WatchStartFailed(kube::Error::Api(status))
        | watcher::Error::WatchFailed(kube::Error::Api(status)) => classify_status(status),
        watcher::Error::WatchError(status) => classify_status(status),
        watcher::Error::InitialListFailed(_)
        | watcher::Error::WatchStartFailed(_)
        | watcher::Error::WatchFailed(_)
        | watcher::Error::NoResourceVersion => WatchFailure::Retryable,
    }
}

fn classify_status(status: &Status) -> WatchFailure {
    if status.is_forbidden() {
        WatchFailure::Forbidden {
            detail: status.message.clone(),
        }
    } else if status.is_not_found() {
        WatchFailure::NotFound {
            detail: status.message.clone(),
        }
    } else {
        WatchFailure::Retryable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `kube::Error::Api` carrying a `Status` with the given code
    /// and reason, matching what a real apiserver response looks like.
    fn api_error(code: u16, reason: &str, message: &str) -> kube::Error {
        kube::Error::Api(Box::new(Status {
            code,
            message: message.to_string(),
            reason: reason.to_string(),
            ..Default::default()
        }))
    }

    fn transport_error() -> kube::Error {
        // Not a `kube::Error::Api` at all — an io-level failure that never
        // reached the apiserver. Any raw non-Api error, e.g. a hyper/tower
        // connect failure, takes this shape.
        kube::Error::LinesCodecMaxLineLengthExceeded
    }

    // --- 403 vs 404 vs 500 vs transport, through every variant that can carry one ---

    #[test]
    fn forbidden_via_initial_list_failed_is_forbidden_not_retryable() {
        let err = watcher::Error::InitialListFailed(api_error(
            403,
            "Forbidden",
            "pods is forbidden: User \"u\" cannot list resource \"pods\" at the cluster scope",
        ));
        match classify(&err) {
            WatchFailure::Forbidden { detail } => {
                assert!(detail.contains("forbidden"));
            }
            other => panic!("expected Forbidden, got {other:?}"),
        }
    }

    #[test]
    fn forbidden_via_watch_start_failed_is_forbidden() {
        let err = watcher::Error::WatchStartFailed(api_error(403, "Forbidden", "nope"));
        assert!(matches!(classify(&err), WatchFailure::Forbidden { .. }));
    }

    #[test]
    fn forbidden_via_watch_failed_is_forbidden() {
        let err = watcher::Error::WatchFailed(api_error(403, "Forbidden", "nope"));
        assert!(matches!(classify(&err), WatchFailure::Forbidden { .. }));
    }

    #[test]
    fn forbidden_via_watch_error_mid_stream_is_forbidden() {
        // `WatchError` carries a bare `Status`, not a `kube::Error` — a
        // `WatchEvent::Error` frame delivered mid-stream rather than a
        // request-level failure. It is a distinct source of a 403 and must
        // be caught the same way as the request-failure variants.
        let status = Status {
            code: 403,
            message: "pods is forbidden mid-stream".to_string(),
            reason: "Forbidden".to_string(),
            ..Default::default()
        };
        let err = watcher::Error::WatchError(Box::new(status));
        assert!(matches!(classify(&err), WatchFailure::Forbidden { .. }));
    }

    #[test]
    fn not_found_is_distinguishable_from_forbidden() {
        // A wrong implementation that treats every non-2xx Status as
        // Forbidden would pass a lone "404 classifies as not-retryable"
        // check. Assert the *kind* of permanent failure is right, not just
        // that it isn't Retryable.
        let err = watcher::Error::InitialListFailed(api_error(
            404,
            "NotFound",
            "widgets.example.com not found",
        ));
        match classify(&err) {
            WatchFailure::NotFound { detail } => assert!(detail.contains("widgets")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn a_500_is_retryable_not_forbidden() {
        // Negative case for the 403 tests above: a same-shape Api error
        // with a server-side code must NOT be classified as Forbidden. If
        // `classify` ever collapsed to "any Api status is Forbidden", this
        // fails while the 403 tests keep passing.
        let err = watcher::Error::InitialListFailed(api_error(
            500,
            "InternalError",
            "etcdserver: request timed out",
        ));
        assert_eq!(classify(&err), WatchFailure::Retryable);
    }

    #[test]
    fn a_transport_error_is_retryable() {
        // Not a `kube::Error::Api` at all — must fall through to Retryable
        // regardless of which watcher::Error variant wraps it.
        let err = watcher::Error::WatchFailed(transport_error());
        assert_eq!(classify(&err), WatchFailure::Retryable);
    }

    #[test]
    fn no_resource_version_is_retryable() {
        assert_eq!(
            classify(&watcher::Error::NoResourceVersion),
            WatchFailure::Retryable
        );
    }

    #[test]
    fn a_410_gone_status_is_retryable_not_forbidden_or_not_found() {
        // 410 Gone (stale resourceVersion) is the canonical *transient*
        // apiserver error kube-runtime already relists on internally. It
        // must not be mistaken for either permanent category.
        let err =
            watcher::Error::WatchStartFailed(api_error(410, "Gone", "resource version too old"));
        assert_eq!(classify(&err), WatchFailure::Retryable);
    }
}
