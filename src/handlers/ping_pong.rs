use crate::types::{ContextState, HandlerOutcome, WsStream};
use anyhow::Result;
use frunk::hlist::Selector;
use futures::{
    SinkExt,
    future::{BoxFuture, FutureExt},
};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;
use tracing::warn;

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
pub fn check_timeout<'a, S, I, J>(
    _ws: &'a mut WsStream,
    state: &'a mut S,
) -> BoxFuture<'a, HandlerOutcome>
where
    S: Selector<PingState, I> + Selector<ContextState, J> + Send + 'static,
{
    async move {
        let timeout = {
            let ps: &mut PingState = state.get_mut();
            ps.last_pong.elapsed() > ps.ping_interval + ps.pong_timeout
        };

        if timeout {
            let ctx: &ContextState = state.get();
            warn!("[{}] ⏰ Ping timeout! Reconnecting...", ctx.context);
            return HandlerOutcome::Reconnect;
        }
        HandlerOutcome::Continue
    }
    .boxed()
}
