use anyhow::Result;
use chrono::{DateTime, Utc};
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt, future::BoxFuture};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{net::TcpStream, time};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

#[derive(Clone, Debug)]
struct WsState {
    connected: bool,
    last_pong: Instant,
    last_msg: DateTime<Utc>,
}

enum HandlerOutcome<Data> {
    Continue(Option<Data>),
    Reconnect,
    Stop,
}

type Handler<Data> = fn(&mut WsState, &Message) -> Result<HandlerOutcome<Data>>;
type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

// ---- HRTB-friendly types ----
type UserFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
type UserTick = dyn for<'a> FnOnce(&'a mut WsState) -> UserFuture<'a> + Send + Sync;

// ---- Named generic function for the inner async tick (HRTB-coercible) ----
fn user_tick_fn<'a>(state: &'a mut WsState) -> UserFuture<'a> {
    Box::pin(async move {
        // freely read/mutate `state` across awaits
        let before = state.connected;
        state.connected = before; // example mutation
        println!(
            "⏰ periodic check — connected={} last_msg={}",
            state.connected, state.last_msg
        );
    })
}

fn user_tick_fn_2<'a>(state: &'a mut WsState) -> UserFuture<'a> {
    Box::pin(async move {
        // freely read/mutate `state` across awaits
        let before = state.connected;
        state.connected = before; // example mutation
        println!(
            "⏰ periodic check 2 — connected={} last_msg={}",
            state.connected, state.last_msg
        );
    })
}

/// Core websocket loop; *multiple* factories are selected concurrently and executed immediately.
async fn run_ws_loop<Data>(
    url: String,
    mut state: WsState,
    on_reconnect: impl Fn(&mut WsState, &mut WsStream) + Send + Sync + 'static,
    handler: impl Fn(&mut WsState, &Message) -> Result<HandlerOutcome<Data>> + Send + Sync + 'static,
    data_sink: impl Fn(Data) + Send + Sync + 'static,

    // Vec of factories. Each factory returns a BoxFuture that yields a Box<UserTick>.
    // Use Arc so we can re-arm (clone) it after each fire.
    mut user_future_factories: Vec<
        Arc<dyn Fn() -> BoxFuture<'static, Box<UserTick>> + Send + Sync>,
    >,
) -> Result<()>
where
    Data: Send + 'static,
{
    loop {
        println!("🔌 Connecting to {url}...");
        let connect_result = connect_async(&url).await;
        let (mut stream, _) = match connect_result {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Connection failed: {e}, retrying in 3s...");
                time::sleep(Duration::from_secs(3)).await;
                continue;
            }
        };

        state.connected = true;
        on_reconnect(&mut state, &mut stream);
        println!("✅ Connected!");

        // Build a FuturesUnordered of (idx, Box<UserTick>), one pending per factory
        let mut pending: FuturesUnordered<BoxFuture<'static, (usize, Box<UserTick>)>> =
            FuturesUnordered::new();

        for (i, f) in user_future_factories.iter().enumerate() {
            let f = Arc::clone(f);
            pending.push(async move { (i, f().await) }.boxed());
        }

        loop {
            tokio::select! {
                // --- WebSocket message received ---
                msg_result = stream.next() => {
                    match msg_result {
                        Some(Ok(msg)) => match handler(&mut state, &msg)? {
                            HandlerOutcome::Continue(maybe_data) => {
                                state.last_msg = Utc::now();
                                if let Some(data) = maybe_data {
                                    data_sink(data);
                                }
                            }
                            HandlerOutcome::Reconnect => {
                                println!("🔁 Reconnection requested by handler");
                                state.connected = false;
                                break; // reconnect outer loop
                            }
                            HandlerOutcome::Stop => {
                                println!("🛑 Handler requested stop");
                                return Ok(());
                            }
                        },
                        Some(Err(e)) => {
                            eprintln!("WebSocket error: {e}");
                            break; // reconnect outer loop
                        }
                        None => {
                            eprintln!("🔕 Stream closed by server");
                            break; // reconnect outer loop
                        }
                    }
                }

                // --- Any factory resolves; run its inner user future immediately, then re-arm that factory ---
                maybe_item = pending.next() => {
                    match maybe_item {
                        Some((idx, tick_closure)) => {
                            // Execute the user tick now
                            let fut = tick_closure(&mut state);
                            fut.await;

                            // Re-arm the SAME factory by pushing a fresh future back
                            if let Some(factory) = user_future_factories.get(idx).cloned() {
                                pending.push(async move { (idx, factory().await) }.boxed());
                            }
                        }
                        None => {
                            // No more factories (empty). If you want to keep running, you can re-seed here.
                            // For now, just idle a bit to avoid a tight loop.
                            time::sleep(Duration::from_millis(50)).await;
                        }
                    }
                }
            }
        }

        state.connected = false;
        println!("⚠️ Connection lost, retrying in 3s...");
        time::sleep(Duration::from_secs(3)).await;
    }
}

fn text_logger(state: &mut WsState, msg: &Message) -> Result<HandlerOutcome<String>> {
    if let Message::Text(txt) = msg {
        println!("📜 Received text: {txt}");
        state.last_msg = Utc::now();
    }
    Ok(HandlerOutcome::Continue(None))
}

fn pong_updater(state: &mut WsState, msg: &Message) -> Result<HandlerOutcome<String>> {
    if matches!(msg, Message::Pong(_)) {
        state.last_pong = Instant::now();
    }
    Ok(HandlerOutcome::Continue(None))
}

fn reconnect_on_keyword(_state: &mut WsState, msg: &Message) -> Result<HandlerOutcome<String>> {
    if let Message::Text(txt) = msg {
        if txt.as_str() == "reconnect" {
            println!("🔁 reconnect requested via message");
            return Ok(HandlerOutcome::Reconnect);
        }
    }
    Ok(HandlerOutcome::Continue(None))
}

fn chain_handlers<Data: 'static>(
    handlers: Vec<Handler<Data>>,
) -> impl Fn(&mut WsState, &Message) -> Result<HandlerOutcome<Data>> {
    move |state: &mut WsState, msg: &Message| {
        for handler in &handlers {
            match handler(state, msg)? {
                HandlerOutcome::Continue(maybe_data) => {
                    if maybe_data.is_some() {
                        return Ok(HandlerOutcome::Continue(maybe_data));
                    }
                }
                HandlerOutcome::Reconnect => return Ok(HandlerOutcome::Reconnect),
                HandlerOutcome::Stop => return Ok(HandlerOutcome::Stop),
            }
        }
        Ok(HandlerOutcome::Continue(None))
    }
}

fn on_reconnect(_state: &mut WsState, _stream: &mut WsStream) {}

#[tokio::main]
async fn main() -> Result<()> {
    let state = WsState {
        connected: false,
        last_pong: Instant::now(),
        last_msg: Utc::now(),
    };

    let combined_handler = chain_handlers(vec![pong_updater, reconnect_on_keyword, text_logger]);

    // Example: build multiple factories with different cadences
    let f1: Arc<dyn Fn() -> BoxFuture<'static, Box<UserTick>> + Send + Sync> = Arc::new(|| {
        async {
            // outer async preparation for f1
            time::sleep(Duration::from_millis(400)).await;
            Box::new(user_tick_fn) as Box<UserTick>
        }
        .boxed()
    });

    let f2: Arc<dyn Fn() -> BoxFuture<'static, Box<UserTick>> + Send + Sync> = Arc::new(|| {
        async {
            // outer async preparation for f2
            time::sleep(Duration::from_millis(900)).await;
            Box::new(user_tick_fn_2) as Box<UserTick>
        }
        .boxed()
    });

    let factories = vec![f1, f2];

    run_ws_loop(
        "ws://localhost:1234".to_string(),
        state,
        on_reconnect,
        combined_handler,
        |msg| println!("📩 Data: {msg}"),
        factories,
    )
    .await
}
