use anyhow::Result;
use frunk::{HCons, HNil, hlist};
use frunk_ws::{
    engine::{bind_stream, run_ws_loop},
    handler::to_handler,
    handlers::{
        forwarder::{ForwarderState, forward_messages},
        heartbeat::{Heartbeat, update_pong},
        logging::{LastMsg, check_last_msg_timeout, log_text},
        ping_pong::{PingState, check_timeout, handle_ping_pong},
    },
    on_connect::subscription::{SubscriptionState, send_subscriptions},
    types::{ConnectHandler, ContextState},
};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::info;

// 1. Define the State
// This is the "God Object" replacement. It's a list of small, independent states.
pub type WsState = HCons<
    SubscriptionState,
    HCons<
        ForwarderState<String>,
        HCons<PingState, HCons<LastMsg, HCons<Heartbeat, HCons<ContextState, HNil>>>>,
    >,
>;

fn make_state(tx: mpsc::UnboundedSender<String>) -> WsState {
    hlist![
        SubscriptionState {
            subscriptions: vec!["{\"action\": \"sub\", \"channel\": \"ticker\"}".to_string()],
        },
        ForwarderState { sender: tx },
        PingState::default(),
        LastMsg::default(),
        Heartbeat::default(),
        ContextState::new("ComplexExample"),
    ]
}

// 2. Define a Parser for the Forwarder
// This logic extracts data from the WS message to send it to the rest of the app.
fn simple_parser(msg: &Message) -> Result<Option<String>> {
    if let Message::Text(t) = msg {
        Ok(Some(t.to_string()))
    } else {
        Ok(None)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // Channel to receive forwarded messages (simulating the "DataSink")
    let (tx, mut rx) = mpsc::unbounded_channel();

    // Spawn a consumer for the forwarded messages
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            info!("📤 Forwarder received: {}", msg);
        }
    });

    let state = make_state(tx);

    // --- Streams (Triggers) ---

    // 1. Timeout Stream: Checks for ping timeout every 5s
    let timeout_stream =
        tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(Duration::from_secs(5)));

    // 2. Watchdog Stream: Checks for last message timeout every 5s
    let watchdog_stream =
        tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(Duration::from_secs(5)));

    // --- Actions (Binding Streams to Logic) ---

    let action_timeout = bind_stream(timeout_stream, |ws, state: &mut WsState, _| {
        check_timeout(ws, state)
    });

    let action_watchdog = bind_stream(watchdog_stream, |ws, state: &mut WsState, _| {
        check_last_msg_timeout(ws, state, Duration::from_secs(30))
    });

    // --- On Connect Handlers ---
    let on_connect: Vec<ConnectHandler<WsState>> =
        vec![Box::new(|ws, state| send_subscriptions(ws, state))];

    // --- Handlers (Processing Incoming Messages) ---

    // The Chain of Responsibility
    let handlers = hlist![
        to_handler(|ws, state, msg| handle_ping_pong(ws, state, msg)),
        to_handler(|ws, state, msg| {
            forward_messages(ws, state, msg, &simple_parser, |_| false)
        }),
        to_handler(|ws, state, msg| update_pong(ws, state, msg)),
        to_handler(|ws, state, msg| log_text(ws, state, msg))
    ];

    info!("🤖 Starting Complex Bot...");
    run_ws_loop(
        "ws://localhost:1234".to_string(),
        state,
        on_connect,
        handlers,
        vec![action_timeout, action_watchdog],
    )
    .await
}
