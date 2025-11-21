use anyhow::Result;
use chrono::{DateTime, Utc};
use frunk::hlist::Selector;
use futures::future::{BoxFuture, FutureExt};
use tokio_tungstenite::tungstenite::Message;

use crate::types::{HandlerOutcome, WsStream};

#[derive(Clone, Debug)]
pub struct LastMsg {
    pub last_msg: DateTime<Utc>,
}

impl Default for LastMsg {
    fn default() -> Self {
        Self {
            last_msg: Utc::now(),
        }
    }
}

pub fn log_text<'a, S, I>(
    _ws: &'a mut WsStream,
    state: &'a mut S,
    msg: &'a Message,
) -> BoxFuture<'a, Result<HandlerOutcome>>
where
    S: Selector<LastMsg, I> + Send + 'static,
{
    async move {
        if let Message::Text(t) = msg {
            println!("📜 Received: {}", t.as_str());
            let last: &mut LastMsg = state.get_mut();
            last.last_msg = Utc::now();
        }
        Ok(HandlerOutcome::Continue)
    }
    .boxed()
}
