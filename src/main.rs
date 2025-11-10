use anyhow::Result;
use chrono::{DateTime, Utc};
use frunk::hlist::Selector;
use frunk::{HCons, HNil, hlist};
use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Notify, broadcast};
use tokio::time::sleep;
use tokio::{net::TcpStream, time};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

// ---------- State structs ----------
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

pub type WsState = HCons<LastMsg, HCons<Heartbeat, HCons<Conn, HNil>>>;

fn make_state() -> WsState {
    hlist![
        LastMsg {
            last_msg: Utc::now()
        },
        Heartbeat {
            last_pong: Instant::now(),
            last_msg: Utc::now()
        },
        Conn { connected: false },
    ]
}

// ---------- Core handler outcome ----------
enum HandlerOutcome {
    Continue,
    Reconnect,
    Stop,
}

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

// ---------- Trigger trait ----------
trait Trigger<M: Default>: Send + Sync {
    fn arm(&self) -> BoxFuture<'static, M>;
}

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

struct BroadcastTrigger<T: Clone + Send + 'static> {
    tx: broadcast::Sender<T>,
}
impl<M: Clone + Send + 'static + Default> Trigger<M> for BroadcastTrigger<M> {
    fn arm(&self) -> BoxFuture<'static, M> {
        let mut rx = self.tx.subscribe();
        async move { rx.recv().await.unwrap_or_default() }.boxed()
    }
}

// ---------- Handler helpers ----------
fn text_logger<S, I>(state: &mut S, msg: &Message) -> Result<HandlerOutcome>
where
    S: Selector<LastMsg, I>,
{
    if let Message::Text(txt) = msg {
        println!("📜 Received text: {txt}");
        let last: &mut LastMsg = state.get_mut();
        last.last_msg = Utc::now();
    }
    Ok(HandlerOutcome::Continue)
}

fn pong_updater<S, I>(state: &mut S, msg: &Message) -> Result<HandlerOutcome>
where
    S: Selector<Heartbeat, I>,
{
    if matches!(msg, Message::Pong(_)) {
        let hb: &mut Heartbeat = state.get_mut();
        hb.last_pong = Instant::now();
    }
    Ok(HandlerOutcome::Continue)
}

fn reconnect_on_keyword(_state: &mut WsState, msg: &Message) -> Result<HandlerOutcome> {
    if let Message::Text(t) = msg
        && t.as_str() == "reconnect"
    {
        println!("🔁 reconnect requested via message");
        return Ok(HandlerOutcome::Reconnect);
    }
    Ok(HandlerOutcome::Continue)
}

// ---------- Tick functions ----------
#[derive(Clone, Default)]
struct Beat;

async fn tick_broadcast(_ws: &mut WsStream, state: &mut WsState, m: String) {
    let last = state.get_mut::<LastMsg, _>(); // &mut LastMsg is fine for read
    println!("tick_broadcast: last_msg={:?} msg={m}", last.last_msg);
}

async fn tick_heartbeat(_ws: &mut WsStream, state: &mut WsState, _m: Beat) {
    let hb = state.get_mut::<Heartbeat, _>();
    println!("tick_heartbeat: last_pong={:?}", hb.last_pong);
}

// ---------- Tick type (FIXED: HRTB over lifetimes) ----------
type Tick<M> =
    Arc<dyn for<'a> Fn(&'a mut WsStream, &'a mut WsState, M) -> BoxFuture<'a, ()> + Send + Sync>;

// ---------- Handler chain macro ----------
#[macro_export]
macro_rules! make_handler_chain {
    (
        fn $fname:ident < $StateTy:ty, $DataTy:ty > ();
        $($handler:path),+ $(,)?
    ) => {
        fn $fname(state: &mut $StateTy, msg: &Message) -> anyhow::Result<HandlerOutcome> {
            $(
                match $handler(state, msg)? {
                    HandlerOutcome::Continue => { /* keep going */ }
                    HandlerOutcome::Reconnect => return Ok(HandlerOutcome::Reconnect),
                    HandlerOutcome::Stop => return Ok(HandlerOutcome::Stop),
                }
            )+
            Ok(HandlerOutcome::Continue)
        }
    }
}

#[macro_export]
macro_rules! make_ws_loop {
    (
        fn $fname:ident (
            url: String,
            state: $StateTy:ty,
            handler: $handler:path,
            on_reconnect: $on_reconnect:path
        );
        $(
            $arm_name:ident : { msg_ty: $MsgTy:ty }
        );+ $(;)?
    ) => {
        async fn $fname(
            url: String,
            mut state: $StateTy,
            $(
                $arm_name: (Arc<dyn Trigger<$MsgTy>>, Tick<$MsgTy>),
            )+
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

                $on_reconnect(&mut state, &mut stream);
                println!("✅ Connected!");

                paste::paste! {
                    $(
                        let ([<trig_ $arm_name>], [<tick_ $arm_name>]) = $arm_name.clone();
                        let mut [<fut_ $arm_name>] = [<trig_ $arm_name>].arm();
                    )+
                }

                loop {
                    // run paste outside so select! sees plain arms
                    paste::paste! {
                        tokio::select! {
                            maybe = stream.next() => {
                                match maybe {
                                    Some(Ok(msg)) => match $handler(&mut state, &msg)? {
                                        HandlerOutcome::Continue => {}
                                        HandlerOutcome::Reconnect => {
                                            println!("🔁 reconnect requested by handler");
                                            break;
                                        }
                                        HandlerOutcome::Stop => {
                                            println!("🛑 stop requested by handler");
                                            return Ok(());
                                        }
                                    },
                                    Some(Err(e)) => { eprintln!("ws error: {e}"); break; }
                                    None => { eprintln!("🔕 stream closed"); break; }
                                }
                            },
                            $(
                                m = &mut [<fut_ $arm_name>] => {
                                    ([<tick_ $arm_name>])(&mut stream, &mut state, m).await;
                                    [<fut_ $arm_name>] = [<trig_ $arm_name>].arm();
                                },
                            )+
                        }
                    }
                }

                println!("⚠️ connection lost, retrying in 2s…");
                time::sleep(Duration::from_secs(2)).await;
            }
        }
    };
}

// ---------- Handler chain ----------
make_handler_chain! {
    fn combined_handler<WsState, String>();
    pong_updater,
    reconnect_on_keyword,
    text_logger
}

pub fn on_reconnect<S, I>(state: &mut S, _stream: &mut WsStream)
where
    S: Selector<Conn, I>,
{
    state.get_mut().connected = true
    // Resubscribe or send hello here if needed
}

make_ws_loop! {
    fn run_ws_loop(
        url: String,
        state: WsState,
        handler: combined_handler,
        on_reconnect: on_reconnect
    );
    broadcast_arm: { msg_ty: String };
    heartbeat_arm: { msg_ty: Beat };
}

// ---------- Demo main ----------
#[tokio::main]
async fn main() -> Result<()> {
    let state = make_state();

    let (tx, _rx0) = broadcast::channel::<String>(64);
    let tx_clone = tx.clone();
    tokio::spawn(async move {
        loop {
            time::sleep(Duration::from_secs(1)).await;
            let _ = tx_clone.send("bo".to_string());
        }
    });

    let broadcast_trigger: Arc<dyn Trigger<String>> = Arc::new(BroadcastTrigger { tx });
    let broadcast_tick: Tick<String> = Arc::new(|ws, st, m| Box::pin(tick_broadcast(ws, st, m)));

    // --- Interval trigger + tick fn ---
    let heartbeat_trigger: Arc<dyn Trigger<Beat>> = Arc::new(IntervalTrigger {
        period: Duration::from_secs(3),
    });
    let heartbeat_tick: Tick<Beat> = Arc::new(|ws, st, m| Box::pin(tick_heartbeat(ws, st, m)));

    run_ws_loop(
        "ws://localhost:1234".to_string(),
        state,
        (broadcast_trigger, broadcast_tick),
        (heartbeat_trigger, heartbeat_tick),
    )
    .await
}
