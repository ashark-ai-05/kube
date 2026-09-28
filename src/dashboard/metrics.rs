use crate::app::event::AppEvent;
use chrono::{DateTime, Utc};
use kube::{Client, ResourceExt, api::DynamicObject};
use std::{collections::BTreeMap, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle};

#[derive(Clone, Copy, Default, Debug)]
pub struct Usage {
    pub cpu_milli: f64,
    pub memory_bytes: f64,
}
impl Usage {
    pub fn add(&mut self, other: Self) {
        self.cpu_milli += other.cpu_milli;
        self.memory_bytes += other.memory_bytes;
    }
}
pub struct PodUsage {
    pub usage: Usage,
    pub timestamp: DateTime<Utc>,
}
type Snapshot = BTreeMap<(String, String), PodUsage>;
#[derive(Default)]
pub struct MetricsFeed {
    pub latest: Snapshot,
    pub error: Option<String>,
    pub revision: u64,
    task: Option<JoinHandle<()>>,
    reply: Option<mpsc::Receiver<Result<Snapshot, String>>>,
}
impl Drop for MetricsFeed {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl MetricsFeed {
    pub fn set_active(
        &mut self,
        active: bool,
        client: Client,
        namespace: Option<String>,
        wake: mpsc::UnboundedSender<AppEvent>,
    ) {
        if !active {
            if let Some(task) = self.task.take() {
                task.abort();
            }
            self.reply = None;
            return;
        }
        if self.task.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel(1);
        self.reply = Some(rx);
        self.task = Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let result = tokio::time::timeout(
                    Duration::from_secs(4),
                    fetch(&client, namespace.as_deref()),
                )
                .await
                .unwrap_or_else(|_| Err("Metrics request timed out".into()));
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
                self.revision += 1;
                changed = true;
                match result {
                    Ok(data) => {
                        self.latest = data;
                        self.error = None;
                    }
                    Err(error) => {
                        self.latest.clear();
                        self.error = Some(error);
                    }
                }
            }
        }
        changed
    }
    pub fn usage(&self, pod: &DynamicObject) -> Option<Usage> {
        if self.latest.is_empty() {
            return None;
        }
        let sample = self
            .latest
            .get(&(pod.namespace().unwrap_or_default(), pod.name_any()))?;
        if (Utc::now() - sample.timestamp).num_seconds() > 90 {
            return None;
        }
        if let Some(created) = &pod.metadata.creation_timestamp {
            let sample_nanos = i128::from(sample.timestamp.timestamp()) * 1_000_000_000
                + i128::from(sample.timestamp.timestamp_subsec_nanos());
            if sample_nanos < created.0.as_nanosecond() {
                return None;
            }
        }
        Some(sample.usage)
    }
}
async fn fetch(client: &Client, namespace: Option<&str>) -> Result<Snapshot, String> {
    let path = namespace
        .map(|ns| format!("/apis/metrics.k8s.io/v1beta1/namespaces/{ns}/pods"))
        .unwrap_or_else(|| "/apis/metrics.k8s.io/v1beta1/pods".into());
    let request = http::Request::get(path)
        .body(Vec::new())
        .map_err(|_| "Invalid metrics request")?;
    let value: serde_json::Value = client.request(request).await.map_err(|error| match error {
        kube::Error::Api(response) => format!(
            "Metrics unavailable (API {}) · needs metrics-server and metrics.k8s.io read access",
            response.code
        ),
        _ => "Metrics unavailable · connection or authentication failed".into(),
    })?;
    parse(&value)
}
fn parse(value: &serde_json::Value) -> Result<Snapshot, String> {
    let items = value["items"]
        .as_array()
        .ok_or("Invalid metrics response")?;
    Ok(items
        .iter()
        .filter_map(|pod| {
            let key = (
                pod["metadata"]["namespace"].as_str()?.into(),
                pod["metadata"]["name"].as_str()?.into(),
            );
            let timestamp = pod["timestamp"].as_str()?.parse().ok()?;
            let containers = pod["containers"].as_array()?;
            if containers.is_empty() {
                return None;
            }
            let mut usage = Usage::default();
            for container in containers {
                usage.cpu_milli += quantity(container["usage"]["cpu"].as_str()?)? * 1000.;
                usage.memory_bytes += quantity(container["usage"]["memory"].as_str()?)?;
            }
            Some((key, PodUsage { usage, timestamp }))
        })
        .collect())
}
pub fn quantity(value: &str) -> Option<f64> {
    // Kubernetes DecimalSI/BinarySI and decimal exponents; reject invalid/nonfinite usage.
    let (number, scale) = [
        ("Ki", 1024f64),
        ("Mi", 1048576.),
        ("Gi", 1073741824.),
        ("Ti", 1099511627776.),
        ("Pi", 1125899906842624.),
        ("Ei", 1152921504606846976.),
        ("n", 1e-9),
        ("u", 1e-6),
        ("m", 1e-3),
        ("k", 1e3),
        ("K", 1e3),
        ("M", 1e6),
        ("G", 1e9),
        ("T", 1e12),
        ("P", 1e15),
        ("E", 1e18),
    ]
    .iter()
    .find_map(|(suffix, scale)| value.strip_suffix(suffix).map(|n| (n, *scale)))
    .unwrap_or((value, 1.));
    let result = number.parse::<f64>().ok()? * scale;
    (result.is_finite() && result >= 0.).then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metrics_units_and_partial_missing_values_are_not_fabricated() {
        assert_eq!(quantity("250m"), Some(0.25));
        assert_eq!(quantity("512Mi"), Some(536870912.));
        assert_eq!(quantity("2e3"), Some(2000.));
        assert_eq!(quantity("NaN"), None);
        assert_eq!(quantity("-1"), None);
        let data = parse(&serde_json::json!({"items":[{"metadata":{"name":"web","namespace":"demo"},"timestamp":"2026-09-28T12:00:00Z","containers":[{"usage":{"cpu":"250000000n","memory":"128Mi"}},{"usage":{"cpu":"50m","memory":"32Mi"}}]},{"metadata":{"name":"missing","namespace":"demo"},"timestamp":"2026-09-28T12:00:00Z","containers":[{"usage":{"cpu":"10m"}}]}]})).unwrap();
        assert_eq!(data.len(), 1);
        let sample = &data[&("demo".into(), "web".into())];
        assert_eq!(sample.usage.cpu_milli, 300.);
        assert_eq!(sample.usage.memory_bytes, 160. * 1048576.);
    }
    #[test]
    fn missing_stale_and_replaced_pod_samples_are_rejected() {
        let mut pod: DynamicObject = serde_json::from_value(serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"namespace":"demo","name":"web","creationTimestamp":(Utc::now()-chrono::Duration::seconds(10)).to_rfc3339()}})).unwrap();
        let mut feed = MetricsFeed::default();
        assert!(feed.usage(&pod).is_none());
        let key = ("demo".into(), "web".into());
        feed.latest.insert(
            key.clone(),
            PodUsage {
                usage: Usage {
                    cpu_milli: 7.,
                    memory_bytes: 1024.,
                },
                timestamp: Utc::now(),
            },
        );
        assert_eq!(feed.usage(&pod).unwrap().cpu_milli, 7.);
        pod.metadata.namespace = Some("other".into());
        assert!(feed.usage(&pod).is_none());
        pod.metadata.namespace = Some("demo".into());
        feed.latest.get_mut(&key).unwrap().timestamp = Utc::now() - chrono::Duration::seconds(20);
        assert!(
            feed.usage(&pod).is_none(),
            "sample predates replacement pod"
        );
        pod.metadata.creation_timestamp = None;
        feed.latest.get_mut(&key).unwrap().timestamp = Utc::now() - chrono::Duration::seconds(100);
        assert!(
            feed.usage(&pod).is_none(),
            "stale usage cannot look current"
        );
    }
    #[tokio::test]
    async fn dropping_feed_cancels_worker_and_errors_clear_old_metrics() {
        let (tx, rx) = mpsc::channel(1);
        let task = tokio::spawn(std::future::pending::<()>());
        let abort = task.abort_handle();
        let mut feed = MetricsFeed {
            task: Some(task),
            reply: Some(rx),
            latest: BTreeMap::new(),
            error: None,
            revision: 0,
        };
        feed.latest.insert(
            ("demo".into(), "web".into()),
            PodUsage {
                usage: Usage::default(),
                timestamp: Utc::now(),
            },
        );
        tx.send(Err("API 403".into())).await.unwrap();
        assert!(feed.drain());
        assert!(feed.latest.is_empty());
        assert_eq!(feed.error.as_deref(), Some("API 403"));
        drop(feed);
        tokio::task::yield_now().await;
        assert!(abort.is_finished());
        assert!(tx.send(Ok(BTreeMap::new())).await.is_err());
    }
}
