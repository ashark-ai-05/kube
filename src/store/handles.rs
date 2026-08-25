#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn abort_all_stops_running_tasks_and_reports_the_count() {
        let ran = Arc::new(AtomicUsize::new(0));
        let mut handles = WatchHandles::new();

        for _ in 0..3 {
            let ran = ran.clone();
            handles.push(tokio::spawn(async move {
                // Long enough that abort lands first.
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                ran.fetch_add(1, Ordering::SeqCst);
            }));
        }

        assert_eq!(handles.len(), 3);
        assert_eq!(handles.abort_all(), 3);
        assert!(handles.is_empty(), "aborted handles must not linger in the registry");

        tokio::task::yield_now().await;
        assert_eq!(ran.load(Ordering::SeqCst), 0, "no task should have run to completion");
    }

    #[tokio::test]
    async fn abort_all_on_an_empty_registry_is_zero_not_a_panic() {
        let mut handles = WatchHandles::new();
        assert_eq!(handles.abort_all(), 0);
    }

    #[tokio::test]
    async fn a_registry_can_be_refilled_after_abort() {
        let mut handles = WatchHandles::new();
        handles.push(tokio::spawn(async {}));
        handles.abort_all();
        handles.push(tokio::spawn(async {}));
        assert_eq!(handles.len(), 1, "switching clusters repeatedly must not accumulate handles");
    }
}
