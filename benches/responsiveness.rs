use kube::{
    api::{ApiResource, DynamicObject, GroupVersionKind},
    runtime::watcher,
};
use kube_tui::{
    logs::{LogBuffer, LogLine},
    store::watch::ResourceStore,
    ui::{
        hit::HitRegistry,
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
    let objects:Vec<_>=(0..10_000).map(|i|{let mut p=DynamicObject::new(&format!("pod-{i:05}"),&ar).within("demo");p.data=serde_json::json!({"status":{"phase":"Running","containerStatuses":[{"ready":true,"restartCount":0}]},"spec":{"containers":[{"name":"app"}]}});Arc::new(p)}).collect();
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
    let mut store = ResourceStore::new();
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
