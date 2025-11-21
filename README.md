# Rust WebSocket Utils (`rust_ws`)

A modular, type-safe, and composable WebSocket framework for Rust, built on top of `tokio-tungstenite` and `frunk`.

## 🚀 Features

- **Composable State Management**: Uses `frunk`'s `HList` to manage application state without a monolithic "God Object". Each module manages its own piece of state independently.
- **Chain of Responsibility Handlers**: Process incoming WebSocket messages through a chain of independent handlers.
- **Stream-Based Triggers**: Bind arbitrary `Stream`s (like timers or channels) to actions that interact with the WebSocket and state.
- **On-Connect Actions**: Define actions to run immediately upon connection (e.g., authentication, subscription).
- **Async/Await**: Fully asynchronous, built on `tokio`.

## 📦 Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
rust_ws = { path = "." } # Or git repository
tokio = { version = "1.45", features = ["full"] }
frunk = "0.4"
anyhow = "1.0"
```

## 🛠️ Usage

### 1. Define Your State

Compose your application state using `HList`. Each component (Auth, Subscription, etc.) defines its own struct.

```rust
use frunk::{HCons, HNil, hlist};
use rust_ws::on_connect::auth::AuthState;
use rust_ws::on_connect::subscription::SubscriptionState;

pub type WsState = HCons<AuthState, HCons<SubscriptionState, HNil>>;

fn make_state() -> WsState {
    hlist![
        AuthState { ... },
        SubscriptionState { ... },
    ]
}
```

### 2. Define Handlers

Handlers process incoming messages. They are composed into a list.

```rust
use rust_ws::handler::to_handler;
use rust_ws::handlers::logging::log_text;

let handlers = hlist![
    to_handler(|ws, state, msg| log_text(ws, state, msg)),
    // Add more handlers here...
];
```

### 3. Run the Loop

Use `run_ws_loop` to start the engine.

```rust
use rust_ws::engine::run_ws_loop;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let state = make_state();
    let handlers = hlist![ ... ];
    let on_connect = vec![ ... ];
    
    run_ws_loop(
        "wss://api.example.com/ws".to_string(),
        state,
        on_connect,
        handlers,
        vec![] // triggers
    ).await
}
```

## 📂 Project Structure

- `src/engine.rs`: Core WebSocket loop and connection logic.
- `src/handlers/`: Modules for processing incoming messages (e.g., `ping_pong`, `logging`).
- `src/on_connect/`: Modules for actions to take upon connection (e.g., `auth`, `subscription`).
- `src/state.rs`: State management utilities.
- `examples/`: Working examples.

## 💡 Examples

Check out the `examples/` directory for complete working code:

- **Hyperliquid Example**: Connects to a real crypto exchange WebSocket, subscribes to data, and handles ping/pong.
  ```bash
  cargo run --example hyperliquid_example
  ```

- **Complex Example**: Demonstrates advanced usage with multiple state components, forwarders, and timers.
  ```bash
  cargo run --example complex_example
  ```
