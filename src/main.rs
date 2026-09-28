#[tokio::main]
async fn main() -> anyhow::Result<()> {
    kube_tui::app::runtime::run().await
}
