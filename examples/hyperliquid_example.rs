use anyhow::Result;
use frunk::{HCons, HNil, hlist};
use rust_ws::{
    engine::run_ws_loop,
    handler::to_handler,
    modules::{
        logging::{LastMsg, log_text},
        ping_pong::{PingState, handle_ping_pong},
        subscription::{SubscriptionState, send_subscriptions},
    },
    state::Conn,
    types::ConnectHandler,
};
use serde_json::json;

// Define the State
// We just need the basic connection state and logging state for this simple example
pub type WsState = HCons<SubscriptionState, HCons<PingState, HCons<LastMsg, HCons<Conn, HNil>>>>;

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
        Conn::default()
    ]
}

#[tokio::main]
async fn main() -> Result<()> {
    let state = make_state();

    // Hyperliquid Mainnet WebSocket URL
    let url = "wss://api.hyperliquid.xyz/ws".to_string();

    // On Connect: Subscribe to BTC L2 Book (Public Data)
    let on_connect: Vec<ConnectHandler<WsState>> =
        vec![Box::new(|ws, state| send_subscriptions(ws, state))];

    // Handlers
    // 1. Standard Ping/Pong (Protocol level)
    // 2. Log everything else (Application level)
    let handlers = hlist![
        to_handler(|ws, state, msg| handle_ping_pong(ws, state, msg)),
        to_handler(|ws, state, msg| log_text(ws, state, msg))
    ];

    println!("🤖 Starting Hyperliquid Example...");
    println!("Connecting to {}...", url);

    run_ws_loop(url, state, on_connect, handlers, vec![]).await
}
