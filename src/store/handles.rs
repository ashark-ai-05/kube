use tokio::task::JoinHandle;

/// Every watch task belonging to the active cluster.
///
/// Switching clusters must abort all of them. Without this, each switch leaks
/// a live watch connection and its cache — invisible with one cluster, and
/// twenty times over with twenty.
#[derive(Default)]
pub struct WatchHandles {
    handles: Vec<JoinHandle<()>>,
}

impl WatchHandles {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, handle: JoinHandle<()>) {
        self.handles.push(handle);
    }

    pub fn len(&self) -> usize {
        self.handles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }

    /// Abort every watch and clear the registry. Returns how many were aborted.
    pub fn abort_all(&mut self) -> usize {
        let n = self.handles.len();
        for h in self.handles.drain(..) {
            h.abort();
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Increments a counter when dropped. Aborting a task drops its future,
    /// so this fires on cancellation but not while the task is merely sleeping.
    struct DropSignal(Arc<AtomicUsize>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn abort_all_actually_cancels_the_tasks_it_drops() {
        let cancelled = Arc::new(AtomicUsize::new(0));
        let mut handles = WatchHandles::new();

        for _ in 0..3 {
            let signal = DropSignal(cancelled.clone());
            handles.push(tokio::spawn(async move {
                // Held across the await, so it is dropped only if the task
                // is cancelled — not merely because it is still sleeping.
                let _signal = signal;
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }));
        }

        // Let each task reach its await point and take ownership of its signal.
        tokio::task::yield_now().await;
        assert_eq!(
            cancelled.load(Ordering::SeqCst),
            0,
            "nothing should be cancelled yet"
        );

        assert_eq!(handles.abort_all(), 3);
        assert!(
            handles.is_empty(),
            "aborted handles must not linger in the registry"
        );

        // Cancellation is processed asynchronously; give the runtime a chance.
        for _ in 0..10 {
            if cancelled.load(Ordering::SeqCst) == 3 {
                break;
            }
            tokio::task::yield_now().await;
        }

        assert_eq!(
            cancelled.load(Ordering::SeqCst),
            3,
            "every watch must actually be cancelled, not just forgotten"
        );
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
        assert_eq!(
            handles.len(),
            1,
            "switching clusters repeatedly must not accumulate handles"
        );
    }
}
