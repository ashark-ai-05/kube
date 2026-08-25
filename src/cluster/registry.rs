use crate::cluster::config::ContextInfo;

/// A cluster's identity: its kubeconfig context name, which is unique per file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClusterId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Failed { reason: String },
}

#[derive(Debug, Clone)]
pub struct ClusterEntry {
    pub id: ClusterId,
    pub context: ContextInfo,
    pub state: ConnectionState,
}

/// Every cluster the kubeconfig knows about, plus which one we are using.
///
/// Construction parses kubeconfig only — no network. With 20+ clusters,
/// connecting eagerly would open 20 authenticated sessions, some of which
/// will hang against an endpoint the current VPN cannot reach.
#[derive(Debug, Clone, Default)]
pub struct ClusterRegistry {
    entries: Vec<ClusterEntry>,
    active: Option<ClusterId>,
}

impl ClusterRegistry {
    pub fn from_contexts(contexts: Vec<ContextInfo>) -> Self {
        let active = contexts
            .iter()
            .find(|c| c.is_current)
            .map(|c| ClusterId(c.name.clone()));
        let entries = contexts
            .into_iter()
            .map(|context| ClusterEntry {
                id: ClusterId(context.name.clone()),
                context,
                state: ConnectionState::Disconnected,
            })
            .collect();
        Self { entries, active }
    }

    pub fn entries(&self) -> &[ClusterEntry] {
        &self.entries
    }

    pub fn find(&self, id: &ClusterId) -> Option<&ClusterEntry> {
        self.entries.iter().find(|e| &e.id == id)
    }

    pub fn active(&self) -> Option<&ClusterEntry> {
        self.active.as_ref().and_then(|id| self.find(id))
    }

    pub fn set_state(&mut self, id: &ClusterId, state: ConnectionState) {
        if let Some(e) = self.entries.iter_mut().find(|e| &e.id == id) {
            e.state = state;
        }
    }

    /// Returns false if the cluster is unknown, leaving `active` unchanged.
    pub fn set_active(&mut self, id: &ClusterId) -> bool {
        if self.entries.iter().any(|e| &e.id == id) {
            self.active = Some(id.clone());
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::auth::AuthMethod;

    fn ctx(name: &str, current: bool) -> ContextInfo {
        ContextInfo {
            name: name.to_string(),
            cluster: format!("{name}-cluster"),
            namespace: None,
            is_current: current,
            auth: AuthMethod::None,
        }
    }

    fn registry() -> ClusterRegistry {
        ClusterRegistry::from_contexts(vec![
            ctx("prod", false),
            ctx("staging", true),
            ctx("dev", false),
        ])
    }

    #[test]
    fn every_context_becomes_an_entry() {
        assert_eq!(registry().entries().len(), 3);
    }

    #[test]
    fn entries_start_disconnected_because_listing_touches_no_network() {
        for e in registry().entries() {
            assert_eq!(
                e.state,
                ConnectionState::Disconnected,
                "{} was not disconnected",
                e.id.0
            );
        }
    }

    #[test]
    fn the_current_context_becomes_active_on_construction() {
        let r = registry();
        assert_eq!(r.active().map(|e| e.id.0.as_str()), Some("staging"));
    }

    #[test]
    fn with_no_current_context_nothing_is_active() {
        let r = ClusterRegistry::from_contexts(vec![ctx("a", false), ctx("b", false)]);
        assert!(r.active().is_none(), "must not guess an active cluster");
    }

    #[test]
    fn state_is_tracked_per_cluster() {
        let mut r = registry();
        r.set_state(&ClusterId("prod".into()), ConnectionState::Connected);
        assert_eq!(
            r.find(&ClusterId("prod".into())).unwrap().state,
            ConnectionState::Connected
        );
        assert_eq!(
            r.find(&ClusterId("dev".into())).unwrap().state,
            ConnectionState::Disconnected,
            "one cluster's state must not leak into another"
        );
    }

    #[test]
    fn a_failed_cluster_keeps_its_reason() {
        let mut r = registry();
        let id = ClusterId("prod".into());
        r.set_state(
            &id,
            ConnectionState::Failed {
                reason: "no route to host".into(),
            },
        );
        match &r.find(&id).unwrap().state {
            ConnectionState::Failed { reason } => assert_eq!(reason, "no route to host"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn setting_an_unknown_cluster_active_is_rejected_not_panicked() {
        let mut r = registry();
        assert!(!r.set_active(&ClusterId("nope".into())));
        assert_eq!(
            r.active().map(|e| e.id.0.as_str()),
            Some("staging"),
            "active must be unchanged"
        );
    }

    #[test]
    fn setting_state_on_an_unknown_cluster_is_a_no_op() {
        let mut r = registry();
        r.set_state(&ClusterId("nope".into()), ConnectionState::Connected);
        assert_eq!(r.entries().len(), 3, "must not invent an entry");
    }
}
