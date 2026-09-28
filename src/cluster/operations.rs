//! Mutations require an explicit plan, an exact object identity, and UI confirmation.
use anyhow::{Result, anyhow, bail};
use kube::{
    Api, Client, ResourceExt,
    api::{ApiResource, DeleteParams, DynamicObject, Patch, PatchParams, Preconditions},
};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    Scale(i32),
    Restart,
    Delete,
}
impl Operation {
    pub fn parse(command: &str) -> Result<Self> {
        let mut words = command.split_whitespace();
        let op = match words.next() {
            Some("scale") => Self::Scale(
                words
                    .next()
                    .ok_or_else(|| anyhow!("Use scale NUMBER"))?
                    .parse::<i32>()?,
            ),
            Some("restart") => Self::Restart,
            Some("delete") => Self::Delete,
            _ => bail!("Unknown operation"),
        };
        if words.next().is_some() {
            bail!("Unexpected arguments");
        }
        if matches!(op,Self::Scale(n) if n<0) {
            bail!("Replicas must be zero or greater");
        }
        Ok(op)
    }
    pub fn label(&self) -> String {
        match self {
            Self::Scale(n) => format!("Scale to {n} replicas"),
            Self::Restart => "Restart workload".into(),
            Self::Delete => "Delete resource".into(),
        }
    }
}
pub fn validate(object: &DynamicObject, op: &Operation) -> Result<()> {
    if object.uid().is_none() {
        bail!("Resource has no UID; refresh before changing it");
    }
    let kind = super::related::kind(object);
    match op {
        Operation::Scale(_)
            if !matches!(
                kind,
                "Deployment" | "StatefulSet" | "ReplicaSet" | "ReplicationController"
            ) =>
        {
            bail!("Scaling is supported for Deployments, StatefulSets and ReplicaSets")
        }
        Operation::Restart if !matches!(kind, "Deployment" | "StatefulSet" | "DaemonSet") => {
            bail!("Restart is supported for Deployments, StatefulSets and DaemonSets")
        }
        _ => {}
    }
    Ok(())
}
pub async fn execute(
    client: Client,
    object: DynamicObject,
    resource: ApiResource,
    op: Operation,
) -> Result<String> {
    for attempt in 0..4 {
        let result =
            execute_once(client.clone(), object.clone(), resource.clone(), op.clone()).await;
        let conflict = result
            .as_ref()
            .err()
            .and_then(|e| e.downcast_ref::<kube::Error>())
            .is_some_and(|e| matches!(e,kube::Error::Api(status) if status.code==409));
        if !conflict || attempt == 3 {
            return result;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50 * (attempt + 1))).await;
    }
    unreachable!()
}

async fn execute_once(
    client: Client,
    object: DynamicObject,
    resource: ApiResource,
    op: Operation,
) -> Result<String> {
    validate(&object, &op)?;
    let api: Api<DynamicObject> = match object.namespace() {
        Some(ns) => Api::namespaced_with(client, &ns, &resource),
        None => Api::all_with(client, &resource),
    };
    let current = api.get(&object.name_any()).await?;
    if current.uid() != object.uid() {
        bail!("This resource was replaced. Reopen it before changing it.");
    }
    let metadata =
        serde_json::json!({"resourceVersion":current.resource_version(),"uid":object.uid()});
    match &op {
        Operation::Scale(replicas) => {
            api.patch(
                &object.name_any(),
                &PatchParams::default(),
                &Patch::Merge(
                    serde_json::json!({"metadata":metadata,"spec":{"replicas":replicas}}),
                ),
            )
            .await?;
        }
        Operation::Restart => {
            api.patch(&object.name_any(),&PatchParams::default(),&Patch::Merge(serde_json::json!({"metadata":metadata,"spec":{"template":{"metadata":{"annotations":{"kubectl.kubernetes.io/restartedAt":chrono::Utc::now().to_rfc3339()}}}}}))).await?;
        }
        Operation::Delete => {
            api.delete(
                &object.name_any(),
                &DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: object.uid(),
                        resource_version: current.resource_version(),
                    }),
                    ..Default::default()
                },
            )
            .await?;
        }
    }
    Ok(format!(
        "{} completed for {}",
        op.label(),
        object.name_any()
    ))
}

pub fn forward_ports(value: &str) -> Result<(u16, u16)> {
    let (local, remote) = value
        .split_once(':')
        .ok_or_else(|| anyhow!("Use forward LOCAL:REMOTE"))?;
    let pair = (local.parse()?, remote.parse()?);
    if pair.0 == 0 || pair.1 == 0 {
        bail!("Ports must be 1–65535");
    }
    Ok(pair)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operations_and_ports_are_strict() {
        assert_eq!(Operation::parse("scale 3").unwrap(), Operation::Scale(3));
        for v in [
            "scale -1",
            "scale 3 ignored",
            "delete extra",
            "restart --all",
        ] {
            assert!(Operation::parse(v).is_err());
        }
        assert_eq!(forward_ports("8080:80").unwrap(), (8080, 80));
        for v in ["0:80", "80:99999", "host:80", "80"] {
            assert!(forward_ports(v).is_err());
        }
    }
}
