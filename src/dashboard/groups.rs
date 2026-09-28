use anyhow::{Context, bail};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::{
    ResourceExt,
    api::DynamicObject,
    core::{Selector, SelectorExt},
};
use regex::Regex;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GroupKey {
    pub namespace: String,
    pub name: String,
    pub origin: String,
}
pub struct Rule {
    name: String,
    namespace: Option<String>,
    selector: Selector,
    prefix: Option<String>,
    regex: Option<Regex>,
}
#[derive(Default)]
pub struct GroupConfig {
    rules: Vec<Rule>,
}
impl GroupConfig {
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let fallback = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
            })
            .join("kube/groups.yaml");
        let Some(path) = path.or_else(|| fallback.exists().then_some(fallback.as_path())) else {
            return Ok(Self::default());
        };
        let mut text = String::new();
        std::fs::File::open(path)
            .with_context(|| format!("opening grouping file {}", path.display()))?
            .take(262145)
            .read_to_string(&mut text)?;
        if text.len() > 262144 {
            bail!("grouping file exceeds 256 KiB");
        }
        Self::parse(&text).with_context(|| format!("invalid grouping file {}", path.display()))
    }
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let value: Value = serde_norway::from_str(text)?;
        keys(&value, &["version", "groups"])?;
        if value.get("version").and_then(Value::as_u64) != Some(1) {
            bail!("grouping version must be 1");
        }
        let entries = value["groups"]
            .as_array()
            .context("groups must be a list")?;
        if entries.len() > 128 {
            bail!("at most 128 group rules are supported");
        }
        let mut names = BTreeSet::new();
        let mut rules = vec![];
        for entry in entries {
            keys(
                entry,
                &["name", "namespace", "selector", "namePrefix", "nameRegex"],
            )?;
            let name = required_string(entry, "name")?;
            if name.len() > 80 || name.chars().any(char::is_control) || !names.insert(name.clone())
            {
                bail!("group names must be unique, printable and at most 80 bytes");
            }
            let namespace = optional_string(entry, "namespace")?;
            if namespace
                .as_ref()
                .is_some_and(|n| !crate::cluster::is_valid_namespace_name(n))
            {
                bail!("invalid group namespace");
            }
            let selector: Selector = if let Some(raw) = entry.get("selector") {
                keys(raw, &["matchLabels", "matchExpressions"])?;
                serde_json::from_value::<LabelSelector>(raw.clone())?.try_into()?
            } else {
                Selector::default()
            };
            let prefix = optional_string(entry, "namePrefix")?;
            let regex = optional_string(entry, "nameRegex")?
                .map(|r| Regex::new(&r))
                .transpose()?;
            if selector.selects_all() && prefix.is_none() && regex.is_none() && namespace.is_none()
            {
                bail!("group {name} needs a namespace, selector or name rule");
            }
            rules.push(Rule {
                name,
                namespace,
                selector,
                prefix,
                regex,
            });
        }
        Ok(Self { rules })
    }
    pub fn has_rules(&self) -> bool {
        !self.rules.is_empty()
    }
    pub fn key(&self, pod: &DynamicObject) -> GroupKey {
        let namespace = pod.namespace().unwrap_or_default();
        let name = pod.metadata.name.as_deref().unwrap_or("");
        for rule in &self.rules {
            if rule.namespace.as_ref().is_none_or(|n| *n == namespace)
                && rule.selector.matches(pod.labels())
                && rule.prefix.as_ref().is_none_or(|p| name.starts_with(p))
                && rule.regex.as_ref().is_none_or(|r| r.is_match(name))
            {
                return GroupKey {
                    namespace,
                    name: rule.name.clone(),
                    origin: "config".into(),
                };
            }
        }
        for label in ["app.kubernetes.io/name", "app"] {
            if let Some(name) = pod.labels().get(label).filter(|s| !s.is_empty()) {
                return GroupKey {
                    namespace,
                    name: name.clone(),
                    origin: "app label".into(),
                };
            }
        }
        if let Some(owner) = pod.metadata.owner_references.as_ref().and_then(|owners| {
            owners
                .iter()
                .find(|o| o.controller == Some(true))
                .or_else(|| owners.first())
        }) {
            return GroupKey {
                namespace,
                name: owner.name.clone(),
                origin: owner.kind.clone(),
            };
        }
        GroupKey {
            namespace,
            name: "Ungrouped".into(),
            origin: "no app label or owner".into(),
        }
    }
}
fn keys(value: &Value, allowed: &[&str]) -> anyhow::Result<()> {
    let map = value.as_object().context("expected a mapping")?;
    for key in map.keys() {
        if !allowed.contains(&key.as_str()) {
            bail!("unknown grouping field: {key}");
        }
    }
    Ok(())
}
fn required_string(value: &Value, key: &str) -> anyhow::Result<String> {
    optional_string(value, key)?.with_context(|| format!("missing {key}"))
}
fn optional_string(value: &Value, key: &str) -> anyhow::Result<Option<String>> {
    value
        .get(key)
        .map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .with_context(|| format!("{key} must be a nonempty string"))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pod() -> DynamicObject {
        serde_json::from_value(serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"ingest-1","namespace":"demo","labels":{"app":"ingest","tier":"worker"},"ownerReferences":[{"apiVersion":"apps/v1","kind":"ReplicaSet","name":"ingest-rs","uid":"owner","controller":true}]}})).unwrap()
    }
    #[test]
    fn app_labels_then_owner_and_namespace_are_stable() {
        let config = GroupConfig::default();
        let mut pod = pod();
        assert_eq!(config.key(&pod).name, "ingest");
        let key = config.key(&pod);
        pod.metadata
            .labels
            .as_mut()
            .unwrap()
            .insert("app.kubernetes.io/name".into(), "ingest".into());
        assert_eq!(config.key(&pod), key);
        pod.metadata.namespace = Some("other".into());
        assert_ne!(config.key(&pod), key);
        pod.metadata.labels = None;
        assert_eq!(config.key(&pod).name, "ingest-rs");
        pod.metadata.owner_references = None;
        assert_eq!(config.key(&pod).name, "Ungrouped");
    }
    #[test]
    fn ordered_custom_rules_support_set_selectors_and_name_patterns() {
        let config = GroupConfig::parse("version: 1\ngroups:\n- name: Pipeline\n  namespace: demo\n  selector:\n    matchExpressions:\n    - key: tier\n      operator: In\n      values: [worker, batch]\n  nameRegex: '^ingest-'\n").unwrap();
        let mut pod = pod();
        assert_eq!(config.key(&pod).name, "Pipeline");
        pod.metadata.name = Some("other".into());
        assert_eq!(config.key(&pod).name, "ingest");
        for invalid in [
            "version: 1\ngroups: [{name: All}]",
            "version: 1\ngroups: [{name: Bad, nameRegex: '['}]",
            "version: 1\ngroups: [{name: Bad, typo: true}]",
            "version: 2\ngroups: []",
        ] {
            assert!(GroupConfig::parse(invalid).is_err());
        }
    }
}
