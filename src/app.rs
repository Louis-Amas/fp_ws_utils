use crate::state::{Heartbeat, LastMsg};
use crate::types::{HandlerOutcome, WsStream};
use anyhow::Result;
use chrono::Utc;
use frunk::hlist::Selector;
use futures::future::{BoxFuture, FutureExt};
use std::time::Instant;
use tokio_tungstenite::tungstenite::Message;

pub fn log_text<'a, S, I>(
    _ws: &'a mut WsStream,
    state: &'a mut S,
    msg: &'a Message,
) -> BoxFuture<'a, Result<HandlerOutcome>>
where
    S: Selector<LastMsg, I> + Send + 'static,
{
    if let Message::Text(t) = msg {
        println!("📜 Received: {}", t.as_str());
        let last: &mut LastMsg = state.get_mut();
        last.last_msg = Utc::now();
    }
    async move { Ok(HandlerOutcome::Continue) }.boxed()
}

pub fn update_pong<'a, S, I>(
    _ws: &'a mut WsStream,
    state: &'a mut S,
    msg: &'a Message,
) -> BoxFuture<'a, Result<HandlerOutcome>>
where
    S: Selector<Heartbeat, I> + Send + 'static,
{
    if matches!(msg, Message::Pong(_)) {
        let hb: &mut Heartbeat = state.get_mut();
        hb.last_pong = Instant::now();
    }
    async move { Ok(HandlerOutcome::Continue) }.boxed()
}
