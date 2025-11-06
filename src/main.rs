use anyhow::Result;
use chrono::{DateTime, Utc};
use frunk::hlist::Selector;
use frunk::{HCons, HNil, hlist};
use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{Notify, broadcast, watch};
use tokio::time::sleep;
use tokio::{net::TcpStream, time};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message}; // brings `get` / `get_mut` into scope

#[derive(Clone, Debug)]
pub struct LastMsg {
    pub last_msg: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct Heartbeat {
    pub last_pong: Instant,
    pub last_msg: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct Conn {
    pub connected: bool,
}

// If you add more components, they just become more HList elements.
pub type WsState = HCons<LastMsg, HCons<Heartbeat, HCons<Conn, HNil>>>;

// Helper to build initial HList state
fn make_state() -> WsState {
    hlist![
        LastMsg {
            last_msg: Utc::now(),
        },
        Heartbeat {
            last_pong: Instant::now(),
            last_msg: Utc::now()
        },
        Conn { connected: false },
    ]
}

enum HandlerOutcome<Data> {
    Continue(Option<Data>),
    Reconnect,
    Stop,
}

#[derive(Debug, Clone)]
enum Msg {
    TestA,
    TestB,
}

impl Default for Msg {
    fn default() -> Self {
        Self::TestA
    }
}

type Handler<S, Data> = fn(&mut S, &Message) -> Result<HandlerOutcome<Data>>;
type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

type TickFn<S, M> = for<'a> fn(&'a mut WsStream, &'a mut S, M) -> UserFuture<'a>;
type Factory<S, M> = Arc<dyn Fn() -> BoxFuture<'static, (TickFn<S, M>, M)> + Send + Sync>;

// ---- HRTB- types for user ticks ----
type UserFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

// Adapter macro for async fn(&mut WsState) -> impl Future<Output=()>
#[macro_export]
macro_rules! user_async_adapter {
    ($f:path) => {{
        fn __adapter<'a>(s: &'a mut WsState) -> UserFuture<'a> {
            Box::pin($f(s))
        }
        __adapter
    }};
}

// ---------- Triggers ----------
trait Trigger<M: Default>: Send + Sync {
    fn arm(&self) -> BoxFuture<'static, M>;
}
type Trig<M: Default> = Arc<dyn Trigger<M>>;

// Fires every fixed interval
struct IntervalTrigger {
    period: Duration,
}
impl<M: Default> Trigger<M> for IntervalTrigger {
    fn arm(&self) -> BoxFuture<'static, M> {
        let d = self.period;
        async move {
            sleep(d).await;

            M::default()
        }
        .boxed()
    }
}

// Fires when someone calls `notify.notify_one()` or `notify_waiters()`
struct NotifyTrigger {
    notify: Arc<Notify>,
}
impl<M: Default> Trigger<M> for NotifyTrigger {
    fn arm(&self) -> BoxFuture<'static, M> {
        let n = self.notify.clone();
        async move {
            n.notified().await;

            M::default()
        }
        .boxed()
    }
}

// Fires when a broadcast message is published (each arming gets its own Rx)
struct BroadcastTrigger<T: Clone + Send + 'static> {
    tx: broadcast::Sender<T>,
}
impl<M: Clone + Send + 'static + Default> Trigger<M> for BroadcastTrigger<M> {
    fn arm(&self) -> BoxFuture<'static, M> {
        let mut rx = self.tx.subscribe();
        async move {
            let m = rx.recv().await.unwrap();
            m
        }
        .boxed()
    }
}

fn make_factory<S: 'static, M: 'static + Default>(
    trig: Trig<M>,
    tick: TickFn<S, M>,
) -> Factory<S, M> {
    Arc::new(move || {
        let trig = trig.clone();
        async move {
            let msg = trig.arm().await;
            (tick, msg)
        }
        .boxed()
    })
}

// ---------- Handler chaining ----------
fn chain_handlers<S: 'static, Data: 'static>(
    handlers: Vec<Handler<S, Data>>,
) -> impl Fn(&mut S, &Message) -> Result<HandlerOutcome<Data>> {
    move |state: &mut S, msg: &Message| {
        for handler in &handlers {
            match handler(state, msg)? {
                HandlerOutcome::Continue(maybe) => {
                    if maybe.is_some() {
                        return Ok(HandlerOutcome::Continue(maybe));
                    }
                }
                HandlerOutcome::Reconnect => return Ok(HandlerOutcome::Reconnect),
                HandlerOutcome::Stop => return Ok(HandlerOutcome::Stop),
            }
        }
        Ok(HandlerOutcome::Continue(None))
    }
}

// ---------- Example handlers ----------
fn text_logger<S, I>(state: &mut S, msg: &Message) -> Result<HandlerOutcome<String>>
where
    S: Selector<LastMsg, I>,
{
    if let Message::Text(txt) = msg {
        println!("📜 Received text: {txt}");
        let last: &mut LastMsg = state.get_mut();
        last.last_msg = Utc::now();
    }
    Ok(HandlerOutcome::Continue(None))
}

fn pong_updater<S, I>(state: &mut S, msg: &Message) -> Result<HandlerOutcome<String>>
where
    S: Selector<Heartbeat, I>,
{
    if matches!(msg, Message::Pong(_)) {
        // Borrow the Heartbeat substate by its type
        let hb: &mut Heartbeat = state.get_mut();
        hb.last_pong = Instant::now();
    }
    Ok(HandlerOutcome::Continue(None))
}

fn reconnect_on_keyword(_state: &mut WsState, msg: &Message) -> Result<HandlerOutcome<String>> {
    if let Message::Text(t) = msg
        && t.as_str() == "reconnect"
    {
        println!("🔁 reconnect requested via message");
        return Ok(HandlerOutcome::Reconnect);
    }
    Ok(HandlerOutcome::Continue(None))
}

async fn user_tick_2(state: &mut WsState, msg: Msg) {
    let bo: &LastMsg = state.get_mut();
    println!("tick2: heavy work done; last_msg={:?}", bo.last_msg);
}

// ---------- Core ws loop ----------
async fn run_ws_loop<S: 'static, Data, M: 'static>(
    url: String,
    mut state: S,
    on_reconnect: impl Fn(&mut S, &mut WsStream) + Send + Sync + 'static,
    handler: impl Fn(&mut S, &Message) -> Result<HandlerOutcome<Data>> + Send + Sync + 'static,
    data_sink: impl Fn(Data) + Send + Sync + 'static,

    user_future_factories: Vec<Factory<S, M>>,
) -> Result<()>
where
    Data: Send + 'static,
{
    use futures::stream::FuturesUnordered;

    loop {
        println!("🔌 Connecting to {url}...");
        let (mut stream, _) = match connect_async(&url).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Connection failed: {e}, retrying in 2s…");
                time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        on_reconnect(&mut state, &mut stream);
        println!("✅ Connected!");

        let mut pending: FuturesUnordered<BoxFuture<'static, (usize, TickFn<S, M>, M)>> =
            FuturesUnordered::new();
        for (i, f) in user_future_factories.iter().enumerate() {
            let f = Arc::clone(f);
            pending.push(
                async move {
                    let (tick, tmsg) = f().await;
                    (i, tick, tmsg)
                }
                .boxed(),
            );
        }

        loop {
            tokio::select! {
                // WS message
                maybe = stream.next() => {
                    match maybe {
                        Some(Ok(msg)) => match handler(&mut state, &msg)? {
                            HandlerOutcome::Continue(maybe_data) => {
                                if let Some(d) = maybe_data { data_sink(d); }
                            }
                            HandlerOutcome::Reconnect => { println!("🔁 reconnect requested by handler"); break; }
                            HandlerOutcome::Stop => { println!("🛑 stop requested by handler"); return Ok(()); }
                        },
                        Some(Err(e)) => { eprintln!("ws error: {e}"); break; }
                        None => { eprintln!("🔕 stream closed"); break; }
                    }
                }
                // Any factory became ready: run its tick, then re-arm the same factory
                item = pending.next() => {
                    match item {
                        Some((idx, tick, tmsg)) => {
                            (tick)(&mut stream, &mut state, tmsg).await;
                            if let Some(factory) = user_future_factories.get(idx).cloned() {
                                pending.push(async move {
                                    let (tck, tmsg2) = factory().await;
                                    (idx, tck, tmsg2)
                                }.boxed());
                            }
                        }
                        None => {
                            // no factories armed; small idle
                            time::sleep(Duration::from_millis(50)).await;
                        }
                    }
                }
            }
        }
        println!("⚠️ connection lost, retrying in 2s…");
        time::sleep(Duration::from_secs(2)).await;
    }
}

// ---------- Demo main ----------
#[tokio::main]
async fn main() -> Result<()> {
    let (last_activity_tx, last_activity_rx) = watch::channel::<Instant>(Instant::now());

    let state = make_state();

    let combined_handler = {
        let inner = chain_handlers(vec![pong_updater, reconnect_on_keyword, text_logger]);
        move |s: &mut WsState, msg: &Message| {
            // update "activity" clock on any message
            let _ = last_activity_tx.send(Instant::now());
            inner(s, msg)
        }
    };

    let notify = Arc::new(Notify::new());
    let on_notify: Trig<Msg> = Arc::new(NotifyTrigger {
        notify: notify.clone(),
    });

    let (sig_tx, _sig_rx0) = broadcast::channel::<Msg>(64);

    let broadcast = Arc::new(BroadcastTrigger { tx: sig_tx.clone() });

    let f2 = make_factory::<_, Msg>(broadcast.clone(), |stream, s, msg| {
        Box::pin(user_tick_2(s, msg))
    });

    let factories = vec![f2];

    // every 5s, broadcast a signal
    let sig_tx_clone = sig_tx.clone();
    tokio::spawn(async move {
        loop {
            time::sleep(Duration::from_secs(1)).await;
            let _ = sig_tx_clone.send(Msg::TestA);
        }
    });

    // after 7s, fire the notify once
    // let notify_clone = notify.clone();
    // tokio::spawn(async move {
    //     time::sleep(Duration::from_secs(7)).await;
    //     notify_clone.notify_one();
    // });

    // Use any echo websocket or your own server. For local testing, change the URL accordingly.
    run_ws_loop::<WsState, String, Msg>(
        "ws://localhost:1234".to_string(),
        state,
        |_state: &mut WsState, _stream: &mut WsStream| {
            // (re)subscribe, send hello, etc.
        },
        combined_handler,
        |data| println!("📩 data: {data}"),
        factories,
    )
    .await
}
