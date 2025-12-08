use anyhow::Result;
use frunk::{HCons, HNil, hlist};
// Import from the library (assuming package name is 'rust_ws')
use frunk_ws::engine::{bind_stream, run_ws_loop};
use frunk_ws::{
    handlers::{
        heartbeat::{Heartbeat, update_pong},
        logging::{LastMsg, log_text},
    },
    types::{ContextState, HandlerOutcome},
};
use futures::{FutureExt, SinkExt, StreamExt};
use std::time::Duration;
use tokio::{sync::broadcast, time::sleep};
use tokio_tungstenite::tungstenite::Message;
use tracing::info;

// Define WsState here in main
pub type WsState = HCons<LastMsg, HCons<Heartbeat, HCons<ContextState, HNil>>>;

fn make_state() -> WsState {
    hlist![
        LastMsg::default(),
        Heartbeat::default(),
        ContextState::new("SimpleExample")
    ]
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let state = make_state();

    // Stream A: Broadcast Channel
    let (tx, rx) = broadcast::channel(16);
    // Simulate external events
    tokio::spawn(async move {
        let mut i = 0;

        loop {
            sleep(Duration::from_secs(1)).await;
            let _ = tx.send(format!("Hello {i}"));
            i += 1;
        }
    });

    // Wrap broadcast rx in a standard Stream wrapper
    let broadcast_stream =
        tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|r| async { r.ok() }); // Convert Result<String> to String

    // Stream B: Interval (Heartbeat)
    let heartbeat_stream =
        tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(Duration::from_secs(5)));

    let stream1 = bind_stream(broadcast_stream, |ws, state: &mut WsState, msg: String| {
        let last: &mut LastMsg = state.get_mut(); // Frunk getter
        info!("📢 Broadcast: {msg} (Last WS msg: {:?})", last.last_msg);
        async move {
            let _ = ws.send(Message::Text(msg)).await;
            HandlerOutcome::Continue
        }
        .boxed()
    });

    let stream2 = bind_stream(heartbeat_stream, |_, state: &mut WsState, _instant| {
        let hb: &mut Heartbeat = state.get_mut(); // Frunk getter
        info!("❤️ Heartbeat tick (Last pong: {:?})", hb.last_pong);
        async { HandlerOutcome::Continue }.boxed()
    });

    let handlers = hlist![
        update_pong, // Generic handler
        log_text     // Generic handler
    ];

    run_ws_loop(
        "ws://localhost:1234".to_string(),
        state,
        vec![],
        handlers,
        vec![stream1, stream2],
    )
    .await
}
