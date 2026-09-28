use crate::ui::theme;
use crate::{
    app::event::AppEvent,
    cluster::{
        self,
        discovery::KindInfo,
        operations::{self, Operation},
        related,
    },
};
use crossterm::event::{Event, KeyCode, KeyEventKind};
use kube::{
    Client, ResourceExt,
    api::{ApiResource, DynamicObject},
};
use ratatui::{
    Frame,
    style::Style,
    widgets::{Block, Borders, Clear, Paragraph},
};
use tokio::{sync::mpsc, task::JoinHandle};

struct Plan {
    object: DynamicObject,
    resource: ApiResource,
    client: Client,
    context: String,
    operation: Operation,
}
pub struct Operations {
    pub writable: bool,
    pub notice: String,
    pending: Option<Plan>,
    input: String,
    tx: mpsc::Sender<(Option<u64>, String)>,
    rx: mpsc::Receiver<(Option<u64>, String)>,
    forward_generation: u64,
    wake: mpsc::UnboundedSender<AppEvent>,
    task: Option<JoinHandle<()>>,
    forwards: Vec<tokio::process::Child>,
}
impl Drop for Operations {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl Operations {
    pub fn new(wake: mpsc::UnboundedSender<AppEvent>) -> Self {
        let (tx, rx) = mpsc::channel(8);
        Self {
            writable: false,
            notice: String::new(),
            pending: None,
            input: String::new(),
            tx,
            rx,
            wake,
            task: None,
            forwards: vec![],
            forward_generation: 0,
        }
    }
    pub fn confirming(&self) -> bool {
        self.pending.is_some()
    }
    pub fn reset_context(&mut self) {
        if !self.forwards.is_empty() {
            self.stop_forwards();
        }
        self.pending = None;
        self.writable = false;
    }
    pub fn prepare(
        &mut self,
        command: &str,
        object: Option<DynamicObject>,
        client: Client,
        kinds: &[KindInfo],
        context: &str,
    ) {
        let result = (|| -> anyhow::Result<Plan> {
            if !self.writable {
                anyhow::bail!("Read-only mode. Use :write to enable confirmations.");
            }
            if self.task.as_ref().is_some_and(|t| !t.is_finished()) {
                anyhow::bail!("An operation is already running");
            }
            let object = object.ok_or_else(|| anyhow::anyhow!("Select a resource first"))?;
            let operation = Operation::parse(command)?;
            operations::validate(&object, &operation)?;
            let resource = related::resource_for(&object, kinds)?;
            Ok(Plan {
                object,
                resource,
                client,
                context: context.into(),
                operation,
            })
        })();
        match result {
            Ok(plan) => {
                self.pending = Some(plan);
                self.input.clear();
            }
            Err(e) => self.notice = cluster::safe_error_text(&e),
        }
    }
    pub fn handle(&mut self, event: &Event) {
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Esc => self.pending = None,
                KeyCode::Char(c) => self.input.push(c),
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Enter => {
                    if self
                        .pending
                        .as_ref()
                        .is_some_and(|p| self.input == p.object.name_any())
                    {
                        let plan = self.pending.take().unwrap();
                        let tx = self.tx.clone();
                        let wake = self.wake.clone();
                        self.notice = format!("{} on {}…", plan.operation.label(), plan.context);
                        self.task = Some(tokio::spawn(async move {
                            let outcome = operations::execute(
                                plan.client,
                                plan.object,
                                plan.resource,
                                plan.operation,
                            )
                            .await;
                            let message = match outcome {
                                Ok(text) => format!("{}: {text}", plan.context),
                                Err(e) => {
                                    format!("{}: {}", plan.context, cluster::safe_error_text(&e))
                                }
                            };
                            let _ = tx.send((None, message)).await;
                            let _ = wake.send(AppEvent::Wake);
                        }));
                    } else {
                        self.notice = "Type the exact resource name to confirm".into();
                    }
                }
                _ => {}
            }
        }
    }
    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        while let Ok((generation, text)) = self.rx.try_recv() {
            if generation.is_some_and(|g| g != self.forward_generation) {
                continue;
            }
            self.notice = text;
            changed = true;
        }
        self.forwards.retain_mut(|child| match child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                self.notice = format!("Port-forward exited: {status}");
                changed = true;
                false
            }
            Err(_) => false,
        });
        changed
    }
    pub fn render(&self, f: &mut Frame) {
        if let Some(plan) = &self.pending {
            let area = crate::ui::views::picker::centered(f.area(), 70, 40);
            f.render_widget(Clear, area);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::AMBER))
                .title(" Confirm operation ");
            let inner = block.inner(area);
            f.render_widget(block, area);
            f.render_widget(Paragraph::new(format!("{}\n\nCluster: {}\nResource: {} {}/{}\n\nType {} to confirm: {}█\nEnter confirm · Esc cancel",plan.operation.label(),plan.context,related::kind(&plan.object),plan.object.namespace().unwrap_or_else(||"cluster".into()),plan.object.name_any(),plan.object.name_any(),self.input)).style(theme::text_style()),inner);
        }
    }
    pub fn stop_forwards(&mut self) {
        self.forward_generation = self.forward_generation.wrapping_add(1);
        self.forwards.clear();
        self.notice = "Port forwards stopped".into();
    }
    pub fn forward(
        &mut self,
        object: &DynamicObject,
        context: &str,
        ports: &str,
    ) -> anyhow::Result<()> {
        if related::kind(object) != "Pod" {
            anyhow::bail!("Select a Pod (Related → Pod) to forward a port");
        }
        let (local, remote) = operations::forward_ports(ports)?;
        let ns = object
            .namespace()
            .ok_or_else(|| anyhow::anyhow!("Pod namespace is missing"))?;
        let mut command = tokio::process::Command::new("kubectl");
        command.args([
            "--context",
            context,
            "-n",
            &ns,
            "port-forward",
            "--address",
            "127.0.0.1",
            &format!("pod/{}", object.name_any()),
            &format!("{local}:{remote}"),
        ]);
        command
            .kill_on_drop(true)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn()?;
        use tokio::io::AsyncBufReadExt;
        let generation = self.forward_generation;
        if let Some(stdout) = child.stdout.take() {
            let tx = self.tx.clone();
            let wake = self.wake.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if tx.send((Some(generation), line)).await.is_err() {
                        break;
                    }
                    let _ = wake.send(AppEvent::Wake);
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let tx = self.tx.clone();
            let wake = self.wake.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if tx
                        .send((Some(generation), format!("Port-forward: {line}")))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    let _ = wake.send(AppEvent::Wake);
                }
            });
        }
        self.forwards.push(child);
        self.notice = format!(
            "Starting localhost:{local} → {}/{remote}",
            object.name_any()
        );
        Ok(())
    }
}

pub fn exec_command(
    object: &DynamicObject,
    context: &str,
    container: Option<&str>,
) -> anyhow::Result<tokio::process::Command> {
    if related::kind(object) != "Pod" {
        anyhow::bail!("Select a Pod (Related → Pod) to open a shell");
    }
    let ns = object
        .namespace()
        .ok_or_else(|| anyhow::anyhow!("Pod namespace is missing"))?;
    let containers = object
        .data
        .pointer("/spec/containers")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("Pod containers are unavailable"))?;
    if container.is_none() && containers.len() > 1 {
        anyhow::bail!("Choose a container with :exec CONTAINER");
    }
    let container = container
        .or_else(|| containers.first().and_then(|v| v["name"].as_str()))
        .unwrap_or("");
    if !containers
        .iter()
        .any(|v| v["name"].as_str() == Some(container))
    {
        anyhow::bail!("Unknown container {container}");
    }
    let mut command = tokio::process::Command::new("kubectl");
    command
        .args([
            "--context",
            context,
            "-n",
            &ns,
            "exec",
            "-it",
            &object.name_any(),
            "-c",
            container,
            "--",
            "sh",
        ])
        .kill_on_drop(true);
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stopped_forward_cannot_overwrite_status_with_queued_output() {
        let (wake, _) = mpsc::unbounded_channel();
        let mut ops = Operations::new(wake);
        ops.tx
            .send((Some(0), "Handling connection for 8080".into()))
            .await
            .unwrap();
        ops.stop_forwards();
        ops.drain();
        assert_eq!(ops.notice, "Port forwards stopped");
    }
    #[tokio::test]
    async fn read_only_and_exact_name_confirmation_gate_mutations() {
        let client =
            Client::try_from(kube::Config::new("http://127.0.0.1:9".parse().unwrap())).unwrap();
        let object:DynamicObject=serde_json::from_value(serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"important","namespace":"demo","uid":"original"}})).unwrap();
        let resource = ApiResource {
            group: "".into(),
            version: "v1".into(),
            api_version: "v1".into(),
            kind: "Pod".into(),
            plural: "pods".into(),
        };
        let kinds = vec![KindInfo {
            resource,
            gvk: related::gvk(&object),
            namespaced: true,
            group_label: "core".into(),
        }];
        let (wake, _rx) = mpsc::unbounded_channel();
        let mut ops = Operations::new(wake);
        ops.prepare(
            "delete",
            Some(object.clone()),
            client.clone(),
            &kinds,
            "acceptance",
        );
        assert!(!ops.confirming());
        ops.writable = true;
        ops.prepare("delete", Some(object), client, &kinds, "acceptance");
        assert!(ops.confirming());
        ops.input = "wrong-name".into();
        ops.handle(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        )));
        assert!(ops.task.is_none());
        assert!(ops.confirming());
        ops.reset_context();
        assert!(!ops.writable);
        assert!(!ops.confirming());
    }
}
