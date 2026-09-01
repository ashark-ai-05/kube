use crate::app::event::{AppEvent, FetchedEvents, WatchStatus, coalesce};
use crate::app::session::{Session, SharedSession, restart_watch, switch_cluster};
use crate::cli::NamespaceScope;
use crate::cluster::{
    AuthMethod, ClusterId, ClusterRegistry, ConnectOptions, ConnectionState, is_valid_namespace_name,
    list_namespaces,
};
use crate::gui::backend::{
    fetch_pod_logs, spawn_discovery_and_watches, spawn_events_fetch, spawn_refetch_wake,
    spawn_table_fetch, truncate_error,
};
use crate::gui::tree::{KindTree, TreeGroup, TreeKind, TreeRow, flatten};
use crate::inspect::{inspect_local, maybe_llm, redact_object};
use crate::store::columns::{ColumnSource, column_source};
use crate::store::events::EventRow;
use crate::store::multi::KindAvailability;
use crate::store::table::{
    SortState, TABLE_REFETCH_DEBOUNCE, TableData, refetch_is_due, row_identity, sort_table_rows,
    sorted_indices,
};
use crate::store::watch::StoreId;
use iced::widget::{button, column, container, row, scrollable, text, text_input, Row, Space};
use iced::{Element, Length, Subscription, Task};
use futures::SinkExt;
use kube::api::{DynamicObject, GroupVersionKind, ResourceExt};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Mutex, mpsc};

#[derive(Clone)]
pub struct Flags {
    pub scope: NamespaceScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Overview,
    Yaml,
    Events,
    Logs,
    Inspect,
}

#[derive(Debug, Clone)]
struct OpenDetail {
    gvk: GroupVersionKind,
    namespace: Option<String>,
    name: String,
    store: StoreId,
    events: Vec<EventRow>,
    events_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
enum Overlay {
    #[default]
    None,
    Cluster { filter: String },
    Namespace { filter: String },
}

pub struct KubeGui {
    tx: mpsc::UnboundedSender<AppEvent>,
    rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<AppEvent>>>,
    session: Option<SharedSession>,
    last_error: Option<String>,
    tree: KindTree,
    selected_row: usize,
    sort: Option<SortState>,
    detail_tab: DetailTab,
    detail: Option<OpenDetail>,
    overlay: Overlay,
    inspect_text: String,
    logs: String,
    cluster_label: String,
    ns_label: String,
    watch_status: WatchStatus,
    rows: Vec<Vec<String>>,
    headers: Vec<String>,
    row_ids: Vec<Option<(Option<String>, String)>>,
    yaml: String,
    objects: Vec<Arc<DynamicObject>>,
    table: Option<TableData>,
    active_kind: GroupVersionKind,
    boot_error: Option<String>,
    connect_opts: ConnectOptions,
}

#[derive(Debug, Clone)]
pub enum Message {
    Backend(Option<AppEvent>),
    Refresh,
    SelectKind(GroupVersionKind),
    ToggleGroup(usize),
    SelectRow(usize),
    SortColumn(usize),
    Tab(DetailTab),
    OpenClusterPicker,
    OpenNamespacePicker,
    OverlayFilter(String),
    PickCluster(String),
    PickNamespace(String),
    CloseOverlay,
    Inspect,
    InspectDone(String),
    LogsDone(String),
    Booted(Result<BootOk, String>),
}

#[derive(Clone)]
pub struct BootOk {
    session: SharedSession,
}

impl std::fmt::Debug for BootOk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BootOk")
    }
}

impl KubeGui {
    pub fn boot(flags: Flags) -> (Self, Task<Message>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let connect_opts = ConnectOptions {
            kubeconfig_paths: crate::cluster::kubeconfig_paths_from_env(
                std::env::var("KUBECONFIG").ok().as_deref(),
                std::path::Path::new(&home),
            ),
            ..Default::default()
        };
        let gui = Self {
            tx: tx.clone(),
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
            session: None,
            last_error: None,
            tree: KindTree {
                groups: vec![],
                selected: 0,
                scroll: 0,
            },
            selected_row: 0,
            sort: None,
            detail_tab: DetailTab::Overview,
            detail: None,
            overlay: Overlay::None,
            inspect_text: String::new(),
            logs: String::new(),
            cluster_label: "connecting…".into(),
            ns_label: String::new(),
            watch_status: WatchStatus::Initialising,
            rows: vec![],
            headers: vec![],
            row_ids: vec![],
            yaml: String::new(),
            objects: vec![],
            table: None,
            active_kind: crate::app::session::default_kind(),
            boot_error: None,
            connect_opts: connect_opts.clone(),
        };
        let scope = flags.scope;
        let startup_opts = ConnectOptions {
            allow_interactive_auth: true,
            ..connect_opts
        };
        let cmd = Task::perform(boot(scope, startup_opts, tx), Message::Booted);
        (gui, cmd)
    }

    pub fn title(&self) -> String {
        format!("kube — {}", self.cluster_label)
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let rx = self.rx.clone();
        Subscription::run_with_id(
            "backend-events",
            iced::stream::channel(64, |mut sender| async move {
                loop {
                    let ev = rx.lock().await.recv().await;
                    let done = ev.is_none();
                    if sender.send(Message::Backend(ev)).await.is_err() {
                        break;
                    }
                    if done {
                        break;
                    }
                }
            }),
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Booted(Ok(ok)) => {
                self.session = Some(ok.session);
                Task::perform(std::future::ready(()), |_| Message::Refresh)
            }
            Message::Booted(Err(e)) => {
                self.boot_error = Some(e);
                Task::none()
            }
            Message::Backend(Some(ev)) => {
                if matches!(ev, AppEvent::Quit) {
                    std::process::exit(0);
                }
                let batch = coalesce(vec![ev]);
                if let Some(e) = batch.errors.last() {
                    self.last_error = Some(truncate_error(e.clone()));
                }
                for se in &batch.session_events {
                    if let crate::app::session::SessionEvent::ConnectFailed { id, reason } = se {
                        self.last_error = Some(truncate_error(format!(
                            "connecting to {}: {reason}",
                            id.0
                        )));
                    }
                    if matches!(se, crate::app::session::SessionEvent::Connected(_)) {
                        self.last_error = None;
                    }
                }
                for f in batch.events_fetched {
                    self.apply_events(f);
                }
                let mut cmds = Vec::new();
                if let Some((store_id, result)) = batch.namespace_list {
                    if let Some(session) = self.session.clone() {
                        cmds.push(Task::perform(
                            async move {
                                let mut s = session.lock().await;
                                if StoreId::of(&s.store) == store_id {
                                    s.namespaces_from_api = Some(result);
                                }
                            },
                            |_| Message::Refresh,
                        ));
                    }
                }
                let need_table = batch.changed_kinds.contains(&self.active_kind)
                    || batch.wake
                    || batch.kinds_discovered;
                cmds.push(self.refresh_view(need_table));
                Task::batch(cmds)
            }
            Message::Backend(None) => Task::none(),
            Message::Refresh => self.refresh_view(true),
            Message::SelectKind(gvk) => {
                if let Some(session) = &self.session {
                    let session = session.clone();
                    let gvk2 = gvk.clone();
                    self.active_kind = gvk;
                    self.selected_row = 0;
                    self.detail = None;
                    return Task::perform(
                        async move {
                            session.lock().await.active_kind = gvk2;
                        },
                        |_| Message::Refresh,
                    );
                }
                Task::none()
            }
            Message::ToggleGroup(i) => {
                if let Some(g) = self.tree.groups.get_mut(i) {
                    g.expanded = !g.expanded;
                }
                Task::none()
            }
            Message::SelectRow(i) => {
                self.selected_row = i;
                self.open_detail_for_row();
                self.refresh_view(false)
            }
            Message::SortColumn(col) => {
                self.sort = Some(match self.sort {
                    Some(s) if s.column == col => SortState {
                        column: col,
                        descending: !s.descending,
                    },
                    _ => SortState {
                        column: col,
                        descending: false,
                    },
                });
                Task::perform(std::future::ready(()), |_| Message::Refresh)
            }
            Message::Tab(tab) => {
                self.detail_tab = tab;
                let mut cmds = Vec::new();
                if tab == DetailTab::Events {
                    if let (Some(session), Some(open)) = (&self.session, &self.detail) {
                        let session = session.clone();
                        let open = open.clone();
                        let tx = self.tx.clone();
                        cmds.push(Task::perform(
                            async move {
                                let client = session.lock().await.client.clone();
                                spawn_events_fetch(
                                    client,
                                    open.gvk,
                                    open.namespace,
                                    open.name,
                                    open.store,
                                    tx,
                                );
                            },
                            |_| Message::Refresh,
                        ));
                    }
                }
                if tab == DetailTab::Logs {
                    cmds.push(self.fetch_logs());
                }
                if tab == DetailTab::Inspect {
                    cmds.push(self.run_inspect());
                }
                Task::batch(cmds)
            }
            Message::OpenClusterPicker => {
                self.overlay = Overlay::Cluster {
                    filter: String::new(),
                };
                Task::none()
            }
            Message::OpenNamespacePicker => {
                self.overlay = Overlay::Namespace {
                    filter: String::new(),
                };
                if let Some(session) = self.session.clone() {
                    let tx = self.tx.clone();
                    return Task::perform(
                        async move {
                            let (client, store) = {
                                let s = session.lock().await;
                                (s.client.clone(), StoreId::of(&s.store))
                            };
                            let result = list_namespaces(&client).await;
                            let _ = tx.send(AppEvent::NamespacesListed { store, result });
                        },
                        |_| Message::Refresh,
                    );
                }
                Task::none()
            }
            Message::OverlayFilter(s) => {
                match &mut self.overlay {
                    Overlay::Cluster { filter } | Overlay::Namespace { filter } => *filter = s,
                    Overlay::None => {}
                }
                Task::none()
            }
            Message::PickCluster(name) => {
                self.overlay = Overlay::None;
                self.switch_to(name)
            }
            Message::PickNamespace(name) => {
                self.overlay = Overlay::None;
                self.rescope(name)
            }
            Message::CloseOverlay => {
                self.overlay = Overlay::None;
                Task::none()
            }
            Message::Inspect => self.run_inspect(),
            Message::InspectDone(t) => {
                self.inspect_text = t;
                Task::none()
            }
            Message::LogsDone(t) => {
                self.logs = t;
                Task::none()
            }
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        if let Some(err) = &self.boot_error {
            return container(text(format!("Could not connect: {err}")))
                .padding(24)
                .into();
        }

        let header = row![
            button(text(&self.cluster_label)).on_press(Message::OpenClusterPicker),
            button(text(&self.ns_label)).on_press(Message::OpenNamespacePicker),
            text(format!("{:?}", self.watch_status)),
            Space::with_width(Length::Fill),
            text(self.last_error.clone().unwrap_or_default()),
        ]
        .spacing(8)
        .padding(8);

        let sidebar = self.sidebar();
        let table = self.table_view();
        let detail = self.detail_view();

        let body = row![sidebar, table, detail]
            .spacing(4)
            .height(Length::Fill);

        let mut root = column![header, body].spacing(4);

        if !matches!(self.overlay, Overlay::None) {
            root = root.push(self.overlay_view());
        }

        container(root)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

impl KubeGui {
    fn sidebar(&self) -> Element<'_, Message> {
        let mut col = column![text("Kinds")].spacing(2).padding(8).width(220);
        for row in flatten(&self.tree) {
            match row {
                TreeRow::Group { index, group } => {
                    let mark = if group.expanded { "▾" } else { "▸" };
                    col = col.push(
                        button(text(format!("{mark} {}", group.label)))
                            .on_press(Message::ToggleGroup(index))
                            .width(Length::Fill),
                    );
                }
                TreeRow::Kind { kind, .. } => {
                    let avail = match &kind.availability {
                        KindAvailability::Watching => kind
                            .count
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "…".into()),
                        KindAvailability::Unavailable { reason } => format!("403 {reason}"),
                        KindAvailability::NotWatched => "not watched".into(),
                    };
                    let label = format!("{}  {avail}", kind.label);
                    col = col.push(
                        button(text(label))
                            .on_press(Message::SelectKind(kind.gvk.clone()))
                            .width(Length::Fill),
                    );
                }
            }
        }
        scrollable(col).height(Length::Fill).into()
    }

    fn table_view(&self) -> Element<'_, Message> {
        let mut header_row = Row::new().spacing(6);
        for (i, h) in self.headers.iter().enumerate() {
            header_row = header_row.push(
                button(text(h.clone()))
                    .on_press(Message::SortColumn(i))
                    .width(Length::Fill),
            );
        }
        let mut col = column![header_row].spacing(2).padding(8);
        for (i, cells) in self.rows.iter().enumerate() {
            let line = cells.join("   ");
            let mut btn = button(text(line)).width(Length::Fill);
            if i == self.selected_row {
                btn = btn.style(iced::widget::button::primary);
            }
            col = col.push(btn.on_press(Message::SelectRow(i)));
        }
        if self.rows.is_empty() {
            col = col.push(text("No objects in this view."));
        }
        scrollable(col)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn detail_view(&self) -> Element<'_, Message> {
        let tabs = row![
            button("Overview").on_press(Message::Tab(DetailTab::Overview)),
            button("YAML").on_press(Message::Tab(DetailTab::Yaml)),
            button("Events").on_press(Message::Tab(DetailTab::Events)),
            button("Logs").on_press(Message::Tab(DetailTab::Logs)),
            button("Inspect").on_press(Message::Tab(DetailTab::Inspect)),
        ]
        .spacing(4);

        let body: Element<_> = match self.detail_tab {
            DetailTab::Overview => {
                let open = self.detail.as_ref();
                let t = match open {
                    Some(d) => format!(
                        "{} {}/{}",
                        d.gvk.kind,
                        d.namespace.as_deref().unwrap_or("-"),
                        d.name
                    ),
                    None => "Click a row to inspect.".into(),
                };
                text(t).into()
            }
            DetailTab::Yaml => text(self.yaml.clone()).into(),
            DetailTab::Events => {
                if let Some(d) = &self.detail {
                    if let Some(e) = &d.events_error {
                        text(e.clone()).into()
                    } else if d.events.is_empty() {
                        text("No events (or still loading).").into()
                    } else {
                        let mut c = column![];
                        for e in &d.events {
                            c = c.push(text(format!(
                                "{} {} ×{} {} — {}",
                                e.age, e.kind, e.count, e.reason, e.message
                            )));
                        }
                        c.into()
                    }
                } else {
                    text("Select a row.").into()
                }
            }
            DetailTab::Logs => text(self.logs.clone()).into(),
            DetailTab::Inspect => text(self.inspect_text.clone()).into(),
        };

        container(column![tabs, scrollable(body)].spacing(8).padding(8))
            .width(Length::Fill)
            .into()
    }

    fn overlay_view(&self) -> Element<'_, Message> {
        match &self.overlay {
            Overlay::None => Space::new(0, 0).into(),
            Overlay::Cluster { filter } => {
                let mut col = column![
                    text("Cluster"),
                    text_input("filter", filter).on_input(Message::OverlayFilter),
                    button("Close").on_press(Message::CloseOverlay),
                ]
                .spacing(4)
                .padding(8);
                if let Some(session) = &self.session {
                    if let Ok(s) = session.try_lock() {
                        for e in s.registry.entries() {
                            if !filter.is_empty()
                                && !e.id.0.to_lowercase().contains(&filter.to_lowercase())
                            {
                                continue;
                            }
                            let st = match &e.state {
                                ConnectionState::Connected => "connected",
                                ConnectionState::Connecting => "connecting",
                                ConnectionState::Failed { .. } => "failed",
                                ConnectionState::Disconnected => "",
                            };
                            col = col.push(
                                button(text(format!("{} {st}", e.id.0)))
                                    .on_press(Message::PickCluster(e.id.0.clone())),
                            );
                        }
                    }
                }
                container(col).into()
            }
            Overlay::Namespace { filter } => {
                let mut col = column![
                    text("Namespace (type a name if listing is forbidden)"),
                    text_input("namespace", filter).on_input(Message::OverlayFilter),
                    button("Use typed name").on_press(Message::PickNamespace(filter.clone())),
                    button("all namespaces").on_press(Message::PickNamespace(ALL.into())),
                    button("Close").on_press(Message::CloseOverlay),
                ]
                .spacing(4)
                .padding(8);
                if let Some(session) = &self.session {
                    if let Ok(s) = session.try_lock() {
                        let loaded: BTreeSet<String> = self
                            .objects
                            .iter()
                            .filter_map(|o| o.metadata.namespace.clone())
                            .collect();
                        let api = s.namespaces_from_api.as_ref();
                        match api {
                            Some(Err(e)) => {
                                col = col.push(text(e.explanation()));
                            }
                            Some(Ok(list)) => {
                                for n in list {
                                    if filter.is_empty() || n.contains(filter.as_str()) {
                                        col = col.push(
                                            button(text(n.clone()))
                                                .on_press(Message::PickNamespace(n.clone())),
                                        );
                                    }
                                }
                            }
                            None => {}
                        }
                        for n in loaded {
                            if filter.is_empty() || n.contains(filter.as_str()) {
                                col = col.push(
                                    button(text(n.clone()))
                                        .on_press(Message::PickNamespace(n)),
                                );
                            }
                        }
                    }
                }
                container(col).into()
            }
        }
    }

    fn apply_events(&mut self, f: FetchedEvents) {
        let Some(open) = &mut self.detail else {
            return;
        };
        if open.gvk != f.gvk || open.namespace != f.namespace || open.name != f.name {
            return;
        }
        if open.store != f.store {
            return;
        }
        match f.result {
            Ok(rows) => {
                open.events = rows;
                open.events_error = None;
            }
            Err(e) => open.events_error = Some(e),
        }
    }

    fn open_detail_for_row(&mut self) {
        let Some(id) = self.row_ids.get(self.selected_row).cloned().flatten() else {
            return;
        };
        let store = match &self.session {
            Some(s) => match s.try_lock() {
                Ok(g) => StoreId::of(&g.store),
                Err(_) => return,
            },
            None => return,
        };
        self.detail = Some(OpenDetail {
            gvk: self.active_kind.clone(),
            namespace: id.0.clone(),
            name: id.1.clone(),
            store: store.clone(),
            events: vec![],
            events_error: None,
        });
        if let Some(session) = &self.session {
            if let Ok(s) = session.try_lock() {
                spawn_events_fetch(
                    s.client.clone(),
                    self.active_kind.clone(),
                    id.0,
                    id.1,
                    store,
                    self.tx.clone(),
                );
            }
        }
        self.fill_yaml();
    }

    fn fill_yaml(&mut self) {
        self.yaml.clear();
        let Some(open) = &self.detail else {
            return;
        };
        let Some(obj) = self.objects.iter().find(|o| {
            o.name_any() == open.name && o.namespace() == open.namespace
        }) else {
            return;
        };
        let red = redact_object((**obj).clone());
        self.yaml = serde_norway::to_string(&red).unwrap_or_else(|_| format!("{red:?}"));
    }

    fn fetch_logs(&self) -> Task<Message> {
        let Some(session) = self.session.clone() else {
            return Task::none();
        };
        let Some(open) = self.detail.clone() else {
            return Task::none();
        };
        Task::perform(
            async move {
                let client = session.lock().await.client.clone();
                fetch_pod_logs(client, open.namespace, open.name).await
            },
            Message::LogsDone,
        )
    }

    fn run_inspect(&self) -> Task<Message> {
        let Some(open) = &self.detail else {
            return Task::none();
        };
        let Some(obj) = self
            .objects
            .iter()
            .find(|o| o.name_any() == open.name && o.namespace() == open.namespace)
            .cloned()
        else {
            return Task::none();
        };
        let events = open.events.clone();
        Task::perform(
            async move {
                let (local, cites) = inspect_local(&obj, &events);
                let cite_block: String = cites
                    .iter()
                    .map(|c| {
                        format!(
                            "- {} {}/{} ({})",
                            c.kind,
                            c.namespace.as_deref().unwrap_or("-"),
                            c.name,
                            c.note
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let prompt = format!("{local}\n\nCitations:\n{cite_block}");
                match maybe_llm(&prompt).await {
                    Some(extra) => format!("{local}\n\n---\n{extra}\n\nCitations:\n{cite_block}"),
                    None => format!("{prompt}"),
                }
            },
            Message::InspectDone,
        )
    }

    fn switch_to(&self, name: String) -> Task<Message> {
        let Some(session) = self.session.clone() else {
            return Task::none();
        };
        let tx = self.tx.clone();
        let opts = ConnectOptions {
            context: Some(name.clone()),
            allow_interactive_auth: false,
            ..self.connect_opts.clone()
        };
        Task::perform(
            async move {
                switch_cluster(
                    session.clone(),
                    ClusterId(name),
                    None,
                    tx.clone(),
                    || crate::cluster::connect_with(&opts),
                    {
                        let session = session.clone();
                        let tx = tx.clone();
                        move |client, store, ns| {
                            spawn_discovery_and_watches(session, client, store, ns, tx)
                        }
                    },
                )
                .await;
            },
            |_| Message::Refresh,
        )
    }

    fn rescope(&self, name: String) -> Task<Message> {
        let Some(session) = self.session.clone() else {
            return Task::none();
        };
        let tx = self.tx.clone();
        let ns = if name == ALL || name == "all namespaces" {
            None
        } else if is_valid_namespace_name(&name) {
            Some(name)
        } else {
            return Task::none();
        };
        Task::perform(
            async move {
                let session2 = session.clone();
                let tx2 = tx.clone();
                restart_watch(session, ns, move |client, store, n| {
                    spawn_discovery_and_watches(session2, client, store, n, tx2)
                })
                .await;
            },
            |_| Message::Refresh,
        )
    }

    fn refresh_view(&mut self, maybe_fetch_table: bool) -> Task<Message> {
        let Some(session) = self.session.clone() else {
            return Task::none();
        };
        // try_lock so the GUI thread never stalls on the session lock.
        let Ok(s) = session.try_lock() else {
            return Task::none();
        };
        self.active_kind = s.active_kind.clone();
        self.cluster_label = s
            .registry
            .active()
            .map(|e| e.id.0.clone())
            .unwrap_or_else(|| "kube".into());
        self.ns_label = s
            .namespace
            .clone()
            .unwrap_or_else(|| "all namespaces".into());
        if let Ok(store) = s.store.try_read() {
            self.watch_status = store.status(&self.active_kind);
            self.objects = store.objects(&self.active_kind);
            self.table = store.table_data(&self.active_kind);
            self.rebuild_tree(&s.kinds, &store);
            if maybe_fetch_table {
                let last_fetch = store.last_table_fetch(&self.active_kind);
                let due = match store.last_change(&self.active_kind) {
                    Some(changed) => refetch_is_due(
                        last_fetch,
                        changed,
                        Instant::now(),
                        TABLE_REFETCH_DEBOUNCE,
                    ),
                    None => self.table.is_none(),
                };
                if due {
                    if let Some(kind) = s.kinds.iter().find(|k| k.gvk == self.active_kind) {
                        spawn_table_fetch(
                            s.client.clone(),
                            kind.resource.clone(),
                            s.namespace.clone(),
                            self.active_kind.clone(),
                            s.store.clone(),
                            self.tx.clone(),
                        );
                    }
                    spawn_refetch_wake(self.tx.clone(), TABLE_REFETCH_DEBOUNCE);
                }
            }
        }
        if let Some(listed) = &s.namespaces_from_api {
            // picker reads this via try_lock in overlay
            let _ = listed;
        }
        drop(s);
        self.rebuild_rows();
        self.fill_yaml();
        Task::none()
    }

    fn rebuild_tree(
        &mut self,
        kinds: &[crate::cluster::discovery::KindInfo],
        store: &crate::store::watch::ResourceStore,
    ) {
        use std::collections::BTreeMap;
        let mut groups: BTreeMap<String, Vec<TreeKind>> = BTreeMap::new();
        for k in kinds {
            groups.entry(k.group_label.clone()).or_default().push(TreeKind {
                gvk: k.gvk.clone(),
                label: k.gvk.kind.clone(),
                count: Some(store.count(&k.gvk)),
                availability: store.availability(&k.gvk),
            });
        }
        let expanded: std::collections::HashMap<String, bool> = self
            .tree
            .groups
            .iter()
            .map(|g| (g.label.clone(), g.expanded))
            .collect();
        self.tree.groups = groups
            .into_iter()
            .map(|(label, kinds)| TreeGroup {
                expanded: expanded.get(&label).copied().unwrap_or(true),
                label,
                kinds,
            })
            .collect();
    }

    fn rebuild_rows(&mut self) {
        match column_source(&self.active_kind, self.table.clone()) {
            ColumnSource::Server(mut t) => {
                if let Some(sort) = &self.sort {
                    sort_table_rows(&mut t.rows, sort);
                }
                self.headers = t.columns.iter().map(|c| c.name.clone()).collect();
                self.rows = t.rows.iter().map(|r| r.cells.clone()).collect();
                self.row_ids = (0..t.rows.len())
                    .map(|i| row_identity(&t, i))
                    .collect();
            }
            ColumnSource::Builtin(cols) => {
                self.headers = cols.iter().map(|c| c.header.to_string()).collect();
                let cells: Vec<Vec<String>> = self
                    .objects
                    .iter()
                    .map(|o| cols.iter().map(|c| (c.extract)(o)).collect())
                    .collect();
                let order = if let Some(sort) = &self.sort {
                    sorted_indices(&cells, sort)
                } else {
                    (0..cells.len()).collect()
                };
                self.rows = order.iter().map(|&i| cells[i].clone()).collect();
                self.row_ids = order
                    .iter()
                    .map(|&i| {
                        let o = &self.objects[i];
                        Some((o.namespace(), o.name_any()))
                    })
                    .collect();
            }
        }
        if self.selected_row >= self.rows.len() && !self.rows.is_empty() {
            self.selected_row = self.rows.len() - 1;
        }
    }
}

const ALL: &str = "all namespaces";

async fn boot(
    cli_scope: NamespaceScope,
    startup_opts: ConnectOptions,
    tx: mpsc::UnboundedSender<AppEvent>,
) -> Result<BootOk, String> {
    let client = crate::cluster::connect_with(&startup_opts)
        .await
        .map_err(|e| crate::cluster::safe_error_text(&e))?;
    let contexts = crate::cluster::load_contexts().unwrap_or_default();
    let (_name, context_namespace, namespace_from_context) = contexts
        .iter()
        .find(|c| c.is_current)
        .map(|c| {
            let (ns, was_explicit) = c
                .namespace
                .clone()
                .map(|ns| (ns, true))
                .unwrap_or_else(|| ("default".into(), false));
            (c.name.clone(), ns, was_explicit)
        })
        .unwrap_or_else(|| ("unknown".into(), "default".into(), false));
    let (watch_namespace, is_fallback_namespace) = match cli_scope {
        NamespaceScope::One(ns) => (Some(ns), false),
        NamespaceScope::All => (None, false),
        NamespaceScope::FromContext => {
            let is_fallback = !namespace_from_context && context_namespace == "default";
            (Some(context_namespace), is_fallback)
        }
    };
    let session: SharedSession = Arc::new(Mutex::new(Session::new(
        ClusterRegistry::from_contexts(contexts),
        client.clone(),
        watch_namespace.clone(),
        is_fallback_namespace,
    )));
    let store = session.lock().await.store.clone();
    spawn_discovery_and_watches(session.clone(), client, store, watch_namespace, tx);
    let _ = AuthMethod::None;
    Ok(BootOk { session })
}
