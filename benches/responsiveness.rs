use kube::{
    api::{ApiResource, DynamicObject, GroupVersionKind},
    runtime::watcher,
};
use kube_tui::{
    logs::{LogBuffer, LogLine},
    store::watch::ResourceStore,
    ui::{
        hit::HitRegistry,
        log_view::LogView,
        views::table::{TableView, render_table},
    },
};
use ratatui::{Terminal, backend::TestBackend};
use std::{sync::Arc, time::Instant};
fn report(name: &str, mut values: Vec<f64>, budget: f64) {
    values.sort_by(f64::total_cmp);
    let p95 = values[values.len() * 95 / 100];
    println!("{name}: p95={p95:.3} ms; budget={budget:.0} ms");
    assert!(p95 < budget, "{name} exceeded budget");
}
fn main() {
    let ar = ApiResource::erase::<k8s_openapi::api::core::v1::Pod>(&());
    let gvk = GroupVersionKind::gvk("", "v1", "Pod");
    let objects:Vec<_>=(0..10_000).map(|i|{let mut p=DynamicObject::new(&format!("pod-{i:05}"),&ar).within("demo");p.metadata.creation_timestamp = Some(serde_json::from_value(serde_json::json!((chrono::Utc::now()-chrono::Duration::seconds((i*3571%10000000) as i64)).to_rfc3339())).unwrap());p.metadata.labels=Some(std::collections::BTreeMap::from([("app".into(),format!("app-{}",i%50))]));p.data=serde_json::json!({"status":{"phase":"Running","containerStatuses":[{"ready":true,"restartCount":i*7919%25}]},"spec":{"containers":[{"name":"app"}]}});Arc::new(p)}).collect();
    let mut terminal = Terminal::new(TestBackend::new(160, 45)).unwrap();
    let mut view = TableView::new();
    let mut hits = HitRegistry::new();
    let mut samples = vec![];
    for i in 0..1000 {
        view.selected = i * 7;
        hits.clear();
        let start = Instant::now();
        terminal
            .draw(|f| render_table(f, f.area(), &objects, &gvk, &mut view, &mut hits))
            .unwrap();
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    report("10,000-resource cached navigation", samples, 16.);
    for header in ["Restarts", "Age", "Name"] {
        let column = view
            .headers
            .iter()
            .position(|name| name.eq_ignore_ascii_case(header))
            .unwrap();
        view.sort = Some(kube_tui::store::table::SortState {
            column,
            descending: true,
            kind: kube_tui::store::table::SortKind::for_header(header),
        });
        let mut samples = vec![];
        for i in 0..100 {
            view.selected = i * 7;
            hits.clear();
            let start = Instant::now();
            terminal
                .draw(|f| render_table(f, f.area(), &objects, &gvk, &mut view, &mut hits))
                .unwrap();
            samples.push(start.elapsed().as_secs_f64() * 1000.);
        }
        report(&format!("10,000-resource {header} sorting"), samples, 16.);
    }
    view.sort = None;
    let mut workspace = kube_tui::ui::workspace::Workspace::default();
    for pod in &objects {
        workspace.dashboard.metrics.latest.insert(
            ("demo".into(), pod.metadata.name.clone().unwrap()),
            kube_tui::dashboard::metrics::PodUsage {
                usage: kube_tui::dashboard::metrics::Usage {
                    cpu_milli: 5.,
                    memory_bytes: 1048576.,
                },
                timestamp: chrono::Utc::now(),
                containers: std::collections::BTreeMap::new(),
            },
        );
    }
    let mut samples = vec![];
    for _ in 0..100 {
        let start = Instant::now();
        workspace.clear_hits();
        terminal
            .draw(|f| {
                workspace.render_home(
                    f,
                    f.area(),
                    &objects,
                    kube_tui::app::event::WatchStatus::Synced,
                )
            })
            .unwrap();
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    report("10,000-pod cached monitor navigation", samples, 16.);
    let mut samples = vec![];
    for _ in 0..100 {
        workspace.dashboard.metrics.revision += 1;
        let start = Instant::now();
        workspace.clear_hits();
        terminal
            .draw(|f| {
                workspace.render_home(
                    f,
                    f.area(),
                    &objects,
                    kube_tui::app::event::WatchStatus::Synced,
                )
            })
            .unwrap();
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    report("10,000-pod monitor metric refresh", samples, 50.);
    workspace.dashboard.search = "restarts>3 cpu>=5m age>10m label:app".into();
    let mut samples = vec![];
    for i in 0..100 {
        workspace
            .dashboard
            .move_selection(if i % 2 == 0 { 1 } else { -1 });
        let start = Instant::now();
        workspace.clear_hits();
        terminal
            .draw(|f| {
                workspace.render_home(
                    f,
                    f.area(),
                    &objects,
                    kube_tui::app::event::WatchStatus::Synced,
                )
            })
            .unwrap();
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    report("10,000-pod combined-query navigation", samples, 16.);
    let mut samples = vec![];
    for i in 0..100 {
        workspace.dashboard.search = format!("restarts>{} cpu>=5m age>10m label:app", i % 4);
        let start = Instant::now();
        workspace.clear_hits();
        terminal
            .draw(|f| {
                workspace.render_home(
                    f,
                    f.area(),
                    &objects,
                    kube_tui::app::event::WatchStatus::Synced,
                )
            })
            .unwrap();
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    report("10,000-pod combined-query editing", samples, 16.);
    let mut logs = LogBuffer::default();
    let start = Instant::now();
    for i in 0..100_000 {
        logs.push(LogLine {
            source: "demo/web/app".into(),
            text: format!("2026-09-28T00:00:00Z level=info message=request-completed sequence={i}"),
        });
    }
    println!(
        "100,000 log records ingested in {:.1} ms; retained payload {:.1} MiB",
        start.elapsed().as_secs_f64() * 1000.,
        logs.bytes() as f64 / 1048576.
    );
    let mut samples = vec![];
    for _ in 0..100 {
        let start = Instant::now();
        logs.filter("request-completed").unwrap();
        std::hint::black_box(logs.visible(0, 40));
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    report("100,000-line log search", samples, 50.);
    let mut viewport = LogView::default();
    let mut samples = vec![];
    for _ in 0..500 {
        viewport.scroll(&logs, -1);
        let start = Instant::now();
        std::hint::black_box(viewport.rows(&logs, 140, 40, true));
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    report("Wrapped log navigation with active search", samples, 16.);
    let mut long = LogBuffer::default();
    for _ in 0..100 {
        long.push(LogLine {
            source: "demo/web/app".into(),
            text: "request-completed ".repeat(3500),
        });
    }
    long.filter("completed").unwrap();
    let mut samples = vec![];
    viewport.reset();
    for i in 0..100 {
        let start = Instant::now();
        std::hint::black_box(viewport.rows(&long, if i % 2 == 0 { 80 } else { 140 }, 40, true));
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    report(
        "64 KiB log record reflow with highlighted search",
        samples,
        16.,
    );
    let mut store = ResourceStore::new();
    for object in &objects {
        store.apply(&gvk, &ar, watcher::Event::Apply((**object).clone()));
    }
    let mut samples = vec![];
    for i in 0..500 {
        let start = Instant::now();
        // Work admitted by a bounded UI pass, while the log ring is full and
        // the resource cache is changing. Includes a fresh cache snapshot.
        for j in 0..256 {
            logs.push(LogLine {
                source: "demo/web/app".into(),
                text: format!("request-completed {i} {j}"),
            });
        }
        for j in 0..16 {
            store.apply(
                &gvk,
                &ar,
                watcher::Event::Apply((*objects[(i + j) % objects.len()]).clone()),
            );
        }
        let current = store.objects(&gvk);
        view.selected = i * 13;
        hits.clear();
        terminal
            .draw(|f| render_table(f, f.area(), &current, &gvk, &mut view, &mut hits))
            .unwrap();
        std::hint::black_box(logs.visible(0, 40));
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    report(
        "UI batch with full log ring and changing 10,000-resource cache",
        samples,
        50.,
    );
    let mut queued = 0;
    let start = Instant::now();
    for _ in 0..100_000 {
        store.apply(&gvk, &ar, watcher::Event::Apply((*objects[0]).clone()));
        if store.notify_once(&gvk) {
            queued += 1;
        }
    }
    println!(
        "100,000 watch updates in {:.1} ms; queued notifications={queued}",
        start.elapsed().as_secs_f64() * 1000.
    );
    assert_eq!(queued, 1);
}
