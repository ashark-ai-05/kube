pub mod events;
pub mod metrics;
pub mod pod;
use kube::api::DynamicObject;
use metrics::{MetricsFeed, Usage};
use pod::{Budget, Filter, Identity, Sort};
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};

pub struct Row {
    pub object: Arc<DynamicObject>,
    pub usage: Option<Usage>,
    pub budget: Budget,
    pub restarts: u64,
    pub age: Option<u64>,
}

#[derive(PartialEq, Eq)]
struct ViewKey {
    filter: Filter,
    search: String,
    sort: Sort,
    descending: bool,
    revision: u64,
}

pub struct Dashboard {
    pub metrics: MetricsFeed,
    pub events: events::Events,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub anchor: Option<Identity>,
    pub filter: Filter,
    pub search: String,
    pub sort: Sort,
    pub descending: bool,
    pub containers: bool,
    pub details: bool,
    pub history: VecDeque<Option<Usage>>,
    sampled_revision: u64,
    history_key: Option<Identity>,
    pub list_height: usize,
    source: Vec<Arc<DynamicObject>>,
    view_key: Option<ViewKey>,
    refreshed: Option<Instant>,
    counts: [usize; 4],
}
impl Default for Dashboard {
    fn default() -> Self {
        Self {
            metrics: MetricsFeed::default(),
            events: events::Events::default(),
            rows: vec![],
            selected: 0,
            anchor: None,
            filter: Filter::All,
            search: String::new(),
            sort: Sort::Name,
            descending: false,
            containers: false,
            details: true,
            history: VecDeque::new(),
            sampled_revision: 0,
            history_key: None,
            list_height: 10,
            source: vec![],
            view_key: None,
            refreshed: None,
            counts: [0; 4],
        }
    }
}
impl Dashboard {
    pub fn reset_scope(&mut self) {
        *self = Self::default();
    }
    pub fn selected_pod(&self) -> Option<&Arc<DynamicObject>> {
        self.rows.get(self.selected).map(|r| &r.object)
    }
    pub fn move_selection(&mut self, delta: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.rows.len().saturating_sub(1));
        self.anchor = self.selected_pod().map(|o| Identity::of(o));
    }
    pub fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.selected = 0;
        self.anchor = None;
    }
    pub fn set_sort(&mut self, sort: Sort, descending: bool) {
        self.sort = sort;
        self.descending = descending;
    }
    pub fn refresh(&mut self, objects: &[Arc<DynamicObject>]) -> [usize; 4] {
        let key = ViewKey {
            filter: self.filter,
            search: self.search.clone(),
            sort: self.sort,
            descending: self.descending,
            revision: self.metrics.revision,
        };
        // Watches replace immutable Arc objects. Key navigation can reuse derived
        // facts while updates, scope, filters, metrics and age ticks invalidate them.
        if self.view_key.as_ref() == Some(&key)
            && self
                .refreshed
                .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
            && self.source.len() == objects.len()
            && self
                .source
                .iter()
                .zip(objects)
                .all(|(a, b)| Arc::ptr_eq(a, b))
        {
            self.sample_selection();
            return self.counts;
        }
        self.source = objects.to_vec();
        self.view_key = Some(key);
        self.refreshed = Some(Instant::now());
        let mut counts = [0; 4];
        let filters = [
            Filter::All,
            Filter::NotReady,
            Filter::Restarts,
            Filter::HighMemory,
        ];
        self.rows = objects
            .iter()
            .filter_map(|object| {
                let usage = self.metrics.usage(object);
                let budget = Budget::of(object);
                for (i, filter) in filters.iter().enumerate() {
                    counts[i] += usize::from(filter.matches(object, usage, budget));
                }
                if !self.filter.matches(object, usage, budget)
                    || !crate::ui::command::matches_resource(&self.search, object)
                {
                    return None;
                }
                Some(Row {
                    object: object.clone(),
                    usage,
                    budget,
                    restarts: pod::restarts(object),
                    age: pod::age_seconds(object),
                })
            })
            .collect();
        let sort = self.sort;
        let descending = self.descending;
        self.rows.sort_by(|a, b| {
            let numeric = |r: &Row| match sort {
                Sort::Ready => Some(f64::from(u8::from(
                    crate::ui::workspace::health(&r.object) == crate::ui::workspace::Health::Ready,
                ))),
                Sort::Restarts => Some(r.restarts as f64),
                Sort::Age => r.age.map(|n| n as f64),
                Sort::Cpu => r.usage.map(|u| u.cpu_milli),
                Sort::Memory => r.usage.map(|u| u.memory_bytes),
                Sort::Name => None,
            };
            let order = match (numeric(a), numeric(b)) {
                (Some(x), Some(y)) => x.total_cmp(&y),
                (Some(_), None) => return std::cmp::Ordering::Less,
                (None, Some(_)) => return std::cmp::Ordering::Greater,
                _ => a.object.metadata.name.cmp(&b.object.metadata.name),
            };
            let order = if descending { order.reverse() } else { order };
            order
                .then_with(|| {
                    a.object
                        .metadata
                        .namespace
                        .cmp(&b.object.metadata.namespace)
                })
                .then_with(|| a.object.metadata.name.cmp(&b.object.metadata.name))
        });
        self.selected = self
            .anchor
            .as_ref()
            .and_then(|key| {
                self.rows
                    .iter()
                    .position(|r| Identity::of(&r.object) == *key)
            })
            .unwrap_or(self.selected)
            .min(self.rows.len().saturating_sub(1));
        self.anchor = self.selected_pod().map(|o| Identity::of(o));
        self.counts = counts;
        self.sample_selection();
        counts
    }
    fn sample_selection(&mut self) {
        if self.history_key != self.anchor {
            self.history.clear();
            self.history_key = self.anchor.clone();
            self.sampled_revision = u64::MAX;
        }
        if self.sampled_revision != self.metrics.revision {
            self.history
                .push_back(self.rows.get(self.selected).and_then(|r| r.usage));
            while self.history.len() > 60 {
                self.history.pop_front();
            }
            self.sampled_revision = self.metrics.revision;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use metrics::PodUsage;
    use std::collections::BTreeMap;

    fn pod(name: &str, ns: &str, uid: &str, restarts: u64, ready: bool) -> Arc<DynamicObject> {
        Arc::new(serde_json::from_value(serde_json::json!({
            "apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":ns,"uid":uid},
            "spec":{"containers":[{"name":"app","resources":{"limits":{"memory":"100Mi"}}}]},
            "status":{"phase":"Running","conditions":[{"type":"Ready","status":if ready{"True"}else{"False"}}],"containerStatuses":[{"name":"app","ready":ready,"restartCount":restarts}]}
        })).unwrap())
    }

    #[test]
    fn numeric_sort_missing_metrics_last_and_identity_survives_reordering() {
        let pods = vec![
            pod("same", "west", "w", 12, true),
            pod("same", "east", "e", 2, false),
            pod("missing", "east", "m", 0, true),
        ];
        let mut dashboard = Dashboard::default();
        for (p, cpu) in [(&pods[0], 200.), (&pods[1], 30.)] {
            dashboard.metrics.latest.insert(
                (
                    p.metadata.namespace.clone().unwrap(),
                    p.metadata.name.clone().unwrap(),
                ),
                PodUsage {
                    usage: Usage {
                        cpu_milli: cpu,
                        memory_bytes: 90. * 1048576.,
                    },
                    timestamp: Utc::now(),
                    containers: BTreeMap::new(),
                },
            );
        }
        dashboard.set_sort(Sort::Restarts, true);
        assert_eq!(dashboard.refresh(&pods), [3, 1, 2, 2]);
        assert_eq!(
            dashboard.selected_pod().unwrap().metadata.uid.as_deref(),
            Some("w")
        );
        dashboard.set_sort(Sort::Cpu, false);
        dashboard.refresh(&pods);
        assert_eq!(dashboard.rows[0].object.metadata.uid.as_deref(), Some("e"));
        assert_eq!(dashboard.rows[2].object.metadata.uid.as_deref(), Some("m"));
        assert_eq!(dashboard.selected, 1);
        dashboard.set_sort(Sort::Cpu, true);
        dashboard.refresh(&pods);
        assert_eq!(dashboard.rows[2].object.metadata.uid.as_deref(), Some("m"));
        dashboard.set_filter(Filter::NotReady);
        dashboard.refresh(&pods);
        assert_eq!(dashboard.rows.len(), 1);
        assert_eq!(
            dashboard.selected_pod().unwrap().metadata.uid.as_deref(),
            Some("e")
        );
    }

    #[test]
    fn history_is_bound_to_pod_uid_and_scope_not_row_number() {
        let mut dashboard = Dashboard::default();
        dashboard.refresh(&[pod("web", "demo", "old", 0, true)]);
        dashboard.history.push_back(Some(Usage {
            cpu_milli: 99.,
            memory_bytes: 100.,
        }));
        dashboard.refresh(&[pod("web", "demo", "new", 0, true)]);
        assert_eq!(dashboard.history.len(), 1);
        assert!(dashboard.history[0].is_none());
        dashboard.reset_scope();
        assert!(
            dashboard.rows.is_empty() && dashboard.history.is_empty() && dashboard.anchor.is_none()
        );
    }
    #[test]
    fn cached_navigation_invalidates_on_watch_filters_and_new_metrics() {
        let mut dashboard = Dashboard::default();
        let mut pods = vec![
            pod("a", "demo", "a", 0, true),
            pod("b", "demo", "b", 0, true),
        ];
        dashboard.refresh(&pods);
        let first = dashboard.refreshed;
        dashboard.move_selection(1);
        dashboard.refresh(&pods);
        assert_eq!(first, dashboard.refreshed);
        assert_eq!(
            dashboard.history_key.as_ref().unwrap().uid.as_deref(),
            Some("b")
        );
        pods[0] = pod("a", "demo", "a", 7, false);
        assert_eq!(dashboard.refresh(&pods), [2, 1, 1, 0]);
        assert_eq!(dashboard.rows[0].restarts, 7);
        dashboard.metrics.latest.insert(
            ("demo".into(), "b".into()),
            PodUsage {
                usage: Usage {
                    cpu_milli: 123.,
                    memory_bytes: 90. * 1048576.,
                },
                timestamp: Utc::now(),
                containers: BTreeMap::new(),
            },
        );
        dashboard.metrics.revision += 1;
        assert_eq!(dashboard.refresh(&pods), [2, 1, 1, 1]);
        assert_eq!(dashboard.rows[1].usage.unwrap().cpu_milli, 123.);
        dashboard.set_filter(Filter::Restarts);
        dashboard.refresh(&pods);
        assert_eq!(dashboard.rows.len(), 1);
        dashboard.refreshed = Some(Instant::now() - Duration::from_secs(2));
        dashboard.refresh(&pods);
        assert!(dashboard.refreshed.unwrap().elapsed() < Duration::from_secs(1));
    }
}
