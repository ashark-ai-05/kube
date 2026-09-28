//! Requires the optional verified AI bundle. No Kubernetes cluster is contacted.
use kube_tui::{
    assistant::{Intent, local::Bundle},
    ui::inspector::Mode,
};
#[tokio::test]
#[ignore = "requires KUBE_AI_DIR pointing at the packaged model and CPU runtime"]
async fn bundled_model_runs_offline_and_returns_a_validated_action() {
    let bundle = Bundle::discover().expect("AI bundle present");
    let result = bundle.infer("Previous logs for this pod").await;
    if result.is_err() {
        // Print startup diagnostics only for this fixed offline test, never user prompts.
        // This distinguishes backend initialization failures from slow inference in CI.
        let mut diagnostic = tokio::process::Command::new(&bundle.server)
            .arg("-m")
            .arg(&bundle.model)
            .args([
                "--host",
                "127.0.0.1",
                "--port",
                "0",
                "-c",
                "4096",
                "-np",
                "1",
                "-ngl",
                "0",
                "--device",
                "none",
                "--no-op-offload",
                "--no-kv-offload",
                "-t",
                "2",
                "--no-webui",
                "--no-warmup",
                "--reasoning",
                "off",
                "--chat-template-kwargs",
                r#"{"enable_thinking":false}"#,
                "-b",
                "256",
                "-ub",
                "128",
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .kill_on_drop(true)
            .spawn()
            .expect("start runtime diagnostics");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(15), diagnostic.wait()).await;
        let _ = diagnostic.kill().await;
        let _ = diagnostic.wait().await;
    }
    assert_eq!(
        result.unwrap(),
        Intent::Inspect {
            mode: Mode::Logs,
            previous: true
        }
    );
    assert!(bundle.infer("delete all pods").await.is_err());
}
