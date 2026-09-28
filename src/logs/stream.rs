use crate::{
    app::event::AppEvent,
    cluster::{self, related::pods_for},
    logs::{Decoder, LogLine},
};
use futures::AsyncReadExt;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api, Client, ResourceExt,
    api::{DynamicObject, LogParams},
};
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::mpsc,
    task::{JoinHandle, JoinSet},
};

#[derive(Clone, Default)]
pub struct Options {
    pub container: Option<String>,
    pub previous: bool,
    pub since_seconds: Option<i64>,
}
#[derive(Debug)]
pub enum Message {
    Line(LogLine),
    Status(String),
    Containers(Vec<String>),
}

#[derive(Clone)]
struct Sender {
    tx: mpsc::Sender<Message>,
    wake: mpsc::UnboundedSender<AppEvent>,
    pending: Arc<AtomicBool>,
}
impl Sender {
    async fn send(&self, message: Message) -> bool {
        if self.tx.send(message).await.is_err() {
            return false;
        }
        if !self.pending.swap(true, Ordering::AcqRel) {
            let _ = self.wake.send(AppEvent::Wake);
        }
        true
    }
}

pub struct StreamSession {
    rx: mpsc::Receiver<Message>,
    pending: Arc<AtomicBool>,
    wake: mpsc::UnboundedSender<AppEvent>,
    task: JoinHandle<()>,
}
impl Drop for StreamSession {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl StreamSession {
    pub fn start(
        client: Client,
        object: DynamicObject,
        options: Options,
        wake: mpsc::UnboundedSender<AppEvent>,
    ) -> Self {
        let (tx, rx) = mpsc::channel(256);
        let pending = Arc::new(AtomicBool::new(false));
        let sender = Sender {
            tx,
            pending: pending.clone(),
            wake: wake.clone(),
        };
        let task = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            let mut sources = HashSet::new();
            let mut containers = HashSet::new();
            let mut tick = tokio::time::interval(Duration::from_secs(3));
            loop {
                tokio::select! {
                    _=tick.tick()=> {
                        match pods_for(client.clone(), &object).await {
                            Ok(pods)=> {
                                let live_uids: HashSet<_> = pods.iter().filter_map(|p|p.uid()).collect();
                                sources.retain(|(uid,_,_)| live_uids.contains(uid));
                                containers.clear();
                                if pods.is_empty() { sender.send(Message::Status("Waiting for matching pods…".into())).await; }
                                for pod in pods {
                                    let ns=pod.namespace().unwrap_or_default();let name=pod.name_any();let uid=pod.uid().unwrap_or_default();
                                    let states=pod.status.as_ref().map(|s| s.container_statuses.iter().flatten().chain(s.init_container_statuses.iter().flatten()).map(|s| (s.name.clone(),s.restart_count)).collect::<std::collections::HashMap<_,_>>()).unwrap_or_default();
                                    let Some(spec)=pod.spec else { continue; };
                                    let entries=spec.containers.into_iter().chain(spec.init_containers.unwrap_or_default());
                                    for container in entries {
                                        containers.insert(container.name.clone());
                                        if options.container.as_ref().is_some_and(|c| c!=&container.name) { continue; }
                                        let key=(uid.clone(),container.name.clone(),states.get(&container.name).copied().unwrap_or(0));
                                        if sources.contains(&key) { continue; }
                                        if tasks.len()>=16 { sender.send(Message::Status("16 concurrent streams; select a container to narrow the view".into())).await;break; }
                                        sources.insert(key.clone());
                                        let sender=sender.clone();let client=client.clone();let options=options.clone();let ns=ns.clone();let name=name.clone();
                                        tasks.spawn(async move { stream_one(client,&ns,&name,&container.name,&key.0,options,sender).await; key });
                                    }
                                }
                                let mut names:Vec<_>=containers.iter().cloned().collect();names.sort();sender.send(Message::Containers(names)).await;
                            }
                            Err(e)=> { sender.send(Message::Status(cluster::safe_error_text(&e))).await; }
                        }
                    }
                    Some(result)=tasks.join_next(), if !tasks.is_empty()=> {
                        if let Err(e)=result { sender.send(Message::Status(format!("Log task failed: {e}"))).await; }
                    }
                }
            }
        });
        Self {
            rx,
            pending,
            wake,
            task,
        }
    }
    pub fn drain(&mut self) -> Vec<Message> {
        self.pending.store(false, Ordering::Release);
        let mut out = Vec::new();
        for _ in 0..256 {
            match self.rx.try_recv() {
                Ok(m) => out.push(m),
                Err(_) => break,
            }
        }
        if !self.rx.is_empty() && !self.pending.swap(true, Ordering::AcqRel) {
            let _ = self.wake.send(AppEvent::Wake);
        }
        out
    }
}

async fn stream_one(
    client: Client,
    ns: &str,
    pod: &str,
    container: &str,
    expected_uid: &str,
    options: Options,
    sender: Sender,
) {
    let api: Api<Pod> = Api::namespaced(client, ns);
    let source = format!("{ns}/{pod}/{container}");
    let mut since = None;
    let mut retry = 1;
    let mut last_timestamp = String::new();
    let mut last_lines = std::collections::VecDeque::new();
    loop {
        match api.get(pod).await {
            Ok(current) if current.uid().as_deref() != Some(expected_uid) => return,
            Err(kube::Error::Api(status)) if matches!(status.code, 403 | 404) => return,
            _ => {}
        }
        let params = LogParams {
            container: Some(container.into()),
            follow: !options.previous,
            previous: options.previous,
            timestamps: true,
            tail_lines: if since.is_none() { Some(1000) } else { None },
            since_seconds: if since.is_none() {
                options.since_seconds
            } else {
                None
            },
            since_time: since,
            ..Default::default()
        };
        let mut replay = last_lines.clone();
        match api.log_stream(pod, &params).await {
            Ok(stream) => {
                sender
                    .send(Message::Status(format!("Streaming {source}")))
                    .await;
                retry = 1;
                let mut stream = Box::pin(stream);
                let mut decoder = Decoder::default();
                let mut bytes = [0u8; 8192];
                loop {
                    match stream.read(&mut bytes).await {
                        Ok(0) => break,
                        Ok(n) => {
                            for text in decoder.feed(&bytes[..n]) {
                                if let Some((ts, _)) = text.split_once(' ')
                                    && let Ok(stamp) = ts.parse::<k8s_openapi::jiff::Timestamp>()
                                {
                                    if ts == last_timestamp && replay.front() == Some(&text) {
                                        replay.pop_front();
                                        continue;
                                    }
                                    replay.clear();
                                    if ts != last_timestamp {
                                        last_timestamp = ts.into();
                                        last_lines.clear();
                                    }
                                    since = Some(stamp);
                                    last_lines.push_back(text.clone());
                                    if last_lines.len() > 256 {
                                        last_lines.pop_front();
                                    }
                                }
                                if !sender
                                    .send(Message::Line(LogLine {
                                        source: source.clone(),
                                        text,
                                    }))
                                    .await
                                {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            sender.send(Message::Status(format!("{source}: {e}"))).await;
                            break;
                        }
                    }
                }
                let remainder = decoder.finish();
                if !remainder.is_empty() {
                    sender
                        .send(Message::Line(LogLine {
                            source: source.clone(),
                            text: remainder,
                        }))
                        .await;
                }
                if options.previous {
                    sender
                        .send(Message::Status(format!("Previous logs loaded: {source}")))
                        .await;
                    return;
                }
                if let Ok(p) = api.get(pod).await
                    && let Some(status) = p.status
                {
                    let stopped = status
                        .container_statuses
                        .iter()
                        .flatten()
                        .chain(status.init_container_statuses.iter().flatten())
                        .any(|s| {
                            s.name == container
                                && s.state.as_ref().is_some_and(|s| s.terminated.is_some())
                        });
                    if stopped {
                        sender
                            .send(Message::Status(format!("Container finished: {source}")))
                            .await;
                        return;
                    }
                }
                sender
                    .send(Message::Status(format!("Reconnecting {source}…")))
                    .await;
            }
            Err(e) => {
                let permanent =
                    matches!(&e,kube::Error::Api(s) if matches!(s.code,400|401|403|404));
                if !sender
                    .send(Message::Status(format!(
                        "{source}: {}",
                        cluster::safe_source_text(&e)
                    )))
                    .await
                {
                    return;
                }
                if permanent || options.previous {
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(retry)).await;
        retry = (retry * 2).min(30);
    }
}
