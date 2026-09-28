pub mod groups;
pub mod metrics;
use groups::{GroupConfig, GroupKey};
use metrics::{MetricsFeed, Usage};
use std::collections::{BTreeSet, VecDeque};

pub struct GroupSummary {
    pub key: GroupKey,
    pub pods: usize,
    pub ready: usize,
    pub issues: usize,
    pub restarts: u64,
    pub nodes: BTreeSet<String>,
    pub usage: Usage,
    pub measured: usize,
}
impl GroupSummary {
    pub fn new(key: GroupKey) -> Self {
        Self {
            key,
            pods: 0,
            ready: 0,
            issues: 0,
            restarts: 0,
            nodes: BTreeSet::new(),
            usage: Usage::default(),
            measured: 0,
        }
    }
}
#[derive(Default)]
pub struct Dashboard {
    pub config: GroupConfig,
    pub metrics: MetricsFeed,
    pub groups: Vec<GroupSummary>,
    pub selected: usize,
    pub issues_focus: bool,
    pub active_group: Option<GroupKey>,
    pub history: VecDeque<Option<Usage>>,
    pub sampled_revision: u64,
}
impl Dashboard {
    pub fn reset_scope(&mut self) {
        self.metrics = MetricsFeed::default();
        self.groups.clear();
        self.history.clear();
        self.sampled_revision = 0;
        self.active_group = None;
        self.selected = 0;
    }
}
