use serde::Serialize;
use std::error::Error;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use zbus::object_server::SignalEmitter;
use zbus::{connection, interface};

mod cli;

const BUS_NAME: &str = "com.github.token419.LlamaDbusd";
const OBJECT_PATH: &str = "/";
const POLL_INTERVAL_MS: u64 = 1000;
const HTTP_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct DaemonState {
    is_online: bool,
    available_models: Vec<String>,
    loaded_models: Vec<String>,
}

#[derive(Debug, Serialize)]
struct UnloadPayload {
    model: String,
}

#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct LoadPayload {
    model: String,
    messages: Vec<Message>,
    max_tokens: u32,
}

#[derive(Clone)]
struct LlamaDbusd {
    state: Arc<RwLock<DaemonState>>,
    client: reqwest::Client,
    server_url: String,
    sync_tx: mpsc::Sender<()>,
    handle: tokio::runtime::Handle,
}

#[interface(name = "com.github.token419.LlamaDbusd")]
impl LlamaDbusd {
    // --- PROPERTIES ---

    #[zbus(property, name = "ServerUrl")]
    async fn server_url(&self) -> String {
        self.server_url.clone()
    }

    #[zbus(property, name = "IsOnline")]
    async fn is_online(&self) -> bool {
        self.state.read().await.is_online
    }

    #[zbus(property, name = "AvailableModels")]
    async fn available_models(&self) -> Vec<String> {
        self.state.read().await.available_models.clone()
    }

    #[zbus(property, name = "LoadedModels")]
    async fn loaded_models(&self) -> Vec<String> {
        self.state.read().await.loaded_models.clone()
    }

    // --- METHODS ---

    #[zbus(name = "UnloadModel")]
    async fn unload_model(&self, model_id: String) -> Result<bool, zbus::fdo::Error> {
        let trimmed_id = model_id.trim();
        if trimmed_id.is_empty() {
            tracing::warn!("UnloadModel called with empty model identifier");
            return Err(zbus::fdo::Error::InvalidArgs(
                "Model identifier cannot be empty".to_string(),
            ));
        }

        tracing::info!(model = %trimmed_id, "Processing UnloadModel request");

        let url = format!("{}/models/unload", self.server_url.trim_end_matches('/'));
        let payload = UnloadPayload {
            model: trimmed_id.to_string(),
        };

        let client = self.client.clone();
        let server_url = self.server_url.clone();
        let model = trimmed_id.to_string();
        let sync_tx = self.sync_tx.clone();

        self.handle
            .spawn(async move {
                let response = client
                    .post(&url)
                    .json(&payload)
                    .send()
                    .await
                    .map_err(|err| {
                        tracing::error!(model = %model, error = %err, "HTTP request to unload model failed");
                        if err.is_timeout() {
                            zbus::fdo::Error::Timeout(format!("Request to llama-server timed out: {err}"))
                        } else if err.is_connect() {
                            zbus::fdo::Error::IOError(format!(
                                "Failed to connect to llama-server at {server_url}: {err}"
                            ))
                        } else {
                            zbus::fdo::Error::Failed(format!("HTTP unload request failed: {err}"))
                        }
                    })?;

                let status = response.status();
                if !status.is_success() {
                    let error_text = response
                        .text()
                        .await
                        .unwrap_or_else(|_| "Unknown error response from server".to_string());

                    tracing::error!(
                        model = %model,
                        status = %status,
                        response = %error_text,
                        "llama-server rejected unload request"
                    );

                    return match status {
                        reqwest::StatusCode::NOT_FOUND => Err(zbus::fdo::Error::FileNotFound(format!(
                            "Model '{model}' not found: {error_text}"
                        ))),
                        reqwest::StatusCode::BAD_REQUEST => Err(zbus::fdo::Error::InvalidArgs(format!(
                            "Invalid unload request for model '{model}': {error_text}"
                        ))),
                        _ => Err(zbus::fdo::Error::Failed(format!(
                            "llama-server returned HTTP {status}: {error_text}"
                        ))),
                    };
                }

                tracing::info!(model = %model, "Successfully unloaded model");
                let _ = sync_tx.send(()).await;
                Ok(true)
            })
            .await
            .map_err(|join_err| {
                tracing::error!(error = %join_err, "Tokio task join failed during UnloadModel");
                zbus::fdo::Error::Failed(format!("Internal task execution failed: {join_err}"))
            })?
    }

    #[zbus(name = "LoadModel")]
    async fn load_model(&self, model_id: String) -> Result<bool, zbus::fdo::Error> {
        let trimmed_id = model_id.trim();
        if trimmed_id.is_empty() {
            tracing::warn!("LoadModel called with empty model identifier");
            return Err(zbus::fdo::Error::InvalidArgs(
                "Model identifier cannot be empty".to_string(),
            ));
        }

        tracing::info!(model = %trimmed_id, "Processing LoadModel request");

        let url = format!(
            "{}/v1/chat/completions",
            self.server_url.trim_end_matches('/')
        );
        let payload = LoadPayload {
            model: trimmed_id.to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: "ping".to_string(),
            }],
            max_tokens: 1,
        };

        let client = self.client.clone();
        let server_url = self.server_url.clone();
        let model = trimmed_id.to_string();
        let sync_tx = self.sync_tx.clone();

        self.handle
            .spawn(async move {
                let response = client
                    .post(&url)
                    .json(&payload)
                    .send()
                    .await
                    .map_err(|err| {
                        tracing::error!(model = %model, error = %err, "HTTP request to load model failed");
                        if err.is_timeout() {
                            zbus::fdo::Error::Timeout(format!("Request to llama-server timed out: {err}"))
                        } else if err.is_connect() {
                            zbus::fdo::Error::IOError(format!(
                                "Failed to connect to llama-server at {server_url}: {err}"
                            ))
                        } else {
                            zbus::fdo::Error::Failed(format!("HTTP load request failed: {err}"))
                        }
                    })?;

                let status = response.status();
                if !status.is_success() {
                    let error_text = response
                        .text()
                        .await
                        .unwrap_or_else(|_| "Unknown error response from server".to_string());

                    tracing::error!(
                        model = %model,
                        status = %status,
                        response = %error_text,
                        "llama-server rejected load request"
                    );

                    return match status {
                        reqwest::StatusCode::NOT_FOUND => Err(zbus::fdo::Error::FileNotFound(format!(
                            "Model '{model}' not found: {error_text}"
                        ))),
                        reqwest::StatusCode::BAD_REQUEST => Err(zbus::fdo::Error::InvalidArgs(format!(
                            "Invalid load request for model '{model}': {error_text}"
                        ))),
                        _ => Err(zbus::fdo::Error::Failed(format!(
                            "llama-server returned HTTP {status}: {error_text}"
                        ))),
                    };
                }

                tracing::info!(model = %model, "Successfully loaded model");
                let _ = sync_tx.send(()).await;
                Ok(true)
            })
            .await
            .map_err(|join_err| {
                tracing::error!(error = %join_err, "Tokio task join failed during LoadModel");
                zbus::fdo::Error::Failed(format!("Internal task execution failed: {join_err}"))
            })?
    }

    // --- SIGNALS ---

    #[zbus(signal, name = "ModelsChanged")]
    async fn models_changed(signal_emitter: &SignalEmitter<'_>) -> zbus::Result<()> {
        Ok(())
    }
}

fn parse_models_json(json_val: &serde_json::Value) -> (Vec<String>, Vec<String>) {
    let mut available = Vec::new();
    let mut loaded = Vec::new();

    let items = json_val
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| json_val.as_array());

    if let Some(items) = items {
        for item in items {
            if let Some(id) = item.get("id").and_then(|i| i.as_str()) {
                available.push(id.to_string());

                let is_loaded = item
                    .get("status")
                    .and_then(|s| s.get("value"))
                    .and_then(|v| v.as_str())
                    .map(|v| v == "loaded")
                    .unwrap_or(false);

                if is_loaded {
                    loaded.push(id.to_string());
                }
            }
        }
    }

    (available, loaded)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let action = match cli::parse_args(std::env::args().skip(1)) {
        Ok(action) => action,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(2);
        }
    };

    let cli_config = match action {
        cli::CliAction::Help => {
            print!("{}", cli::help_message());
            std::process::exit(0);
        }
        cli::CliAction::Run(config) => config,
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("Starting llama-dbusd daemon...");

    let server_url = cli_config.server_url;

    let state = Arc::new(RwLock::new(DaemonState::default()));
    let (sync_tx, mut sync_rx) = mpsc::channel::<()>(16);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()?;

    let handle = tokio::runtime::Handle::current();

    let llama_service = LlamaDbusd {
        state: Arc::clone(&state),
        client: client.clone(),
        server_url: server_url.clone(),
        sync_tx: sync_tx.clone(),
        handle,
    };

    let conn = connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, llama_service)?
        .build()
        .await?;

    tracing::info!(
        bus_name = BUS_NAME,
        object_path = OBJECT_PATH,
        server_url = %server_url,
        "D-Bus service registered and listening"
    );

    let object_server = conn.object_server();
    let interface_ref = object_server
        .interface::<_, LlamaDbusd>(OBJECT_PATH)
        .await?;
    let signal_emitter = interface_ref.signal_emitter().clone();

    let state_clone = Arc::clone(&state);
    let poll_url = format!("{}/models", server_url.trim_end_matches('/'));

    tokio::spawn(async move {
        loop {
            let response = client.get(&poll_url).send().await;

            let (next_online, next_available, next_loaded) = match response {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<serde_json::Value>().await {
                        Ok(json_val) => {
                            let (avail, loaded) = parse_models_json(&json_val);
                            (true, avail, loaded)
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "Failed to parse JSON response from /models");
                            (true, Vec::new(), Vec::new())
                        }
                    }
                }
                Ok(resp) => {
                    tracing::debug!(status = %resp.status(), "llama-server /models endpoint returned non-success");
                    (false, Vec::new(), Vec::new())
                }
                Err(err) => {
                    tracing::trace!(error = %err, "Unable to reach llama-server /models endpoint");
                    (false, Vec::new(), Vec::new())
                }
            };

            let available_count = next_available.len();
            let loaded_count = next_loaded.len();

            let (changed_online, changed_available, changed_loaded) = {
                let mut state_guard = state_clone.write().await;
                let changed_online = state_guard.is_online != next_online;
                let changed_available = state_guard.available_models != next_available;
                let changed_loaded = state_guard.loaded_models != next_loaded;

                if changed_online || changed_available || changed_loaded {
                    state_guard.is_online = next_online;
                    state_guard.available_models = next_available;
                    state_guard.loaded_models = next_loaded;
                }

                (changed_online, changed_available, changed_loaded)
            };

            if changed_online || changed_available || changed_loaded {
                tracing::info!(
                    online = next_online,
                    available_count,
                    loaded_count,
                    "Model state changed; emitting D-Bus signals"
                );

                let service_ref = interface_ref.get().await;

                if changed_online {
                    if let Err(err) =
                        LlamaDbusd::is_online_changed(&service_ref, &signal_emitter).await
                    {
                        tracing::warn!(error = %err, "Failed to emit IsOnline property change signal");
                    }
                }
                if changed_available {
                    if let Err(err) =
                        LlamaDbusd::available_models_changed(&service_ref, &signal_emitter).await
                    {
                        tracing::warn!(error = %err, "Failed to emit AvailableModels property change signal");
                    }
                }
                if changed_loaded {
                    if let Err(err) =
                        LlamaDbusd::loaded_models_changed(&service_ref, &signal_emitter).await
                    {
                        tracing::warn!(error = %err, "Failed to emit LoadedModels property change signal");
                    }
                }

                if let Err(err) = LlamaDbusd::models_changed(&signal_emitter).await {
                    tracing::warn!(error = %err, "Failed to emit ModelsChanged signal");
                }
            }

            tokio::select! {
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(POLL_INTERVAL_MS)) => {},
                _ = sync_rx.recv() => {
                    tracing::debug!("Synchronization wake-up event received");
                },
            }
        }
    });

    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutdown signal received, terminating llama-dbusd daemon");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_models_json_data_object() {
        let sample = serde_json::json!({
            "data": [
                {
                    "id": "qwen2.5-coder:7b",
                    "status": { "value": "loaded" }
                },
                {
                    "id": "llama-3.2-3b",
                    "status": { "value": "unloaded" }
                },
                {
                    "id": "mistral-7b"
                }
            ]
        });

        let (available, loaded) = parse_models_json(&sample);
        assert_eq!(
            available,
            vec!["qwen2.5-coder:7b", "llama-3.2-3b", "mistral-7b"]
        );
        assert_eq!(loaded, vec!["qwen2.5-coder:7b"]);
    }

    #[test]
    fn test_parse_models_json_bare_array() {
        let sample = serde_json::json!([
            {
                "id": "model-alpha",
                "status": { "value": "loaded" }
            },
            {
                "id": "model-beta"
            }
        ]);

        let (available, loaded) = parse_models_json(&sample);
        assert_eq!(available, vec!["model-alpha", "model-beta"]);
        assert_eq!(loaded, vec!["model-alpha"]);
    }

    #[test]
    fn test_parse_models_json_empty_or_invalid() {
        let sample = serde_json::json!({});
        let (available, loaded) = parse_models_json(&sample);
        assert!(available.is_empty());
        assert!(loaded.is_empty());
    }
}
