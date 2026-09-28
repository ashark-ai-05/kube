//! Per-request CPU worker. Loads only on demand, uses loopback, and is killed on cancellation.
use super::{Intent, decode};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{Request, body::Bytes};
use hyper_util::rt::TokioIo;
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{net::TcpStream, process::Command};

#[derive(Clone)]
pub struct Bundle {
    pub model: PathBuf,
    pub server: PathBuf,
}
impl Bundle {
    pub fn discover() -> Option<Self> {
        let root = std::env::var_os("KUBE_AI_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|p| p.join("ai")))
            })?;
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("manifest.json")).ok()?).ok()?;
        if manifest["model"]["protocol"] != "kube-json-v1" {
            return None;
        }
        let bundle = Self {
            model: root.join("model.gguf"),
            server: root.join("runtime/llama-server"),
        };
        (bundle.model.is_file() && bundle.server.is_file()).then_some(bundle)
    }
    pub async fn infer(&self, query: &str) -> Result<Intent, String> {
        super::check_query(query)?;
        tokio::time::timeout(Duration::from_secs(60), self.infer_inner(query))
            .await
            .map_err(|_| "Local model timed out. Rephrase or use Commands.".to_string())?
    }
    async fn infer_inner(&self, query: &str) -> Result<Intent, String> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
            .map_err(|_| "Could not reserve a local model port")?;
        let port = listener
            .local_addr()
            .map_err(|_| "Could not read local port")?
            .port();
        let mut random = [0u8; 32];
        getrandom::fill(&mut random)
            .map_err(|_| "Could not initialize local model authentication")?;
        let key: String = random.iter().map(|b| format!("{b:02x}")).collect();
        drop(listener);
        let mut child=Command::new(&self.server).args(["-m"]).arg(&self.model)
            .args(["--host","127.0.0.1","--port",&port.to_string(),"--api-key",&key,"-c","4096","-np","1","-ngl","0","--device","none","--no-op-offload","--no-kv-offload","-t","2","--no-webui","--no-warmup","--reasoning","off","--chat-template-kwargs",r#"{"enable_thinking":false}"#,"-b","256","-ub","128"])
            .env_clear().env("PATH","/usr/bin:/bin").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true)
            .spawn().map_err(|_|"Could not start the bundled local runtime. Check the AI package for your platform.")?;
        // Cold loading on slower machines shares the outer request deadline.
        // A separate short startup cutoff can reject a healthy worker before it loads.
        loop {
            if child
                .try_wait()
                .map_err(|_| "Could not check local runtime")?
                .is_some()
            {
                return Err("The bundled local runtime exited while loading.".into());
            }
            if let Ok((200, _)) = request(port, &key, "GET", "/health", String::new()).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let body = completion_body(query);
        let (status, body) = request(port, &key, "POST", "/v1/chat/completions", body).await?;
        let _ = child.kill().await;
        let _ = child.wait().await;
        if status != 200 {
            return Err("The local model could not interpret this request.".into());
        }
        let value: serde_json::Value =
            serde_json::from_slice(&body).map_err(|_| "Invalid local model response")?;
        decode(
            value["choices"][0]["message"]["content"]
                .as_str()
                .ok_or("Empty local model response")?,
            query,
        )
    }
}
fn completion_body(query: &str) -> String {
    // Property order guides constrained decoding: view must precede previous.
    let schema = include_str!("../../ai/action.schema.json");
    let messages=serde_json::json!([{"role":"system","content":include_str!("../../ai/prompt.txt")},{"role":"user","content":query}]).to_string();
    format!(
        r#"{{"messages":{messages},"temperature":0,"max_tokens":220,"response_format":{{"type":"json_schema","json_schema":{{"name":"kube_action","strict":true,"schema":{schema}}}}}}}"#
    )
}

async fn request(
    port: u16,
    key: &str,
    method: &str,
    path: &str,
    body: String,
) -> Result<(u16, Vec<u8>), String> {
    tokio::time::timeout(
        Duration::from_secs(if method == "GET" { 2 } else { 45 }),
        async {
            let stream = TcpStream::connect(("127.0.0.1", port))
                .await
                .map_err(|_| "Local model is not ready")?;
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(stream))
                    .await
                    .map_err(|_| "Local model connection failed")?;
            let connection = tokio::spawn(async move {
                let _ = connection.await;
            });
            // This task owns no process and stops when its one HTTP connection closes.
            let req = Request::builder()
                .method(method)
                .uri(path)
                .header("Host", format!("127.0.0.1:{port}"))
                .header("Authorization", format!("Bearer {key}"))
                .header("Content-Type", "application/json")
                .header("Connection", "close")
                .body(Full::new(Bytes::from(body)))
                .map_err(|_| "Invalid local request")?;
            let result = async {
                let reply = sender
                    .send_request(req)
                    .await
                    .map_err(|_| "Local inference failed")?;
                let status = reply.status().as_u16();
                let data = Limited::new(reply.into_body(), 32 * 1024)
                    .collect()
                    .await
                    .map_err(|_| "Local model response exceeded its limit")?
                    .to_bytes()
                    .to_vec();
                Ok((status, data))
            }
            .await;
            connection.abort();
            result
        },
    )
    .await
    .map_err(|_| "Local inference timed out".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_preserves_dependent_argument_order_and_escapes_user_text() {
        let body = completion_body("Open \"this\" pod's logs");
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["messages"][1]["content"], "Open \"this\" pod's logs");
        assert_eq!(parsed["response_format"]["type"], "json_schema");
        let schema = body.split("\"schema\":").nth(1).unwrap();
        assert!(schema.find("\"view\"").unwrap() < schema.find("\"previous\"").unwrap());
    }
}
