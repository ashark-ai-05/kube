use super::pod::Identity;
use crate::{
    app::event::AppEvent,
    store::events::{EventRow, fetch_events_for_uid},
};
use kube::Client;
use std::time::Duration;
use tokio::{sync::mpsc, task::JoinHandle};
#[derive(Default)]
pub struct Events {
    pub rows: Vec<EventRow>,
    pub error: Option<String>,
    target: Option<Identity>,
    task: Option<JoinHandle<()>>,
    reply: Option<mpsc::Receiver<Result<Vec<EventRow>, String>>>,
}
impl Drop for Events {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl Events {
    pub fn set_target(
        &mut self,
        target: Option<Identity>,
        client: Client,
        wake: mpsc::UnboundedSender<AppEvent>,
    ) {
        if target == self.target {
            return;
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.reply = None;
        self.rows.clear();
        self.error = None;
        self.target = target.clone();
        let Some(target) = target else { return };
        let (tx, rx) = mpsc::channel(1);
        self.reply = Some(rx);
        self.task = Some(tokio::spawn(async move {
            // Rapid navigation cancels this delay before making an API request.
            tokio::time::sleep(Duration::from_millis(180)).await;
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let result = tokio::time::timeout(
                    Duration::from_secs(4),
                    fetch_events_for_uid(
                        &client,
                        Some(&target.namespace),
                        &target.name,
                        target.uid.as_deref(),
                    ),
                )
                .await
                .map_err(|_| "Events request timed out".to_string())
                .and_then(|r| {
                    r.map_err(|_| "Events unavailable · check events read access".into())
                });
                if tx.send(result).await.is_err() {
                    break;
                }
                let _ = wake.send(AppEvent::Wake);
            }
        }));
    }
    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &mut self.reply {
            while let Ok(result) = rx.try_recv() {
                changed = true;
                match result {
                    Ok(mut rows) => {
                        rows.sort_by(|a, b| {
                            crate::store::table::age_seconds(&a.age)
                                .unwrap_or(f64::INFINITY)
                                .total_cmp(
                                    &crate::store::table::age_seconds(&b.age)
                                        .unwrap_or(f64::INFINITY),
                                )
                        });
                        self.rows = rows;
                        self.rows.truncate(32);
                        self.error = None
                    }
                    Err(e) => {
                        self.rows.clear();
                        self.error = Some(e)
                    }
                }
            }
        }
        changed
    }
    pub fn note_for(&self, target: &Identity) -> String {
        // Selection can change during rendering, before the next subscription
        // is armed. Never present the previous pod's event under the new pod.
        if self.target.as_ref() != Some(target) {
            return "Loading events for selected pod…".into();
        }
        if let Some(error) = &self.error {
            return error.clone();
        }
        if let Some(e) = self
            .rows
            .iter()
            .find(|r| r.kind == "Warning")
            .or_else(|| self.rows.first())
        {
            format!("{} · {} · {} · {}", e.kind, e.reason, e.age, e.message)
                .chars()
                .filter(|c| !c.is_control())
                .collect()
        } else if self.reply.is_some() {
            "No events observed yet · Enter for full inspection".into()
        } else {
            "Select a pod to inspect its events".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rendered_event_never_belongs_to_a_previous_selection_or_uid() {
        let previous = Identity {
            namespace: "demo".into(),
            name: "web".into(),
            uid: Some("old".into()),
        };
        let current = Identity {
            uid: Some("new".into()),
            ..previous.clone()
        };
        let events = Events {
            target: Some(previous.clone()),
            rows: vec![EventRow {
                kind: "Warning".into(),
                reason: "Unhealthy".into(),
                message: "old pod's failed probe".into(),
                age: "1s".into(),
                count: 1,
            }],
            error: None,
            task: None,
            reply: None,
        };
        assert!(events.note_for(&previous).contains("failed probe"));
        assert!(!events.note_for(&current).contains("failed probe"));
        assert!(events.note_for(&current).contains("Loading"));
    }
    #[tokio::test]
    async fn changing_target_cancels_old_worker_and_discards_its_reply() {
        let client =
            Client::try_from(kube::Config::new("http://127.0.0.1:1".parse().unwrap())).unwrap();
        let (wake, _) = mpsc::unbounded_channel();
        let mut events = Events::default();
        events.set_target(
            Some(Identity {
                namespace: "demo".into(),
                name: "web".into(),
                uid: Some("old".into()),
            }),
            client.clone(),
            wake.clone(),
        );
        let abort = events.task.as_ref().unwrap().abort_handle();
        events.error = Some("old cluster error".into());
        events.set_target(
            Some(Identity {
                namespace: "demo".into(),
                name: "web".into(),
                uid: Some("new".into()),
            }),
            client.clone(),
            wake.clone(),
        );
        tokio::task::yield_now().await;
        assert!(abort.is_finished());
        assert!(events.error.is_none() && events.rows.is_empty());
        let abort = events.task.as_ref().unwrap().abort_handle();
        events.set_target(None, client, wake);
        tokio::task::yield_now().await;
        assert!(abort.is_finished() && events.reply.is_none());
    }
}
