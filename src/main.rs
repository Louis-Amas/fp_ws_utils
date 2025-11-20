use anyhow::Result;
use chrono::{DateTime, Utc};
use frunk::{HCons, HNil, hlist, hlist::Selector};
use futures::StreamExt;
use futures::future::{BoxFuture, FutureExt};
use futures::stream::{BoxStream, SelectAll};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio::time::sleep;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

// ---------- State structs (Unchanged) ----------
#[derive(Clone, Debug)]
pub struct LastMsg {
    pub last_msg: DateTime<Utc>,
}
#[derive(Clone, Debug)]
pub struct Heartbeat {
    pub last_pong: Instant,
}
#[derive(Clone, Debug)]
pub struct Conn {
    pub connected: bool,
}

pub type WsState = HCons<LastMsg, HCons<Heartbeat, HCons<Conn, HNil>>>;

fn make_state() -> WsState {
    hlist![
        LastMsg {
            last_msg: Utc::now()
        },
        Heartbeat {
            last_pong: Instant::now()
        },
        Conn { connected: false },
    ]
}

// ---------- Type Definitions ----------
type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

// The "Action" is a closure that takes State + Stream and does something async.
// It is generic over S so it works with ANY state struct.
type Action<S> = Box<dyn for<'a> FnOnce(&'a mut WsStream, &'a mut S) -> BoxFuture<'a, ()> + Send>;

pub enum HandlerOutcome {
    Continue,
    Reconnect,
    Stop,
}

// ---------- Composable WS Handler Trait ----------
// This allows passing hlist![fn1, fn2] directly to the loop.

pub trait WsHandler<S> {
    fn handle(&self, state: &mut S, msg: &Message) -> Result<HandlerOutcome>;
}

// 1. Implement for individual functions
impl<F, S> WsHandler<S> for F
where
    F: Fn(&mut S, &Message) -> Result<HandlerOutcome>,
{
    fn handle(&self, state: &mut S, msg: &Message) -> Result<HandlerOutcome> {
        self(state, msg)
    }
}

// 2. Implement for HList (The Chain)
impl<S, Head, Tail> WsHandler<S> for HCons<Head, Tail>
where
    Head: WsHandler<S>,
    Tail: WsHandler<S>,
{
    fn handle(&self, state: &mut S, msg: &Message) -> Result<HandlerOutcome> {
        match self.head.handle(state, msg)? {
            HandlerOutcome::Continue => self.tail.handle(state, msg),
            outcome => Ok(outcome),
        }
    }
}

impl<S> WsHandler<S> for HNil {
    fn handle(&self, _state: &mut S, _msg: &Message) -> Result<HandlerOutcome> {
        Ok(HandlerOutcome::Continue)
    }
}

// ---------- The Helper: Stream -> Action Stream ----------
// This replaces your "Trigger" trait.
// It takes ANY standard Rust Stream and binds it to a logic function.
fn bind_stream<S, M, St, F>(stream: St, logic: F) -> BoxStream<'static, Action<S>>
where
    S: 'static,
    M: Send + 'static,
    St: futures::Stream<Item = M> + Send + 'static,
    // Logic: Fn(Stream, State, Item) -> Future
    F: Fn(&mut WsStream, &mut S, M) -> BoxFuture<'static, ()> + Send + Sync + 'static,
{
    let logic = Arc::new(logic);

    stream
        .map(move |item| {
            let logic = logic.clone();
            // Create the Action closure
            let action: Action<S> = Box::new(move |ws, state| logic(ws, state, item));
            action
        })
        .boxed()
}

// ---------- The Generic Loop ----------
async fn run_ws_loop<S, H>(
    url: String,
    mut state: S,
    handler: H,
    // We just take a list of Action streams.
    // The loop doesn't care where they came from (Interval, Broadcast, Channel).
    input_streams: Vec<BoxStream<'static, Action<S>>>,
) -> Result<()>
where
    H: WsHandler<S>,
{
    // Merge all action streams into one
    let mut combined_actions: SelectAll<_> = input_streams.into_iter().collect();

    loop {
        println!("🔌 Connecting to {url}...");
        let (mut stream, _) = match connect_async(&url).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Connection failed: {e}, retrying in 2s…");
                sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        println!("✅ Connected!");

        loop {
            tokio::select! {
                // 1. WebSocket Incoming Messages (Handled by HList chain)
                maybe_msg = stream.next() => {
                    match maybe_msg {
                        Some(Ok(msg)) => match handler.handle(&mut state, &msg)? {
                            HandlerOutcome::Continue => {}
                            HandlerOutcome::Reconnect => break,
                            HandlerOutcome::Stop => return Ok(()),
                        },
                        Some(Err(e)) => { eprintln!("WS Error: {e}"); break; }
                        None => { break; }
                    }
                }
                // 2. Action Streams (Heartbeats, Broadcasts, etc.)
                Some(action) = combined_actions.next() => {
                    action(&mut stream, &mut state).await;
                }
            }
        }
        println!("⚠️ Connection lost, retrying...");
        sleep(Duration::from_secs(2)).await;
    }
}

// ---------- Application Logic Handlers ----------

fn log_text<S, I>(state: &mut S, msg: &Message) -> Result<HandlerOutcome>
where
    S: Selector<LastMsg, I>,
{
    if let Message::Text(t) = msg {
        println!("📜 Received: {}", t.as_str());
        let last: &mut LastMsg = state.get_mut();
        last.last_msg = Utc::now();
    }
    Ok(HandlerOutcome::Continue)
}

fn update_pong<S, I>(state: &mut S, msg: &Message) -> Result<HandlerOutcome>
where
    S: Selector<Heartbeat, I>,
{
    if matches!(msg, Message::Pong(_)) {
        let hb: &mut Heartbeat = state.get_mut();
        hb.last_pong = Instant::now();
    }
    Ok(HandlerOutcome::Continue)
}

// ---------- Main ----------
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
