use anyhow::Result;
use chrono::{DateTime, Utc};
use futures::StreamExt;
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

/// Core websocket loop where user_future_factory runs inside select!
/// and when it resolves, it immediately executes its returned closure.
async fn run_ws_loop<Data, UFut, UFutOuter>(
    url: String,
    mut state: WsState,
    on_reconnect: impl Fn(&mut WsState, &mut WsStream) + Send + Sync + 'static,
    handler: impl Fn(&mut WsState, &Message) -> Result<HandlerOutcome<Data>> + Send + Sync + 'static,
    data_sink: impl Fn(Data) + Send + Sync + 'static,
    user_future_factory: impl Fn() -> UFutOuter + Send + Sync + 'static + Clone,
) -> Result<()>
where
    Data: Send + 'static,
    UFut: std::future::Future<Output = ()> + Send + 'static,
    UFutOuter: std::future::Future<Output = Box<dyn for<'a> FnOnce(&'a mut WsState) -> UFut + Send + Sync>>
        + Send,
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
                                break;
                            }
                            HandlerOutcome::Stop => {
                                println!("🛑 Handler requested stop");
                                return Ok(());
                            }
                        },
                        Some(Err(e)) => {
                            eprintln!("WebSocket error: {e}");
                            break;
                        }
                        None => {
                            eprintln!("🔕 Stream closed by server");
                            break;
                        }
                    }
                }

                // --- User future factory resolved ---
                closure = user_future_factory() => {
                    println!("⚙️ user_future_factory resolved — executing inner user future");
                    let fut = closure(&mut state);
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
        state.last_msg = Utc::now();
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

    // Outer async factory that returns a closure
    let user_future_factory = || async {
        println!("⚙️ Outer async factory building...");
        time::sleep(Duration::from_secs(1)).await;

        Box::new(|state: &mut WsState| {
            let connected = state.connected;
            let last_msg = state.last_msg;
            async move {
                time::sleep(Duration::from_secs(1)).await;
                println!(
                    "⏰ periodic check — connected={} last_msg={}",
                    connected, last_msg
                );
            }
        }) as Box<dyn for<'a> FnOnce(&'a mut WsState) -> _ + Send + Sync>
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
