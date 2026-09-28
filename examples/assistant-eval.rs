//! Evaluate the shipped action vocabulary; --model-only also measures raw model limitations.
use kube_tui::assistant::{self, local::Bundle};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cases: serde_json::Value = serde_json::from_str(include_str!("../ai/eval.json"))?;
    let model_only = std::env::args().any(|a| a == "--model-only");
    let model = Bundle::discover();
    let mut failed = 0;
    for case in cases.as_array().unwrap() {
        let query = case["query"].as_str().unwrap();
        let start = std::time::Instant::now();
        let result = match assistant::check_query(query) {
            Err(e) => Err(e),
            Ok(()) => match if model_only {
                Ok(None)
            } else {
                assistant::parse(query)
            } {
                Ok(Some(intent)) => Ok(intent),
                Err(e) => Err(e),
                Ok(None) => match &model {
                    Some(m) => m.infer(query).await,
                    None => Err("No local model bundle found".into()),
                },
            },
        };
        let actual = result.as_ref().map(|i| i.describe());
        let pass = if case["reject"] == true {
            result.is_err()
        } else {
            actual
                .as_ref()
                .is_ok_and(|s| Some(s.as_str()) == case["expected"].as_str())
        };
        if !pass {
            failed += 1;
        }
        println!(
            "{} {:>5} ms | {} | {}",
            if pass { "PASS" } else { "FAIL" },
            start.elapsed().as_millis(),
            query,
            actual.unwrap_or_else(|e| format!("Rejected: {e}"))
        );
    }
    anyhow::ensure!(failed == 0, "{failed} intent evaluations failed");
    Ok(())
}
