# Contributing to llama-dbusd

Thank you for your interest in contributing to `llama-dbusd`. This document outlines the technical standards, architectural patterns, local development environment, and submission workflow required for all contributions.

---

## 1. Welcome & Repo Philosophy

`llama-dbusd` is a lean, Free and Open Source (FOSS) community asset engineered to provide an ultra-lightweight, high-throughput D-Bus IPC bridge between Linux desktop environments and local `llama.cpp` inference instances.

Our contribution philosophy adheres to the following engineering principles:

* **Explicit Problem Solving**: We prioritize verifiable engineering solutions over speculative abstractions. Every change should solve a defined problem, improve performance, or fix a reproducible bug.
* **Structural Clarity & Minimalism**: Keep the codebase compact, legible, and maintainable. Avoid unnecessary dependencies, excessive trait layers, and unneeded indirection.
* **Minimal Conversational Fluff**: PRs and issue tickets should be direct and technical. Provide concrete reproductions, benchmarks, trace logs, and architectural rationale rather than conversational pleasantries.
* **Deterministic Behavior**: As a system-level desktop daemon, `llama-dbusd` must never deadlock, leak memory, panic, or desynchronize desktop UI shells.

---

## 2. Development Environment Workflow

Our baseline development environment is **Arch Linux**. Contributors running Arch or Arch-based distributions should use native system tooling and standard Rustup toolchains.

### 2.1 System Dependencies

Install the required build toolchain, development headers, and D-Bus runtime utilities using `pacman` or your preferred AUR helper (`paru` / `parman`):

```bash
# Using standard pacman
sudo pacman -S --needed base-devel rustup dbus pkgconf git

# Or using paru
paru -S --needed base-devel rustup dbus pkgconf git

# Or using parman
parman install base-devel rustup dbus pkgconf git
```

The test harness and daemon runtime require native `dbus` user packages installed on the host system to provide `/usr/bin/dbus-daemon` and `/usr/include/dbus-1.0`.

### 2.2 Rust Toolchain Setup

Configure the official stable toolchain via `rustup` and ensure necessary components are installed:

```bash
rustup default stable
rustup component add clippy rustfmt
```

### 2.3 Local Build & Execution

Clone the repository and compile the daemon in debug mode:

```bash
git clone https://github.com/token419/llama-dbusd.git
cd llama-dbusd

# Build debug binary
cargo build

# Run locally with structured tracing output enabled
RUST_LOG=debug cargo run
```

### 2.4 Testing Local D-Bus Operations

You can run `llama-dbusd` directly in a terminal session and query its properties using `busctl`:

```bash
# Verify daemon is registered on the user session bus
busctl --user introspect com.github.token419.LlamaDbusd /

# Inspect state properties
busctl --user get-property com.github.token419.LlamaDbusd / com.github.token419.LlamaDbusd IsOnline
busctl --user get-property com.github.token419.LlamaDbusd / com.github.token419.LlamaDbusd AvailableModels
busctl --user get-property com.github.token419.LlamaDbusd / com.github.token419.LlamaDbusd LoadedModels

# Trigger model load / unload methods
busctl --user call com.github.token419.LlamaDbusd / com.github.token419.LlamaDbusd LoadModel s "example-model"
busctl --user call com.github.token419.LlamaDbusd / com.github.token419.LlamaDbusd UnloadModel s "example-model"

# Monitor live broadcast signals
busctl --user monitor com.github.token419.LlamaDbusd
```

---

## 3. Code & Style Constraints

All contributions are subject to automated verification gates. Submissions that fail formatting or lint checks will not be merged.

### 3.1 Formatting

Code must be strictly formatted using the standard Rust formatter:

```bash
cargo fmt --check
```

Never disable or override `rustfmt` rules on individual blocks unless technically unavoidable.

### 3.2 Linting & Static Analysis

Clippy checks must complete with zero warnings across all targets:

```bash
cargo clippy --all-targets -- -D warnings
```

* Unused variables, dead code, and redundant clones must be resolved before submission.
* Avoid using `allow(...)` annotations unless addressing an intentional design decision documented with an inline comment.

### 3.3 Asynchronous Safety & Concurrency

`llama-dbusd` is built on top of `tokio` (1.0) and `zbus` (4.0). Contributors must uphold strict asynchronous safety:

1. **Thread Safety (`Send + Sync`)**: All shared state primitives, D-Bus interfaces, and background tasks must implement `Send + Sync`.
2. **No Blocking Locks Across `.await` Boundaries**: Never hold a standard library blocking lock (`std::sync::Mutex`, `std::sync::RwLock`) across an `.await` suspension point. Doing so will freeze the Tokio worker thread and cause daemon deadlocks. Use `tokio::sync::RwLock` or `tokio::sync::Mutex` for state retained across asynchronous calls:
   ```rust
   // CORRECT: Using Tokio asynchronous lock across await points
   let state = self.state.read().await;
   let is_online = state.is_online;
   drop(state);
   some_async_operation().await;
   ```
3. **Non-Blocking I/O**: Network requests to `llama-server` must utilize asynchronous HTTP calls (`reqwest::Client`) with explicit timeouts. Never perform synchronous filesystem or socket operations inside the async reactor pool.
4. **Panic-Free Error Handling**: Never call `.unwrap()` or `.expect()` in runtime daemon paths. Use `Result<T, E>` and map errors into standard `zbus::fdo::Error` or internal domain errors:
   ```rust
   // CORRECT: Return zbus-compatible error responses
   let response = client
       .get(&url)
       .send()
       .await
       .map_err(|err| zbus::fdo::Error::Failed(format!("HTTP request failed: {err}")))?;
   ```
5. **Structured Logging**: Use `tracing` macros (`error!`, `warn!`, `info!`, `debug!`, `trace!`) with structured key-value fields instead of `println!` or raw formatting strings.

---

## 4. Headless Integration Testing

Every bug fix, feature addition, or protocol adjustment **must** include asynchronous integration test coverage inside `tests/integration_tests.rs`.

### 4.1 Isolated Session Bus Architecture (`IsolatedSessionBus`)

Unit and integration tests cannot rely on or mutate the developer's personal desktop session bus, nor can they assume a system-wide D-Bus daemon is active in headless CI environments.

To ensure deterministic, hermetic execution, our integration test harness utilizes `IsolatedSessionBus`:

```rust
use std::process::{Child, Command, Stdio};
use std::io::{BufRead, BufReader};

pub struct IsolatedSessionBus {
    process: Option<Child>,
    address: String,
}
```

#### How It Operates
* `IsolatedSessionBus` spawns an isolated `dbus-daemon --session --print-address --nofork` subprocess on an ephemeral UNIX socket.
* It parses the printed bus address from `stdout` and configures client/server connections strictly against this private socket address.
* When the test harness scope completes, `IsolatedSessionBus` drops and terminates the private daemon process, leaving no leftover sockets or system state.

#### Authoring New Integration Tests
When writing a new integration test:
1. Initialize an `IsolatedSessionBus` instance.
2. Connect your test client and server interfaces using `zbus::connection::Builder::address()`.
3. Test method calls, property synchronization, and signal broadcasts within isolated scopes.

```rust
#[tokio::test]
async fn test_new_capability() {
    let bus = IsolatedSessionBus::new();
    let conn = zbus::connection::Builder::address(bus.address())
        .unwrap()
        .build()
        .await
        .unwrap();

    // Perform non-colliding async assertions here
}
```

### 4.2 Executing the Test Suite

Run the full test suite locally before pushing changes:

```bash
cargo test --verbose
```

For verbose output from integration tests specifically:

```bash
cargo test --test integration_tests -- --nocapture
```

---

## 5. Pull Requests & Releases

### 5.1 Branching & Commit Guidelines

* Base your branch on the latest `master` (or `main`) branch.
* Use clear, imperative commit messages adhering to standard conventional commit prefixes:
  * `feat: add support for dynamic polling intervals`
  * `fix: prevent race condition on model unload signal`
  * `perf: eliminate redundant string allocations in JSON parsing`
  * `test: add isolated bus coverage for HTTP connection drop`
  * `ci: cache cross-compilation toolchain binaries`

### 5.2 CI Matrix Gatekeeper

All pull requests automatically trigger the GitHub Actions workflow (`.github/workflows/ci.yml`). PRs must pass all stages before they can be merged:

1. **Gatekeeper (Lint & Test)**:
   * `cargo fmt --check`
   * `cargo clippy --all-targets -- -D warnings`
   * `cargo test --verbose` (with `libdbus-1-dev` and `dbus` running headless `IsolatedSessionBus` tests).
2. **Release Builder (Cross-Compilation Matrix)**:
   * Cross-compilation must cleanly succeed on both tier-1 Linux architectures:
     * `x86_64-unknown-linux-gnu`
     * `aarch64-unknown-linux-gnu`
   * Builds are executed via `cross build --release` inside standard containerized toolchain environments.

### 5.3 Automated Release Pipeline

Releases are triggered when a Git version tag matching `v*` (e.g., `v1.0.0`) is pushed to the repository. The release pipeline:

1. Awaits successful gatekeeper tests and cross-compilation across both `x86_64` and `aarch64` architectures.
2. Collects the compiled release binaries.
3. Packages each target into a standalone `.tar.gz` bundle containing:
   * `llama-dbusd` (executable)
   * `README.md`
   * `LICENSE`
   * `llama-dbusd.service`
4. Computes SHA256 checksums (`SHA256SUMS.txt`).
5. Publishes a draft release on GitHub with attached release assets and autogenerated release notes.
