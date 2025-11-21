use std::time::Duration;

use anyhow::Result;
use frunk::{HCons, HNil, hlist};
use futures::future::BoxFuture;
use rust_ws::{
    engine::{bind_stream, run_ws_loop},
    modules::{
        auth::{AuthState, send_auth},
        forwarder::{ForwarderState, MessageParser, forward_messages},
        heartbeat::{Heartbeat, update_pong},
        logging::{LastMsg, log_text},
        ping_pong::{PingState, check_timeout, handle_ping_pong},
        subscription::{SubscriptionState, send_subscriptions},
    },
    state::Conn,
    types::{HandlerOutcome, WsStream},
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
            authenticated: false,
        },
        SubscriptionState {
            subscriptions: vec!["{\"action\": \"sub\", \"channel\": \"ticker\"}".to_string()],
            subscribed: false,
        },
        ForwarderState { sender: Some(tx) },
        PingState::default(),
        LastMsg::default(),
        Heartbeat::default(),
        Conn::default(),
    ]
}

// 2. Define a Parser for the Forwarder
// This logic extracts data from the WS message to send it to the rest of the app.
struct SimpleParser;
static PARSER: SimpleParser = SimpleParser;
impl MessageParser<String> for SimpleParser {
    fn parse(&self, msg: &Message) -> Option<String> {
        if let Message::Text(t) = msg {
            Some(t.to_string())
        } else {
            None
        }
    }
}

fn my_ping_handler<'a>(
    ws: &'a mut WsStream,
    state: &'a mut WsState,
    msg: &'a Message,
) -> BoxFuture<'a, Result<HandlerOutcome>> {
    handle_ping_pong(ws, state, msg)
}

fn my_forwarder<'a>(
    ws: &'a mut WsStream,
    state: &'a mut WsState,
    msg: &'a Message,
) -> BoxFuture<'a, Result<HandlerOutcome>> {
    forward_messages(ws, state, msg, &PARSER)
}

fn my_pong_updater<'a>(
    ws: &'a mut WsStream,
    state: &'a mut WsState,
    msg: &'a Message,
) -> BoxFuture<'a, Result<HandlerOutcome>> {
    update_pong(ws, state, msg)
}

fn my_logger<'a>(
    ws: &'a mut WsStream,
    state: &'a mut WsState,
    msg: &'a Message,
) -> BoxFuture<'a, Result<HandlerOutcome>> {
    log_text(ws, state, msg)
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

    // 1. Auth Stream: Tries to authenticate every 1s (until successful)
    let auth_stream =
        tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(Duration::from_secs(1)));

    // 2. Sub Stream: Tries to subscribe every 1s (until successful)
    let sub_stream =
        tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(Duration::from_secs(1)));

    // 3. Timeout Stream: Checks for ping timeout every 5s
    let timeout_stream =
        tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(Duration::from_secs(5)));

    // --- Actions (Binding Streams to Logic) ---

    let action_auth = bind_stream(auth_stream, |ws, state: &mut WsState, _| {
        send_auth(ws, state, ())
    });

    let action_sub = bind_stream(sub_stream, |ws, state: &mut WsState, _| {
        send_subscriptions(ws, state, ())
    });

    let action_timeout = bind_stream(timeout_stream, |ws, state: &mut WsState, _| {
        check_timeout(ws, state, ())
    });

    // --- Handlers (Processing Incoming Messages) ---

    // The Chain of Responsibility
    let handlers = hlist![
        my_ping_handler, // 1. Handle Pings automatically
        my_forwarder,    // 2. Parse and forward interesting messages
        my_pong_updater, // 3. Update pong state
        my_logger        // 4. Log everything else
    ];

    println!("🤖 Starting Complex Bot...");
    run_ws_loop(
        "ws://localhost:1234".to_string(),
        state,
        handlers,
        vec![action_auth, action_sub, action_timeout],
    )
    .await
}
