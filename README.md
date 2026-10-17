# llama-dbusd

[![Rust Version](https://img.shields.io/badge/rustc-1.75+-orange.svg)](https://www.rust-lang.org/)
[![D-Bus Interface](https://img.shields.io/badge/D--Bus-com.github.token419.LlamaDbusd-blue.svg)](https://www.freedesktop.org/wiki/Software/dbus/)
[![Tokio](https://img.shields.io/badge/async-Tokio_1.0-blueviolet.svg)](https://tokio.rs/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![CI Matrix](https://img.shields.io/badge/targets-x86__64%20%7C%20aarch64-brightgreen.svg)](#target-architecture-matrix)

**`llama-dbusd`** is an ultra-lightweight, high-performance asynchronous system daemon written in Rust. It serves as a native IPC bridge exposing standard Session D-Bus (`com.github.token419.LlamaDbusd`) methods, properties, and signals to connect Linux desktop UI environments—such as GNOME Shell extensions, status bars, and desktop widgets—with local [`llama.cpp`](https://github.com/ggml-org/llama.cpp) inference servers (`llama-server`).

---

## Table of Contents

- [Overview & Architecture](#overview--architecture)
  - [Core Design](#core-design)
  - [State Synchronization & Concurrency](#state-synchronization--concurrency)
  - [Structured Observability](#structured-observability)
- [D-Bus Interface Specification](#d-bus-interface-specification)
  - [Service Identifiers](#service-identifiers)
  - [Properties](#properties)
  - [Methods](#methods)
  - [Signals](#signals)
- [Testing Instructions](#testing-instructions)
  - [Running the Integration Test Suite](#running-the-integration-test-suite)
  - [Isolated Headless Test Architecture](#isolated-headless-test-architecture)
- [Build & Installation](#build--installation)
  - [Compilation](#compilation)
  - [Target Architecture Matrix](#target-architecture-matrix)
  - [Binary Size & Release Profile Optimizations](#binary-size--release-profile-optimizations)
  - [Local Installation & Systemd Service](#local-installation--systemd-service)
  - [Command-Line Options](#command-line-options)
  - [Environment Configuration](#environment-configuration)
  - [Manual D-Bus Inspection with `busctl`](#manual-d-bus-inspection-with-busctl)
- [AI Assistance Attestation](#ai-assistance-attestation)
- [License](#license)

---

## Overview & Architecture

### Core Design

Desktop shells and system indicator widgets (such as GJS-based GNOME Shell extensions) require responsive, low-latency, and event-driven status updates without blocking UI rendering or spamming local HTTP ports with continuous polling loops. 

`llama-dbusd` solves this by decoupling desktop user interfaces from the HTTP REST API of `llama-server`. It runs as a background user daemon, maintaining a live poll loop against the local inference engine and pushing instantaneous state transitions over the Session D-Bus using native property change notifications and custom signals.

```
+-------------------------------------------------------------+
|                     Desktop UI / Shell                      |
|          (GNOME Shell Extension, Status Tray, etc.)         |
+-------------------------------------------------------------+
                              |
                     Session D-Bus IPC
          (com.github.token419.LlamaDbusd at path /)
                              |
+-------------------------------------------------------------+
|                        llama-dbusd                          |
|  - zbus 4.0 Interface Router                                |
|  - Arc<RwLock<DaemonState>> In-Memory Store                 |
|  - Tokio Async Runtime Task Scheduler                       |
|  - Background HTTP Polling & Sync Channel                   |
+-------------------------------------------------------------+
                              |
                       HTTP / REST API
         (GET /models, POST /v1/chat/completions, etc.)
                              |
+-------------------------------------------------------------+
|                        llama-server                         |
|                     (llama.cpp binary)                      |
+-------------------------------------------------------------+
```

### State Synchronization & Concurrency

`llama-dbusd` relies on an atomic, thread-safe in-memory cache protected by `Arc<tokio::sync::RwLock<DaemonState>>`:

```rust
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct DaemonState {
    is_online: bool,
    available_models: Vec<String>,
    loaded_models: Vec<String>,
}
```

- **Non-Blocking Concurrent Reads**: D-Bus property access queries (`IsOnline`, `AvailableModels`, `LoadedModels`) acquire asynchronous read locks, guaranteeing lock contention remains near zero even under frequent external IPC queries.
- **Atomic State Mutations**: The background polling loop checks `GET /models` on a configurable timer (default: 1,000 ms) and acquires a write lock only when diffing detected state changes.
- **Immediate Wakeup Synchronization**: Whenever a model load (`LoadModel`) or unload (`UnloadModel`) request completes, an internal unbounded Tokio MPSC channel (`mpsc::Sender<()>`) triggers an immediate state synchronization pass, bypassing the standard polling delay and firing D-Bus change signals instantly.

### Structured Observability

Logging is orchestrated through the asynchronous [`tracing`](https://docs.rs/tracing) facade and [`tracing-subscriber`](https://docs.rs/tracing-subscriber) with dynamic runtime filtering (`EnvFilter`). Every major event—including connection establishment, model status diffs, HTTP timeouts, and signal broadcasts—is emitted with structured key-value attributes (e.g., `model = %trimmed_id`, `status = %status`, `bus_name = BUS_NAME`), enabling integration into `journald` and centralized monitoring tools.

---

## D-Bus Interface Specification

### Service Identifiers

| Parameter | Value |
| :--- | :--- |
| **Well-Known Bus Name** | `com.github.token419.LlamaDbusd` |
| **Object Path** | `/` |
| **Interface Name** | `com.github.token419.LlamaDbusd` |
| **Bus Type** | Session Bus (`DBUS_BUS_SESSION`) |

### Properties

| Property Name | Type Signature | Access | Description |
| :--- | :--- | :--- | :--- |
| `ServerUrl` | `s` (string) | Read | The active `llama-server` base HTTP address configured for daemon polling and orchestration. |
| `IsOnline` | `b` (boolean) | Read | Indicates whether the backing `llama-server` HTTP instance is reachable and responding with HTTP 200 OK. |
| `AvailableModels` | `as` (array of strings) | Read | List of all model identifiers discovered and advertised by the inference server. |
| `LoadedModels` | `as` (array of strings) | Read | List of model identifiers currently resident in GPU/system memory (`status.value == "loaded"`). |

### Methods

| Method Name | Input Arguments | Return Type | Description |
| :--- | :--- | :--- | :--- |
| `LoadModel` | `model_id: s` (string) | `b` (boolean) | Triggers loading of the target model into `llama-server` memory by dispatching an initial priming chat completion request (`/v1/chat/completions`). Returns `true` on success. |
| `UnloadModel` | `model_id: s` (string) | `b` (boolean) | Issues an explicit model eviction request to `llama-server` (`/models/unload`). Returns `true` upon successful eviction. |

### Signals

| Signal Name | Payload | Trigger Condition |
| :--- | :--- | :--- |
| `ModelsChanged` | *None* | Emitted whenever `AvailableModels`, `LoadedModels`, or `IsOnline` state changes. |
| `org.freedesktop.DBus.Properties.PropertiesChanged` | `(sa{sv}as)` | Standard D-Bus property mutation signal emitted automatically for `IsOnline`, `AvailableModels`, or `LoadedModels`. |

---

## Testing Instructions

### Running the Integration Test Suite

The test suite contains both unit tests for JSON payload parsing and an end-to-end integration test framework for D-Bus methods, properties, and signal streams.

To execute the entire test suite locally:

```bash
cargo test
```

To run the integration tests with detailed logging output:

```bash
cargo test --test integration_tests -- --nocapture
```

To execute unit tests exclusively:

```bash
cargo test --lib
```

### Isolated Headless Test Architecture

Running integration tests against D-Bus traditionally introduces flakiness when tests attempt to claim well-known names on the developer's live desktop session bus or fail outright in headless continuous integration (CI) environments.

`llama-dbusd` eliminates this problem through the custom `IsolatedSessionBus` test fixture in `tests/integration_tests.rs`:

1. **Dedicated Child Bus Spawning**: Each test run orchestrates a decoupled, private `dbus-daemon` subprocess:
   ```bash
   dbus-daemon --session --print-address --nofork
   ```
2. **Dynamic Socket Negotiation**: The test harness captures the transient UNIX socket address emitted over stdout (e.g., `unix:abstract=/tmp/dbus-XXXXXXXXXX,guid=...`) and binds both the mock server and the client proxy strictly to that isolated bus address.
3. **Headless CI Execution**: The suite requires no existing desktop session, no X11/Wayland display server, and never claims the host's actual `com.github.token419.LlamaDbusd` bus name.
4. **Deterministic Teardown**: The `IsolatedSessionBus` struct implements the `Drop` trait, terminating and reaping the isolated `dbus-daemon` process immediately upon test completion.
5. **Async Signal Verification**: Using `zbus::proxy::SignalStream`, the integration suite validates that signal emissions (`ModelsChanged`) are chronologically delivered over the wire and properly caught by event listeners inside the Tokio runtime.

---

## Build & Installation

### Compilation

Build the release binary using Cargo:

```bash
cargo build --release
```

The resulting compiled binary will be placed at `target/release/llama-dbusd`.

### Target Architecture Matrix

Our continuous integration and release pipeline validates builds across the following tier-1 Linux architectures:

| Architecture Triple | Target Platform | Description |
| :--- | :--- | :--- |
| `x86_64-unknown-linux-gnu` | 64-bit x86 Linux | Modern Intel and AMD desktop, workstation, and server environments. |
| `aarch64-unknown-linux-gnu` | 64-bit ARM Linux | ARM-based Linux platforms, Single Board Computers (e.g., Raspberry Pi 5), and ARM64 workstations. |

To cross-compile for these targets using `cross` or native target toolchains:

```bash
# Add rust target toolchains
rustup target add x86_64-unknown-linux-gnu
rustup target add aarch64-unknown-linux-gnu

# Build for x86_64 Linux
cargo build --release --target x86_64-unknown-linux-gnu

# Build for aarch64 Linux
cargo build --release --target aarch64-unknown-linux-gnu
```

### Binary Size & Release Profile Optimizations

To maintain a minimal memory and storage footprint suitable for an always-on desktop background daemon, `llama-dbusd` applies aggressive compiler optimizations in `Cargo.toml`:

```toml
[profile.release]
opt-level = "z"     # Optimize strictly for minimal binary size
lto = true          # Enable Link-Time Optimization globally across all crates
codegen-units = 1   # Single code-generation unit to maximize cross-crate optimizations
panic = "abort"     # Strip unwinding landing pads to reduce binary overhead
strip = true        # Strip all symbols, debug tables, and section names from the ELF binary
```

### Local Installation & Systemd Service

1. **Install the Binary**:
   ```bash
   install -Dm755 target/release/llama-dbusd ~/.local/bin/llama-dbusd
   ```

2. **Install the Systemd User Unit**:
   ```bash
   install -Dm644 llama-dbusd.service ~/.config/systemd/user/llama-dbusd.service
   ```

3. **Enable and Start the Service**:
   ```bash
   systemctl --user daemon-reload
   systemctl --user enable --now llama-dbusd.service
   ```

4. **Verify Service Health & Logs**:
   ```bash
   systemctl --user status llama-dbusd.service
   journalctl --user -u llama-dbusd.service -f
   ```

### Command-Line Options

`llama-dbusd` provides POSIX-compliant argument parsing:

```text
Usage: llama-dbusd [OPTIONS]

An ultra-lightweight, asynchronous Rust D-Bus daemon for local llama.cpp orchestration.

Options:
  -u, --url <URL>    llama-server base URL [default: http://127.0.0.1:8080] [env: LLAMA_SERVER_URL]
  -h, --help         Print help information
```

| Flag | Long Flag | Description | Default |
| :--- | :--- | :--- | :--- |
| `-u <URL>` | `--url <URL>` | llama-server base URL (overrides `LLAMA_SERVER_URL`) | `http://127.0.0.1:8080` |
| `-h` | `--help` | Print help information and exit | |

Flag syntax supports standard POSIX and GNU conventions (e.g. `-u http://127.0.0.1:8080`, `-uhttp://127.0.0.1:8080`, `--url=http://127.0.0.1:8080`, bundled `-hu`, and `--` option delimiter).

### Environment Configuration

The daemon supports configuration via environment variables:

| Variable | Default Value | Description |
| :--- | :--- | :--- |
| `LLAMA_SERVER_URL` | `http://127.0.0.1:8080` | Base URL of the running `llama-server` instance. |
| `RUST_LOG` | `info` | Logging verbosity filter (e.g., `debug`, `llama_dbusd=trace`, `warn`). |

To override these parameters in systemd, create a drop-in override:

```bash
systemctl --user edit llama-dbusd.service
```

Add the desired environment settings:

```ini
[Service]
Environment="LLAMA_SERVER_URL=http://127.0.0.1:8080"
Environment="RUST_LOG=info,llama_dbusd=debug"
```

### Manual D-Bus Inspection with `busctl`

Verify daemon operation directly on the Session Bus using systemd's `busctl`:

```bash
# Introspect the full interface and object tree
busctl --user introspect com.github.token419.LlamaDbusd /

# Check server connectivity status
busctl --user get-property com.github.token419.LlamaDbusd / com.github.token419.LlamaDbusd IsOnline

# Read list of available models
busctl --user get-property com.github.token419.LlamaDbusd / com.github.token419.LlamaDbusd AvailableModels

# Read list of currently loaded models
busctl --user get-property com.github.token419.LlamaDbusd / com.github.token419.LlamaDbusd LoadedModels

# Request model load
busctl --user call com.github.token419.LlamaDbusd / com.github.token419.LlamaDbusd LoadModel s "unsloth/Qwen3-0.6B-GGUF:Q4_K_XL"

# Request model unload
busctl --user call com.github.token419.LlamaDbusd / com.github.token419.LlamaDbusd UnloadModel s "unsloth/Qwen3-0.6B-GGUF:Q4_K_XL"

# Monitor signals live on the bus wire
busctl --user monitor com.github.token419.LlamaDbusd
```

---

## AI Assistance Attestation

```
Built in collaboration with Google Gemini.
```

---

## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for full details.
