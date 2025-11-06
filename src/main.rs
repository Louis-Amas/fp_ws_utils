use anyhow::Result;
use chrono::{DateTime, Utc};
use frunk::hlist::Selector;
use frunk::{HCons, HNil, hlist};
use futures::FutureExt;
use futures::StreamExt;
use futures::future::BoxFuture;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Notify, broadcast, watch};
use tokio::time::sleep;
use tokio::{net::TcpStream, time};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

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

// ---------- Core handler outcome ----------
enum HandlerOutcome<Data> {
    Continue(Option<Data>),
    Reconnect,
    Stop,
}

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

trait Trigger<M: Default>: Send + Sync {
    fn arm(&self) -> BoxFuture<'static, M>;
}

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

        // FIXME: DO not unwrap
        async move { rx.recv().await.unwrap_or_default() }.boxed()
    }
}

// ---------- Handler helpers (real functions) ----------
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

#[derive(Clone, Default)]
struct Beat; // example: periodic heartbeat tick

async fn tick_broadcast(_ws: &mut WsStream, state: &mut WsState, m: String) {
    let bo: &LastMsg = state.get_mut();
    println!("tick_broadcast: last_msg={:?} msg={m}", bo.last_msg);
}

async fn tick_heartbeat(_ws: &mut WsStream, state: &mut WsState, _m: Beat) {
    let hb: &Heartbeat = state.get_mut();
    println!("tick_heartbeat: last_pong={:?}", hb.last_pong);
}

#[macro_export]
macro_rules! make_handler_chain {
    (
        fn $fname:ident < $StateTy:ty, $DataTy:ty > ();
        $($handler:path),+ $(,)?
    ) => {
        fn $fname(state: &mut $StateTy, msg: &Message) -> anyhow::Result<HandlerOutcome<$DataTy>> {
            $(
                match $handler(state, msg)? {
                    HandlerOutcome::Continue(Some(d)) => return Ok(HandlerOutcome::Continue(Some(d))),
                    HandlerOutcome::Continue(None) => { /* keep going */ }
                    HandlerOutcome::Reconnect => return Ok(HandlerOutcome::Reconnect),
                    HandlerOutcome::Stop => return Ok(HandlerOutcome::Stop),
                }
            )+
            Ok(HandlerOutcome::Continue(None))
        }
    }
}

// ---------- Generate a run loop with one arm per factory ----------
#[macro_export]
macro_rules! make_ws_loop {
    (
        fn $fname:ident (
            url: String,
            state: $StateTy:ty,
            handler: $handler:path,
            data_sink: $sink:path,
            on_reconnect: $on_reconnect:path
        );
        $(
            $arm_name:ident : {
                trig: $trig_expr:expr,
                msg_ty: $MsgTy:ty,
                tick_fn: $tick_fn:path
            }
        );+ $(;)?
    ) => {
        async fn $fname(
            url: String,
            mut state: $StateTy,
        ) -> anyhow::Result<()> {
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

                // user-defined reconnect hook
                $on_reconnect(&mut state, &mut stream);
                println!("✅ Connected!");

                // arm each trigger once
                $(
                    let mut $arm_name : futures::future::BoxFuture<'static, $MsgTy> = ($trig_expr).arm();
                )+

                loop {
                    tokio::select! {
                        maybe = stream.next() => {
                            match maybe {
                                Some(Ok(msg)) => match $handler(&mut state, &msg)? {
                                    HandlerOutcome::Continue(maybe_data) => {
                                        if let Some(d) = maybe_data { $sink(d); }
                                    }
                                    HandlerOutcome::Reconnect => { println!("🔁 reconnect requested by handler"); break; }
                                    HandlerOutcome::Stop => { println!("🛑 stop requested by handler"); return Ok(()); }
                                },
                                Some(Err(e)) => { eprintln!("ws error: {e}"); break; }
                                None => { eprintln!("🔕 stream closed"); break; }
                            }
                        }

                        $(
                            m = &mut $arm_name => {
                                // run the real tick function
                                $tick_fn(&mut stream, &mut state, m).await;
                                // re-arm
                                $arm_name = ($trig_expr).arm();
                            }
                        )+
                    }
                }

                println!("⚠️ connection lost, retrying in 2s…");
                time::sleep(Duration::from_secs(2)).await;
            }
        }
    };
}

make_handler_chain! {
    fn combined_handler<WsState, String>();
    pong_updater,
    reconnect_on_keyword,
    text_logger
}

// Example sink (real function)
pub fn print_data(d: String) {
    println!("📩 data: {d}");
}

// Example reconnect hook (real function)
pub fn on_reconnect(_state: &mut WsState, _stream: &mut WsStream) {
    // (re)subscribe, send hello, etc.
}

// ---------- Build the ws loop with strongly-typed arms ----------
make_ws_loop! {
    fn run_ws_loop(
        url: String,
        state: WsState,
        handler: combined_handler,
        data_sink: print_data,
        on_reconnect: on_reconnect
    );
    // Each arm is: { trigger expr, message type, tick function (real fn path) }
    broadcast_arm: {
        trig: {
            // Broadcast<Sig>
            let (sig_tx, _rx0) = broadcast::channel::<String>(64);
            // demo: broadcast every second
            let tx_clone = sig_tx.clone();
            tokio::spawn(async move {
                loop {
                    time::sleep(Duration::from_secs(1)).await;
                    let _ = tx_clone.send("bo".to_string());
                }
            });
            Arc::new(BroadcastTrigger { tx: sig_tx }) as Arc<dyn Trigger<String>>
        },
        msg_ty: String,
        tick_fn: crate::tick_broadcast
    };
    heartbeat_arm: {
        trig: {
            // Fixed interval Beat
            Arc::new(IntervalTrigger { period: Duration::from_secs(3) }) as Arc<dyn Trigger<Beat>>
        },
        msg_ty: Beat,
        tick_fn: crate::tick_heartbeat
    };
}

// ---------- Demo main ----------
#[tokio::main]
async fn main() -> Result<()> {
    let _activity = watch::channel::<Instant>(Instant::now()); // keep if you want to wire activity separately
    let state = make_state();

    // Use any echo websocket or your own server. For local testing, change the URL accordingly.
    run_ws_loop("ws://localhost:1234".to_string(), state).await
}
