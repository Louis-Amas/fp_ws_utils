use anyhow::Result;
use chrono::{DateTime, Utc};
use futures::{SinkExt, StreamExt};
use std::time::{Duration, Instant};
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

async fn run_ws_loop<Data, UFut>(
    url: String,
    mut state: WsState,
    on_reconnect: impl Fn(&mut WsState, &mut WsStream) + Send + Sync + 'static,
    handler: impl Fn(&mut WsState, &Message) -> Result<HandlerOutcome<Data>> + Send + Sync + 'static,
    data_sink: impl Fn(Data) + Send + Sync + 'static,
    user_future: &mut (impl for<'a> FnMut(&'a mut WsState) -> UFut + Send + Sync),
) -> Result<()>
where
    UFut: std::future::Future<Output = ()> + Send,
    Data: Send + 'static,
{
    loop {
        println!("🔌 Connecting to {url}...");
        let connect_result = connect_async(&url).await;
        let (mut stream, _) = match connect_result {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Connection failed: {e}, retrying in 3s...");
                tokio::time::sleep(Duration::from_secs(3)).await;
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

                // --- User-specified future completed ---
                _ = user_future(&mut state) => {
                    println!("⚙️ User future triggered action");
                    // The closure can mutate state or trigger other side effects.
                    // This branch just waits again next tick.
                }
            }
        }

        state.connected = false;
        println!("⚠️ Connection lost, retrying in 3s...");
    }
}

fn text_logger(_state: &mut WsState, msg: &Message) -> Result<HandlerOutcome<String>> {
    if let Message::Text(txt) = msg {
        println!("📜 Received text: {txt}");
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
            match handler(state, &msg)? {
                HandlerOutcome::Continue(maybe_data) => {
                    // If a handler produced data, pass it along — but keep processing
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

fn on_reconnect(state: &mut WsState, stream: &mut WsStream) {}

#[tokio::main]
async fn main() -> Result<()> {
    let state = WsState {
        connected: false,
        last_pong: Instant::now(),
        last_msg: Utc::now(),
    };

    // Compose a handler chain (optional)
    let combined_handler = chain_handlers(vec![pong_updater, reconnect_on_keyword, text_logger]);

    let mut user_future = |state: &mut WsState| {
        // take a snapshot
        let connected = state.connected;
        let time = state.last_msg;
        async move {
            time::sleep(Duration::from_secs(1)).await;
            println!("⏰ periodic check — connected = {} {}", connected, time);
        }
    };

    run_ws_loop(
        "ws://localhost:1234".to_string(),
        state,
        on_reconnect,
        combined_handler,
        |msg| println!("📩 Data: {msg}"),
        &mut user_future,
    )
    .await
}
