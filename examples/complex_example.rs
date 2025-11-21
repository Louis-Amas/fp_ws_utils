use std::time::Duration;

use anyhow::Result;
use frunk::{HCons, HNil, hlist};
use rust_ws::{
    engine::{bind_stream, run_ws_loop},
    handler::to_handler,
    modules::{
        auth::{AuthState, send_auth},
        forwarder::{ForwarderState, forward_messages},
        heartbeat::{Heartbeat, update_pong},
        logging::{LastMsg, log_text},
        ping_pong::{PingState, check_timeout, handle_ping_pong},
        subscription::{SubscriptionState, send_subscriptions},
    },
    state::Conn,
    types::ConnectHandler,
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

// 1. Define the State
// This is the "God Object" replacement. It's a list of small, independent states.
pub type WsState = HCons<
    AuthState,
    HCons<
        SubscriptionState,
        HCons<
            ForwarderState<String>,
            HCons<PingState, HCons<LastMsg, HCons<Heartbeat, HCons<Conn, HNil>>>>,
        >,
    >,
>;

fn make_state(tx: mpsc::UnboundedSender<String>) -> WsState {
    hlist![
        AuthState {
            auth_message: Some("{\"action\": \"auth\", \"token\": \"secret\"}".to_string()),
        },
        SubscriptionState {
            subscriptions: vec!["{\"action\": \"sub\", \"channel\": \"ticker\"}".to_string()],
        },
        ForwarderState { sender: tx },
        PingState::default(),
        LastMsg::default(),
        Heartbeat::default(),
        Conn::default(),
    ]
}

// 2. Define a Parser for the Forwarder
// This logic extracts data from the WS message to send it to the rest of the app.
fn simple_parser(msg: &Message) -> Option<String> {
    if let Message::Text(t) = msg {
        Some(t.to_string())
    } else {
        None
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Channel to receive forwarded messages (simulating the "DataSink")
    let (tx, mut rx) = mpsc::unbounded_channel();

    // Spawn a consumer for the forwarded messages
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            println!("📤 Forwarder received: {}", msg);
        }
    });

    let state = make_state(tx);

    // --- Streams (Triggers) ---

    // 1. Timeout Stream: Checks for ping timeout every 5s
    let timeout_stream =
        tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(Duration::from_secs(5)));

    // --- Actions (Binding Streams to Logic) ---

    let action_timeout = bind_stream(timeout_stream, |ws, state: &mut WsState, _| {
        check_timeout(ws, state, ())
    });

    // --- On Connect Handlers ---
    let on_connect: Vec<ConnectHandler<WsState>> = vec![
        Box::new(|ws, state| send_auth(ws, state, ())),
        Box::new(|ws, state| send_subscriptions(ws, state, ())),
    ];

    // --- Handlers (Processing Incoming Messages) ---

    // The Chain of Responsibility
    let handlers = hlist![
        to_handler(|ws, state, msg| handle_ping_pong(ws, state, msg)),
        to_handler(|ws, state, msg| forward_messages(ws, state, msg, &simple_parser)),
        to_handler(|ws, state, msg| update_pong(ws, state, msg)),
        to_handler(|ws, state, msg| log_text(ws, state, msg))
    ];

    println!("🤖 Starting Complex Bot...");
    run_ws_loop(
        "ws://localhost:1234".to_string(),
        state,
        on_connect,
        handlers,
        vec![action_timeout],
    )
    .await
}
