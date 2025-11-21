use anyhow::Result;
use frunk::hlist;
use futures::FutureExt;
use futures::StreamExt;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::time::sleep;

// Import from the library (assuming package name is 'rust_ws')
use rust_ws::state::{make_state, WsState, LastMsg, Heartbeat};
use rust_ws::engine::{bind_stream, run_ws_loop};
use rust_ws::app::{log_text, update_pong};

#[tokio::main]
async fn main() -> Result<()> {
    let state = make_state();

    // Stream A: Broadcast Channel
    let (tx, rx) = broadcast::channel(16);
    // Simulate external events
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(1)).await;
            let _ = tx.send("Hello".to_string());
        }
    });

    // Wrap broadcast rx in a standard Stream wrapper
    let broadcast_stream =
        tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|r| async { r.ok() }); // Convert Result<String> to String

    // Stream B: Interval (Heartbeat)
    let heartbeat_stream =
        tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(Duration::from_secs(5)));

    let stream1 = bind_stream(broadcast_stream, |_, state: &mut WsState, msg: String| {
        let last: &mut LastMsg = state.get_mut(); // Frunk getter
        println!("📢 Broadcast: {msg} (Last WS msg: {:?})", last.last_msg);
        async {}.boxed()
    });

    let stream2 = bind_stream(heartbeat_stream, |_, state: &mut WsState, _instant| {
        let hb: &mut Heartbeat = state.get_mut(); // Frunk getter
        println!("❤️ Heartbeat tick (Last pong: {:?})", hb.last_pong);
        async {}.boxed()
    });

    let handlers = hlist![
        update_pong, // Generic handler
        log_text     // Generic handler
    ];

    run_ws_loop(
        "ws://localhost:1234".to_string(),
        state,
        handlers,
        vec![stream1, stream2],
    )
    .await
}
