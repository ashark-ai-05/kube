//! Requires the optional verified AI bundle. No Kubernetes cluster is contacted.
use kube_tui::{
    assistant::{Intent, local::Bundle},
    ui::inspector::Mode,
};
#[tokio::test]
#[ignore = "requires KUBE_AI_DIR pointing at the packaged model and CPU runtime"]
async fn bundled_model_runs_offline_and_returns_a_validated_action() {
    let bundle = Bundle::discover().expect("AI bundle present");
    assert_eq!(
        bundle.infer("Previous logs for this pod").await.unwrap(),
        Intent::Inspect {
            mode: Mode::Logs,
            previous: true
        }
    );
    assert!(bundle.infer("delete all pods").await.is_err());
}
