use anyhow::Result;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use std::{
    future::Future,
    pin::Pin,
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

// ---- KEY: a named generic function is HRTB-coercible; closures are not ----
fn user_tick_fn<'a>(state: &'a mut WsState) -> UserFuture<'a> {
    Box::pin(async move {
        // freely read/mutate `state` across awaits
        let before = state.connected;
        time::sleep(Duration::from_secs(1)).await;
        state.connected = before; // example mutation
        println!(
            "⏰ periodic check — connected={} last_msg={}",
            state.connected, state.last_msg
        );
    })
}

/// Core websocket loop; factory is selected concurrently and executed immediately.
async fn run_ws_loop<Data, UFutOuter>(
    url: String,
    mut state: WsState,
    on_reconnect: impl Fn(&mut WsState, &mut WsStream) + Send + Sync + 'static,
    handler: impl Fn(&mut WsState, &Message) -> Result<HandlerOutcome<Data>> + Send + Sync + 'static,
    data_sink: impl Fn(Data) + Send + Sync + 'static,
    user_future_factory: impl Fn() -> UFutOuter + Send + Sync + 'static + Clone,
) -> Result<()>
where
    Data: Send + 'static,
    UFutOuter: Future<Output = Box<UserTick>> + Send + 'static,
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
                                break; // reconnect
                            }
                            HandlerOutcome::Stop => {
                                println!("🛑 Handler requested stop");
                                return Ok(());
                            }
                        },
                        Some(Err(e)) => {
                            eprintln!("WebSocket error: {e}");
                            break; // reconnect
                        }
                        None => {
                            eprintln!("🔕 Stream closed by server");
                            break; // reconnect
                        }
                    }
                }

                // --- Factory resolves inside select; immediately run the inner async with &mut state ---
                tick_closure = user_future_factory() => {
                    let fut = tick_closure(&mut state);
                    fut.await;
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

    let user_future_factory = || async {
        println!("⚙️ Outer async factory building...");
        time::sleep(Duration::from_secs(1)).await;

        Box::new(user_tick_fn) as Box<UserTick>
    };

    run_ws_loop(
        "ws://localhost:1234".to_string(),
        state,
        on_reconnect,
        combined_handler,
        |msg| println!("📩 Data: {msg}"),
        user_future_factory,
    )
    .await
}

