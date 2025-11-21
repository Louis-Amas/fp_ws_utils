use std::time::Instant;

use anyhow::Result;
use frunk::hlist::Selector;
use futures::future::{BoxFuture, FutureExt};
use tokio_tungstenite::tungstenite::Message;

use crate::types::{HandlerOutcome, WsStream};

#[derive(Clone, Debug)]
pub struct Heartbeat {
    pub last_pong: Instant,
}

impl Default for Heartbeat {
    fn default() -> Self {
        Self {
            last_pong: Instant::now(),
        }
    }
}

pub fn update_pong<'a, S, I>(
    _ws: &'a mut WsStream,
    state: &'a mut S,
    msg: &'a Message,
) -> BoxFuture<'a, Result<HandlerOutcome>>
where
    S: Selector<Heartbeat, I> + Send + 'static,
{
    async move {
        if matches!(msg, Message::Pong(_)) {
            let hb: &mut Heartbeat = state.get_mut();
            hb.last_pong = Instant::now();
        }
        Ok(HandlerOutcome::Continue)
    }
    .boxed()
}
