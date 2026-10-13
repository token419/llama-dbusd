use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::{sleep, timeout};
use zbus::export::ordered_stream::OrderedStreamExt;
use zbus::object_server::SignalEmitter;
use zbus::proxy::SignalStream;
use zbus::{connection, interface, proxy};

const BUS_NAME: &str = "com.github.token419.LlamaDbusd";
const OBJECT_PATH: &str = "/";
const INTERFACE_NAME: &str = "com.github.token419.LlamaDbusd";

/// Isolated session bus runner that spawns a dedicated `dbus-daemon` instance.
/// This ensures tests run in an isolated environment without colliding with any
/// system-wide daemon, and works cleanly in headless GitHub Actions CI environments.
pub struct IsolatedSessionBus {
    process: Option<Child>,
    address: String,
}

impl Default for IsolatedSessionBus {
    fn default() -> Self {
        Self::new()
    }
}

impl IsolatedSessionBus {
    pub fn new() -> Self {
        // Attempt to spawn an isolated dbus-daemon session
        if let Ok(mut child) = Command::new("dbus-daemon")
            .arg("--session")
            .arg("--print-address")
            .arg("--nofork")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(stdout) = child.stdout.take() {
                let mut reader = BufReader::new(stdout);
                let mut address = String::new();
                if reader.read_line(&mut address).is_ok() && !address.trim().is_empty() {
                    return Self {
                        process: Some(child),
                        address: address.trim().to_string(),
                    };
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }

        // Fallback: check if DBUS_SESSION_BUS_ADDRESS is already configured
        if let Ok(addr) = std::env::var("DBUS_SESSION_BUS_ADDRESS") {
            if !addr.trim().is_empty() {
                return Self {
                    process: None,
                    address: addr,
                };
            }
        }

        panic!(
            "Failed to launch isolated dbus-daemon and DBUS_SESSION_BUS_ADDRESS is unset. \
             Please install dbus-daemon for integration testing."
        );
    }

    pub fn address(&self) -> &str {
        &self.address
    }
}

impl Drop for IsolatedSessionBus {
    fn drop(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// In-memory state of the mock daemon matching the LlamaDbusd architecture.
#[derive(Debug, Clone)]
pub struct DaemonState {
    pub is_online: bool,
    pub available_models: Vec<String>,
    pub loaded_models: Vec<String>,
    pub load_delay: Duration,
    pub unload_delay: Duration,
}

impl Default for DaemonState {
    fn default() -> Self {
        Self {
            is_online: false,
            available_models: Vec::new(),
            loaded_models: Vec::new(),
            load_delay: Duration::from_secs(1),
            unload_delay: Duration::from_millis(50),
        }
    }
}

/// Mock D-Bus daemon implementation matching `com.github.token419.LlamaDbusd`.
#[derive(Clone)]
pub struct MockLlamaDbusd {
    pub state: Arc<RwLock<DaemonState>>,
    pub handle: tokio::runtime::Handle,
}

#[interface(name = "com.github.token419.LlamaDbusd")]
impl MockLlamaDbusd {
    // --- PROPERTIES ---

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

    #[zbus(name = "LoadModel")]
    async fn load_model(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        model_id: String,
    ) -> Result<bool, zbus::fdo::Error> {
        let trimmed = model_id.trim();
        if trimmed.is_empty() {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Model identifier cannot be empty".to_string(),
            ));
        }

        let delay = self.state.read().await.load_delay;
        let state = self.state.clone();
        let model = trimmed.to_string();

        self.handle
            .spawn(async move {
                sleep(delay).await;
                let mut guard = state.write().await;
                if !guard.loaded_models.contains(&model) {
                    guard.loaded_models.push(model);
                }
            })
            .await
            .map_err(|join_err| zbus::fdo::Error::Failed(join_err.to_string()))?;

        let _ = Self::loaded_models_changed(self, &emitter).await;
        let _ = Self::models_changed(&emitter).await;
        Ok(true)
    }

    #[zbus(name = "UnloadModel")]
    async fn unload_model(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        model_id: String,
    ) -> Result<bool, zbus::fdo::Error> {
        let trimmed = model_id.trim();
        if trimmed.is_empty() {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Model identifier cannot be empty".to_string(),
            ));
        }

        let delay = self.state.read().await.unload_delay;
        let state = self.state.clone();
        let model = trimmed.to_string();

        self.handle
            .spawn(async move {
                sleep(delay).await;
                let mut guard = state.write().await;
                guard.loaded_models.retain(|m| m != &model);
            })
            .await
            .map_err(|join_err| zbus::fdo::Error::Failed(join_err.to_string()))?;

        let _ = Self::loaded_models_changed(self, &emitter).await;
        let _ = Self::models_changed(&emitter).await;
        Ok(true)
    }

    // --- SIGNALS ---

    #[zbus(signal, name = "ModelsChanged")]
    async fn models_changed(signal_emitter: &SignalEmitter<'_>) -> zbus::Result<()> {
        Ok(())
    }
}

/// D-Bus client proxy interface for interacting with `com.github.token419.LlamaDbusd`.
#[proxy(
    interface = "com.github.token419.LlamaDbusd",
    default_service = "com.github.token419.LlamaDbusd",
    default_path = "/"
)]
pub trait LlamaDbusd {
    fn load_model(&self, model: &str) -> zbus::Result<bool>;
    fn unload_model(&self, model: &str) -> zbus::Result<bool>;

    #[zbus(property)]
    fn is_online(&self) -> zbus::Result<bool>;

    #[zbus(property)]
    fn available_models(&self) -> zbus::Result<Vec<String>>;

    #[zbus(property)]
    fn loaded_models(&self) -> zbus::Result<Vec<String>>;

    #[zbus(signal)]
    fn models_changed(&self) -> zbus::Result<()>;
}

/// Test harness packaging the isolated bus runner, mock server, and client proxy.
pub struct TestHarness {
    pub _bus: IsolatedSessionBus,
    pub state: Arc<RwLock<DaemonState>>,
    pub server_conn: zbus::Connection,
    pub client_conn: zbus::Connection,
    pub proxy: LlamaDbusdProxy<'static>,
}

impl TestHarness {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Self::with_state(DaemonState::default()).await
    }

    pub async fn with_state(
        initial_state: DaemonState,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let bus = IsolatedSessionBus::new();
        let state = Arc::new(RwLock::new(initial_state));
        let handle = tokio::runtime::Handle::current();

        let service = MockLlamaDbusd {
            state: state.clone(),
            handle,
        };

        let server_conn = connection::Builder::address(bus.address())?
            .name(BUS_NAME)?
            .serve_at(OBJECT_PATH, service)?
            .build()
            .await?;

        let client_conn = connection::Builder::address(bus.address())?.build().await?;

        let proxy = LlamaDbusdProxy::builder(&client_conn)
            .destination(BUS_NAME)?
            .path(OBJECT_PATH)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await?;

        Ok(Self {
            _bus: bus,
            state,
            server_conn,
            client_conn,
            proxy,
        })
    }

    pub async fn set_available_models(
        &self,
        models: Vec<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        {
            let mut guard = self.state.write().await;
            guard.available_models = models;
        }

        let iface_ref = self
            .server_conn
            .object_server()
            .interface::<_, MockLlamaDbusd>(OBJECT_PATH)
            .await?;
        let signal_emitter = iface_ref.signal_emitter();
        let service_ref = iface_ref.get().await;

        MockLlamaDbusd::available_models_changed(&service_ref, signal_emitter).await?;
        MockLlamaDbusd::models_changed(signal_emitter).await?;
        Ok(())
    }

    pub async fn set_is_online(&self, online: bool) -> Result<(), Box<dyn std::error::Error>> {
        {
            let mut guard = self.state.write().await;
            guard.is_online = online;
        }

        let iface_ref = self
            .server_conn
            .object_server()
            .interface::<_, MockLlamaDbusd>(OBJECT_PATH)
            .await?;
        let signal_emitter = iface_ref.signal_emitter();
        let service_ref = iface_ref.get().await;

        MockLlamaDbusd::is_online_changed(&service_ref, signal_emitter).await?;
        MockLlamaDbusd::models_changed(signal_emitter).await?;
        Ok(())
    }

    pub async fn trigger_models_changed(&self) -> Result<(), Box<dyn std::error::Error>> {
        let iface_ref = self
            .server_conn
            .object_server()
            .interface::<_, MockLlamaDbusd>(OBJECT_PATH)
            .await?;
        let signal_emitter = iface_ref.signal_emitter();
        MockLlamaDbusd::models_changed(signal_emitter).await?;
        Ok(())
    }
}

#[tokio::test]
async fn test_method_load_model_success() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to initialize test harness");

    let model_to_load = "unsloth/Qwen3-0.6B-GGUF:Q4_K_XL";
    let start_time = Instant::now();

    let result = harness
        .proxy
        .load_model(model_to_load)
        .await
        .expect("Failed to invoke LoadModel");

    let elapsed = start_time.elapsed();

    assert!(result, "Expected LoadModel to return true");
    assert!(
        elapsed >= Duration::from_millis(990),
        "Expected at least 1-second delay for llama.cpp server simulation, took {:?}",
        elapsed
    );

    let loaded = harness
        .proxy
        .loaded_models()
        .await
        .expect("Failed to get LoadedModels property");
    assert_eq!(loaded, vec![model_to_load.to_string()]);
}

#[tokio::test]
async fn test_method_unload_model_success() {
    let initial_state = DaemonState {
        loaded_models: vec!["unsloth/Qwen3-0.6B-GGUF:Q4_K_XL".to_string()],
        unload_delay: Duration::from_millis(50),
        ..Default::default()
    };

    let harness = TestHarness::with_state(initial_state)
        .await
        .expect("Failed to initialize test harness");

    let model_to_unload = "unsloth/Qwen3-0.6B-GGUF:Q4_K_XL";
    let start_time = Instant::now();

    let result = harness
        .proxy
        .unload_model(model_to_unload)
        .await
        .expect("Failed to invoke UnloadModel");

    let elapsed = start_time.elapsed();

    assert!(result, "Expected UnloadModel to return true");
    assert!(
        elapsed >= Duration::from_millis(40),
        "Expected short delay execution for unload, took {:?}",
        elapsed
    );

    let loaded = harness
        .proxy
        .loaded_models()
        .await
        .expect("Failed to get LoadedModels property");
    assert!(
        loaded.is_empty(),
        "Expected LoadedModels to be empty after unload, got {:?}",
        loaded
    );
}

#[tokio::test]
async fn test_property_available_models() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to initialize test harness");

    let initial = harness
        .proxy
        .available_models()
        .await
        .expect("Failed to read initial AvailableModels");
    assert!(initial.is_empty());

    let test_models = vec![
        "qwen2.5-coder:7b".to_string(),
        "llama-3.2-3b".to_string(),
        "mistral-7b".to_string(),
    ];

    harness
        .set_available_models(test_models.clone())
        .await
        .expect("Failed to update available models");

    let updated = harness
        .proxy
        .available_models()
        .await
        .expect("Failed to read updated AvailableModels");
    assert_eq!(updated, test_models);

    let second_batch = vec!["deepseek-r1:8b".to_string(), "phi-4".to_string()];

    harness
        .set_available_models(second_batch.clone())
        .await
        .expect("Failed to mutate available models second time");

    let current = harness
        .proxy
        .available_models()
        .await
        .expect("Failed to read second batch AvailableModels");
    assert_eq!(current, second_batch);
}

#[tokio::test]
async fn test_property_is_online() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to initialize test harness");

    let initial_status = harness
        .proxy
        .is_online()
        .await
        .expect("Failed to read initial IsOnline");
    assert!(!initial_status, "Initial daemon status should be offline");

    harness
        .set_is_online(true)
        .await
        .expect("Failed to toggle daemon online");
    let online_status = harness
        .proxy
        .is_online()
        .await
        .expect("Failed to read IsOnline after toggling to true");
    assert!(online_status, "Daemon should be online");

    harness
        .set_is_online(false)
        .await
        .expect("Failed to toggle daemon offline");
    let offline_status = harness
        .proxy
        .is_online()
        .await
        .expect("Failed to read IsOnline after toggling to false");
    assert!(!offline_status, "Daemon should be offline");
}

#[tokio::test]
async fn test_property_loaded_models() {
    let initial_state = DaemonState {
        load_delay: Duration::from_millis(10),
        unload_delay: Duration::from_millis(10),
        ..Default::default()
    };

    let harness = TestHarness::with_state(initial_state)
        .await
        .expect("Failed to initialize test harness");

    let initial = harness
        .proxy
        .loaded_models()
        .await
        .expect("Failed to read initial LoadedModels");
    assert!(initial.is_empty());

    let model1 = "model-alpha:7b";
    let res1 = harness
        .proxy
        .load_model(model1)
        .await
        .expect("Failed to load model 1");
    assert!(res1);

    let loaded_after_one = harness
        .proxy
        .loaded_models()
        .await
        .expect("Failed to read LoadedModels after loading first model");
    assert_eq!(loaded_after_one, vec![model1.to_string()]);

    let model2 = "model-beta:3b";
    let res2 = harness
        .proxy
        .load_model(model2)
        .await
        .expect("Failed to load model 2");
    assert!(res2);

    let loaded_after_two = harness
        .proxy
        .loaded_models()
        .await
        .expect("Failed to read LoadedModels after loading second model");
    assert_eq!(
        loaded_after_two,
        vec![model1.to_string(), model2.to_string()]
    );

    let res_unload1 = harness
        .proxy
        .unload_model(model1)
        .await
        .expect("Failed to unload model 1");
    assert!(res_unload1);

    let loaded_after_unload = harness
        .proxy
        .loaded_models()
        .await
        .expect("Failed to read LoadedModels after unloading first model");
    assert_eq!(loaded_after_unload, vec![model2.to_string()]);

    let res_unload2 = harness
        .proxy
        .unload_model(model2)
        .await
        .expect("Failed to unload model 2");
    assert!(res_unload2);

    let final_loaded = harness
        .proxy
        .loaded_models()
        .await
        .expect("Failed to read LoadedModels after all unloads");
    assert!(final_loaded.is_empty());
}

#[tokio::test]
async fn test_signal_models_changed() {
    let harness = TestHarness::new()
        .await
        .expect("Failed to initialize test harness");

    let mut signal_stream: SignalStream = harness
        .proxy
        .inner()
        .receive_signal("ModelsChanged")
        .await
        .expect("Failed to spawn SignalStream listener for ModelsChanged");

    let trigger_time = Instant::now();

    harness
        .trigger_models_changed()
        .await
        .expect("Failed to trigger model change event");

    let signal_message = timeout(Duration::from_secs(5), signal_stream.next())
        .await
        .expect("Timed out waiting for ModelsChanged signal")
        .expect("Signal stream terminated unexpectedly");

    let interception_time = Instant::now();

    assert!(
        interception_time >= trigger_time,
        "Signal was received before event trigger chronologically"
    );

    let header = signal_message.header();
    assert_eq!(
        header.member().map(|m| m.as_str()),
        Some("ModelsChanged"),
        "Signal member name must match ModelsChanged"
    );
    assert_eq!(
        header.interface().map(|i| i.as_str()),
        Some(INTERFACE_NAME),
        "Signal interface must match com.github.token419.LlamaDbusd"
    );
}
