use anyhow::Result;
use chrono::{DateTime, Utc};
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
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

// ---------- Core types ----------
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
type Factory = Arc<dyn Fn() -> BoxFuture<'static, TickFn> + Send + Sync>;

// ---- HRTB-friendly types for user ticks ----
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
trait Trigger: Send + Sync {
    fn arm(&self) -> BoxFuture<'static, ()>; // resolves when ready
}
type Trig = Arc<dyn Trigger>;

// Fires every fixed interval
struct IntervalTrigger {
    period: Duration,
}
impl Trigger for IntervalTrigger {
    fn arm(&self) -> BoxFuture<'static, ()> {
        let d = self.period;
        async move { sleep(d).await }.boxed()
    }
}

// Fires when someone calls `notify.notify_one()` or `notify_waiters()`
struct NotifyTrigger {
    notify: Arc<Notify>,
}
impl Trigger for NotifyTrigger {
    fn arm(&self) -> BoxFuture<'static, ()> {
        let n = self.notify.clone();
        async move { n.notified().await }.boxed()
    }
}

// Fires when a broadcast message is published (each arming gets its own Rx)
struct BroadcastTrigger<T: Clone + Send + 'static> {
    tx: broadcast::Sender<T>,
}
impl<T: Clone + Send + 'static> Trigger for BroadcastTrigger<T> {
    fn arm(&self) -> BoxFuture<'static, ()> {
        let mut rx = self.tx.subscribe();
        async move {
            let _ = rx.recv().await;
        }
        .boxed()
    }
}

// Fires with exponential backoff (stateful)
struct BackoffTrigger {
    base: Duration,
    max: Duration,
    state: Mutex<Duration>,
}
impl Trigger for BackoffTrigger {
    fn arm(&self) -> BoxFuture<'static, ()> {
        let delay = {
            let mut s = self.state.lock().unwrap();
            let current = (*s).max(self.base);
            *s = (current * 2).min(self.max);
            current
        };
        async move { sleep(delay).await }.boxed()
    }
}

// Combinators: AnyOf, AllOf
struct AnyOf {
    a: Trig,
    b: Trig,
}
impl Trigger for AnyOf {
    fn arm(&self) -> BoxFuture<'static, ()> {
        let fa = self.a.arm();
        let fb = self.b.arm();
        async move {
            tokio::select! {
                _ = fa => {},
                _ = fb => {},
            }
        }
        .boxed()
    }
}

struct AllOf {
    a: Trig,
    b: Trig,
}
impl Trigger for AllOf {
    fn arm(&self) -> BoxFuture<'static, ()> {
        let fa = self.a.arm();
        let fb = self.b.arm();
        async move {
            let (_ra, _rb) = tokio::join!(fa, fb);
        }
        .boxed()
    }
}
type TickFn = for<'a> fn(&'a mut WsState) -> UserFuture<'a>;

fn make_factory(trig: Trig, tick: TickFn) -> Factory {
    Arc::new(move || {
        let trig = trig.clone();
        async move {
            trig.arm().await;
            tick // just return the function pointer
        }
        .boxed()
    })
}

// ---------- Handler chaining ----------
fn chain_handlers<Data: 'static>(
    handlers: Vec<Handler<Data>>,
) -> impl Fn(&mut WsState, &Message) -> Result<HandlerOutcome<Data>> {
    move |state: &mut WsState, msg: &Message| {
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
    if let Message::Text(t) = msg {
        if t.as_str() == "reconnect" {
            println!("🔁 reconnect requested via message");
            return Ok(HandlerOutcome::Reconnect);
        }
    }
    Ok(HandlerOutcome::Continue(None))
}

// ---------- Example ticks ----------
async fn user_tick_1(state: &mut WsState) {
    println!(
        "tick1: connected={} last_msg={}",
        state.connected, state.last_msg
    );
}

async fn user_tick_2(state: &mut WsState) {
    time::sleep(Duration::from_millis(150)).await;
    println!("tick2: heavy work done; last_pong={:?}", state.last_pong);
}

// ---------- Core ws loop ----------
async fn run_ws_loop<Data>(
    url: String,
    mut state: WsState,
    on_reconnect: impl Fn(&mut WsState, &mut WsStream) + Send + Sync + 'static,
    handler: impl Fn(&mut WsState, &Message) -> Result<HandlerOutcome<Data>> + Send + Sync + 'static,
    data_sink: impl Fn(Data) + Send + Sync + 'static,

    user_future_factories: Vec<Factory>,
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

        state.connected = true;
        on_reconnect(&mut state, &mut stream);
        println!("✅ Connected!");

        let mut pending: FuturesUnordered<BoxFuture<'static, (usize, TickFn)>> =
            FuturesUnordered::new();
        for (i, f) in user_future_factories.iter().enumerate() {
            let f = Arc::clone(f);
            pending.push(async move { (i, f().await) }.boxed());
        }

        loop {
            tokio::select! {
                // WS message
                maybe = stream.next() => {
                    match maybe {
                        Some(Ok(msg)) => match handler(&mut state, &msg)? {
                            HandlerOutcome::Continue(maybe_data) => {
                                state.last_msg = Utc::now();
                                if let Some(d) = maybe_data { data_sink(d); }
                            }
                            HandlerOutcome::Reconnect => { println!("🔁 reconnect requested by handler"); state.connected = false; break; }
                            HandlerOutcome::Stop => { println!("🛑 stop requested by handler"); return Ok(()); }
                        },
                        Some(Err(e)) => { eprintln!("ws error: {e}"); break; }
                        None => { eprintln!("🔕 stream closed"); break; }
                    }
                }
                // Any factory became ready: run its tick, then re-arm the same factory
                item = pending.next() => {
                    match item {
                        Some((idx, tick)) => {
                            (tick)(&mut state).await;
                            if let Some(factory) = user_future_factories.get(idx).cloned() {
                                pending.push(async move { (idx, factory().await) }.boxed());
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

        state.connected = false;
        println!("⚠️ connection lost, retrying in 2s…");
        time::sleep(Duration::from_secs(2)).await;
    }
}

// ---------- Demo main ----------
#[tokio::main]
async fn main() -> Result<()> {
    // --- shared “last activity” for a watch-trigger demo
    let (last_activity_tx, last_activity_rx) = watch::channel::<Instant>(Instant::now());

    // --- initial state
    let state = WsState {
        connected: false,
        last_pong: Instant::now(),
        last_msg: Utc::now(),
    };

    // --- handlers (update watch when message arrives)
    let combined_handler = {
        let inner = chain_handlers(vec![pong_updater, reconnect_on_keyword, text_logger]);
        move |s: &mut WsState, msg: &Message| {
            // update "activity" clock on any message
            let _ = last_activity_tx.send(Instant::now());
            inner(s, msg)
        }
    };

    // --- triggers
    let periodic_2s: Trig = Arc::new(IntervalTrigger {
        period: Duration::from_secs(2),
    });

    let notify = Arc::new(Notify::new());
    let on_notify: Trig = Arc::new(NotifyTrigger {
        notify: notify.clone(),
    });

    let (sig_tx, _sig_rx0) = broadcast::channel::<()>(64);
    let on_signal: Trig = Arc::new(BroadcastTrigger { tx: sig_tx.clone() });

    // fires when idle ≥ 3s
    // let idle_3s: Trig = Arc::new(WatchTrigger {
    //     rx: last_activity_rx.clone(),
    //     pred: Arc::new(|last: &Instant| last.elapsed() >= Duration::from_secs(3)),
    // });

    // exponential backoff starting at 200ms up to 5s
    let backoff: Trig = Arc::new(BackoffTrigger {
        base: Duration::from_millis(200),
        max: Duration::from_secs(5),
        state: Mutex::new(Duration::from_millis(200)),
    });

    // // combine: run when idle ≥3s AND backoff finishes
    // let idle_then_backoff: Trig = Arc::new(AllOf {
    //     a: idle_3s.clone(),
    //     b: backoff.clone(),
    // });

    // or: either periodic 2s OR manual signal
    let periodic_or_signal: Trig = Arc::new(AnyOf {
        a: periodic_2s.clone(),
        b: on_signal.clone(),
    });

    // --- build user ticks as factories
    let f1 = make_factory(periodic_or_signal.clone(), |s| Box::pin(user_tick_1(s)));
    let f2 = make_factory(on_notify.clone(), |s| Box::pin(user_tick_2(s)));
    // let f3 = make_factory(idle_then_backoff.clone(), |s| Box::pin(user_tick_2(s)));

    let factories = vec![f1, f2];

    // --- demo: produce some signals in background
    // every 5s, broadcast a signal
    let sig_tx_clone = sig_tx.clone();
    tokio::spawn(async move {
        loop {
            time::sleep(Duration::from_secs(5)).await;
            let _ = sig_tx_clone.send(());
        }
    });

    // after 7s, fire the notify once
    let notify_clone = notify.clone();
    tokio::spawn(async move {
        time::sleep(Duration::from_secs(7)).await;
        notify_clone.notify_one();
    });

    // --- run
    // Use any echo websocket or your own server. For local testing, change the URL accordingly.
    run_ws_loop::<String>(
        "wss://echo.websocket.events".to_string(), // or "ws://localhost:1234"
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
