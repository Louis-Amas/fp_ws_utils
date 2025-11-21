use std::time::{Duration, Instant};

use anyhow::Result;
use frunk::hlist::Selector;
use futures::{
    SinkExt,
    future::{BoxFuture, FutureExt},
};
use tokio_tungstenite::tungstenite::Message;

use crate::types::{HandlerOutcome, WsStream};

#[derive(Clone, Debug)]
pub struct PingState {
    pub last_pong: Instant,
    pub ping_interval: Duration,
    pub pong_timeout: Duration,
}

impl Default for PingState {
    fn default() -> Self {
        Self {
            last_pong: Instant::now(),
            ping_interval: Duration::from_secs(30),
            pong_timeout: Duration::from_secs(10),
        }
    }
}

pub fn handle_ping_pong<'a, S, I>(
    ws: &'a mut WsStream,
    state: &'a mut S,
    msg: &'a Message,
) -> BoxFuture<'a, Result<HandlerOutcome>>
where
    S: Selector<PingState, I> + Send + 'static,
{
    async move {
        match msg {
            Message::Ping(data) => {
                // Auto-reply to Ping is handled by tungstenite usually, but we can be explicit
                let _ = ws.send(Message::Pong(data.clone())).await;
            }
            Message::Pong(_) => {
                let ps: &mut PingState = state.get_mut();
                ps.last_pong = Instant::now();
            }
            _ => {}
        }
        Ok(HandlerOutcome::Continue)
    }
    .boxed()
}

// This function checks if we timed out
pub fn check_timeout<'a, S, I>(
    _ws: &'a mut WsStream,
    state: &'a mut S,
) -> BoxFuture<'a, ()>
where
    S: Selector<PingState, I> + Send + 'static,
{
    async move {
        let ps: &mut PingState = state.get_mut();
        if ps.last_pong.elapsed() > ps.ping_interval + ps.pong_timeout {
            println!("⏰ Ping timeout! Reconnecting...");
            // In a real scenario, we might want to trigger a reconnection here.
            // However, 'bind_stream' actions return (), so they can't directly force a reconnect via return value.
            // We would typically close the stream or use a shared flag.
            // For now, we just log.
        }
    }
    .boxed()
}
