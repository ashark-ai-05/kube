use iced::widget::{button, column, container, pick_list, row, scrollable, text, text_input, Space};
use iced::{Alignment, Element, Length, Task, Theme};
use kube::Client;
use std::fmt;
use voss::inspect::{inspect_with_optional_openai, openai_api_key_present, summarize};
use voss::snapshot::{
    connect, current_context_name, detail_to_objects, list_namespaces, list_pods, load_pod_detail,
    Connection, PodDetail, PodRow,
};

const LOG_TAIL: i64 = 80;

#[derive(Clone)]
struct ClientHandle(Client);

impl fmt::Debug for ClientHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Client")
    }
}


fn main() -> iced::Result {
    iced::application("Voss — Kubernetes inspector", App::update, App::view)
        .theme(|_| Theme::TokyoNight)
        .window_size((1280.0, 820.0))
        .run_with(App::boot)
}

struct App {
    status: String,
    connection: Option<Connection>,
    client: Option<ClientHandle>,
    namespaces: Vec<String>,
    namespace: Option<String>,
    pods: Vec<PodRow>,
    selected_pod: Option<String>,
    detail: Option<PodDetail>,
    inspect_q: String,
    inspect_a: String,
    loading: bool,
}

#[derive(Debug, Clone)]
enum Message {
    Booted(Result<(ConnectionHint, ClientHandle), String>),
    Namespaces(Result<Vec<String>, String>),
    NamespacePicked(String),
    Pods(Result<Vec<PodRow>, String>),
    SelectPod(String),
    Detail(Result<PodDetail, String>),
    InspectQuery(String),
    RunInspect,
    InspectDone(String),
    Refresh,
}

#[derive(Debug, Clone)]
struct ConnectionHint {
    inner: Connection,
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let app = Self {
            status: "Connecting current kubecontext…".into(),
            connection: None,
            client: None,
            namespaces: Vec::new(),
            namespace: None,
            pods: Vec::new(),
            selected_pod: None,
            detail: None,
            inspect_q: String::new(),
            inspect_a: if openai_api_key_present() {
                "OPENAI_API_KEY is set — inspect can call OpenAI. Answers still cite fetched objects only.".into()
            } else {
                "No OPENAI_API_KEY — inspect uses local retrieval + summary with citations.".into()
            },
            loading: true,
        };
        (app, Task::perform(boot_connect(), Message::Booted))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Booted(Ok((hint, client))) => {
                self.connection = Some(hint.inner);
                self.client = Some(client.clone());
                self.status = format!(
                    "Connected via {} (context {})",
                    self.connection.as_ref().unwrap().kubeconfig_path,
                    self.connection.as_ref().unwrap().context
                );
                self.loading = true;
                Task::perform(list_namespaces(client.0.clone()), Message::Namespaces)
            }
            Message::Booted(Err(e)) => {
                self.loading = false;
                self.status = e;
                self.pods.clear();
                self.detail = None;
                Task::none()
            }
            Message::Namespaces(Ok(ns)) => {
                self.namespaces = ns;
                self.loading = false;
                if self.namespace.is_none() {
                    self.namespace = self
                        .namespaces
                        .iter()
                        .find(|n| *n == "default")
                        .cloned()
                        .or_else(|| self.namespaces.first().cloned());
                }
                if let (Some(c), Some(ns)) = (self.client.clone(), self.namespace.clone()) {
                    self.loading = true;
                    return Task::perform(list_pods(c.0, ns), Message::Pods);
                }
                Task::none()
            }
            Message::Namespaces(Err(e)) => {
                self.loading = false;
                self.status = e;
                Task::none()
            }
            Message::NamespacePicked(ns) => {
                self.namespace = Some(ns.clone());
                self.selected_pod = None;
                self.detail = None;
                self.pods.clear();
                if let Some(c) = self.client.clone() {
                    self.loading = true;
                    Task::perform(list_pods(c.0, ns), Message::Pods)
                } else {
                    Task::none()
                }
            }
            Message::Pods(Ok(pods)) => {
                self.pods = pods;
                self.loading = false;
                Task::none()
            }
            Message::Pods(Err(e)) => {
                self.loading = false;
                self.pods.clear();
                self.status = e;
                Task::none()
            }
            Message::SelectPod(name) => {
                self.selected_pod = Some(name.clone());
                if let (Some(c), Some(ns)) = (self.client.clone(), self.namespace.clone()) {
                    self.loading = true;
                    Task::perform(load_pod_detail(c.0, ns, name, LOG_TAIL), Message::Detail)
                } else {
                    Task::none()
                }
            }
            Message::Detail(Ok(d)) => {
                self.detail = Some(d);
                self.loading = false;
                Task::none()
            }
            Message::Detail(Err(e)) => {
                self.loading = false;
                self.detail = None;
                self.status = e;
                Task::none()
            }
            Message::InspectQuery(q) => {
                self.inspect_q = q;
                Task::none()
            }
            Message::RunInspect => {
                let Some(detail) = self.detail.as_ref() else {
                    self.inspect_a =
                        "Select a pod first. Inspect only answers from fetched objects.".into();
                    return Task::none();
                };
                let ns = self.namespace.clone().unwrap_or_default();
                let objects = detail_to_objects(&ns, detail);
                let q = self.inspect_q.clone();
                self.loading = true;
                Task::perform(
                    async move {
                        let mut ans = inspect_with_optional_openai(&objects).await;
                        if !q.trim().is_empty() {
                            let local = summarize(&objects);
                            ans.text = format!("Question: {q}\n\n{}", local.text);
                        }
                        ans.text
                    },
                    Message::InspectDone,
                )
            }
            Message::InspectDone(text) => {
                self.inspect_a = text;
                self.loading = false;
                Task::none()
            }
            Message::Refresh => {
                if let (Some(c), Some(ns)) = (self.client.clone(), self.namespace.clone()) {
                    self.loading = true;
                    Task::perform(list_pods(c.0, ns), Message::Pods)
                } else {
                    Task::perform(boot_connect(), Message::Booted)
                }
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let header = row![
            text("Voss").size(28),
            text("  native inspector").size(16),
            Space::with_width(Length::Fill),
            button("Refresh").on_press(Message::Refresh),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let ctx = self
            .connection
            .as_ref()
            .map(|c| format!("context {} · {}", c.context, c.kubeconfig_path))
            .unwrap_or_else(|| "not connected".into());

        let ns_picker = pick_list(
            self.namespaces.clone(),
            self.namespace.clone(),
            Message::NamespacePicked,
        )
        .placeholder("namespace");

        let mut table = column![row![
            cell("NAME", true),
            cell("READY", true),
            cell("PHASE", true),
            cell("RESTARTS", true),
            cell("AGE", true),
            cell("NODE", true),
        ]
        .spacing(8)];
        if self.pods.is_empty() {
            table = table.push(text(if self.client.is_none() {
                "No pods — connect failed or kubeconfig missing. Live cluster state is never invented."
            } else if self.loading {
                "Loading pods from the API…"
            } else {
                "No pods in this namespace (empty list from the API)."
            }));
        } else {
            for p in &self.pods {
                let name = p.name.clone();
                let selected = self.selected_pod.as_deref() == Some(name.as_str());
                let restarts = p.restarts.to_string();
                let r = row![
                    cell(&p.name, selected),
                    cell(&p.ready, false),
                    cell(&p.phase, false),
                    cell(&restarts, false),
                    cell(&p.age, false),
                    cell(&p.node, false),
                ]
                .spacing(8);
                table = table.push(button(r).on_press(Message::SelectPod(name)));
            }
        }

        let detail_pane: Element<_> = if let Some(d) = &self.detail {
            let mut ccol = column![text("Containers").size(18)];
            for c in &d.containers {
                ccol = ccol.push(text(format!(
                    "{}  {}  ready={}  restarts={}  {}",
                    c.name, c.image, c.ready, c.restarts, c.state
                )));
            }
            let mut cond = column![text("Conditions").size(18)];
            if d.conditions.is_empty() {
                cond = cond.push(text("(none fetched)"));
            } else {
                for c in &d.conditions {
                    cond = cond.push(text(c.clone()));
                }
            }
            let mut ev = column![text("Events").size(18)];
            if d.events.is_empty() {
                ev = ev.push(text("(no events returned by the API)"));
            } else {
                for e in &d.events {
                    ev = ev.push(text(format!(
                        "{} {} — {}",
                        e.type_, e.reason, e.message
                    )));
                }
            }
            let logs = column![
                text(format!(
                    "Logs (last {LOG_TAIL}, container {})",
                    d.log_container.as_deref().unwrap_or("-")
                ))
                .size(18),
                text(if d.log_lines.is_empty() {
                    "(no log lines fetched)".into()
                } else {
                    d.log_lines.join("\n")
                })
            ];
            scrollable(
                column![ccol, cond, ev, logs]
                    .spacing(12)
                    .padding(8)
                    .width(Length::Fill),
            )
            .height(Length::Fill)
            .into()
        } else {
            text("Select a pod to load containers, conditions, events, and logs from the API.")
                .into()
        };

        let inspect = column![
            text("Inspect (citations from fetched ns/pod/event/log)").size(18),
            text_input("optional question", &self.inspect_q)
                .on_input(Message::InspectQuery)
                .on_submit(Message::RunInspect),
            button("Inspect").on_press(Message::RunInspect),
            scrollable(text(&self.inspect_a)).height(Length::FillPortion(1)),
        ]
        .spacing(8)
        .width(Length::FillPortion(1));

        let body = row![
            column![
                row![text("Namespace"), ns_picker].spacing(8),
                scrollable(table.spacing(4)).height(Length::FillPortion(1)),
                container(detail_pane).height(Length::FillPortion(1)),
            ]
            .spacing(10)
            .width(Length::FillPortion(2)),
            inspect,
        ]
        .spacing(16);

        container(
            column![
                header,
                text(ctx),
                text(&self.status).size(14),
                if self.loading {
                    text("working…")
                } else {
                    text("")
                },
                body,
                text("Voss is not affiliated with Lens, OpenLens, or k9s. Secrets are redacted by default. v0 is read-only.")
                    .size(12),
            ]
            .spacing(10)
            .padding(16),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

fn cell(s: impl Into<String>, emphasize: bool) -> Element<'static, Message> {
    let s = s.into();
    let t = if emphasize {
        text(s).size(14)
    } else {
        text(s).size(13)
    };
    container(t).width(140).into()
}

async fn boot_connect() -> Result<(ConnectionHint, ClientHandle), String> {
    let (client, mut conn) = connect().await?;
    if let Ok(name) = current_context_name() {
        conn.context = name;
    }
    Ok((ConnectionHint { inner: conn }, ClientHandle(client)))
}
