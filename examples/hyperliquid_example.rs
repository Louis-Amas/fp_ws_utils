use anyhow::Result;
use frunk::{HCons, HNil, hlist};
use frunk_ws::{
    engine::run_ws_loop,
    handler::to_handler,
    handlers::{
        logging::{LastMsg, log_text},
        ping_pong::{PingState, handle_ping_pong},
    },
    on_connect::subscription::{SubscriptionState, send_subscriptions},
    types::{ConnectHandler, ContextState},
};
use serde_json::json;
use tracing::info;

// Define the State
// We just need the basic connection state and logging state for this simple example
pub type WsState =
    HCons<SubscriptionState, HCons<PingState, HCons<LastMsg, HCons<ContextState, HNil>>>>;

fn make_state() -> WsState {
    let sub_msg = json!({
        "method": "subscribe",
        "subscription": {
            "type": "l2Book",
            "coin": "BTC"
        }
    })
    .to_string();

    hlist![
        SubscriptionState {
            subscriptions: vec![sub_msg]
        },
        PingState::default(),
        LastMsg::default(),
        ContextState::new("HyperliquidExample"),
    ]
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let state = make_state();

    // --- On Connect Handlers ---
    let on_connect: Vec<ConnectHandler<WsState>> =
        vec![Box::new(|ws, state| send_subscriptions(ws, state))];

    // --- Handlers (Processing Incoming Messages) ---
    let handlers = hlist![
        to_handler(|ws, state, msg| handle_ping_pong(ws, state, msg)),
        to_handler(|ws, state, msg| log_text(ws, state, msg))
    ];

    let url = "wss://api.hyperliquid.xyz/ws";
    info!("🤖 Starting Hyperliquid Example...");
    info!("Connecting to {}...", url);
    run_ws_loop(
        url.to_string(),
        state,
        on_connect,
        handlers,
        vec![], // No extra input streams for this simple example
    )
    .await
}
