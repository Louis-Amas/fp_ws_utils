use crate::types::{ContextState, HandlerOutcome, WsStream};
use anyhow::Result;
use chrono::{DateTime, Utc};
use frunk::hlist::Selector;
use futures::future::{BoxFuture, FutureExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

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

pub fn log_text<'a, S, I, J>(
    _ws: &'a mut WsStream,
    state: &'a mut S,
    msg: &'a Message,
) -> BoxFuture<'a, Result<HandlerOutcome>>
where
    S: Selector<LastMsg, I> + Selector<ContextState, J> + Send + 'static,
{
    async move {
        if let Message::Text(t) = msg {
            let ctx: &ContextState = state.get();
            info!("[{}] 📜 Received: {}", ctx.context, t.as_str());
            let last: &mut LastMsg = state.get_mut();
            last.last_msg = Utc::now();
        }
        Ok(HandlerOutcome::Continue)
    }
    .boxed()
}

pub fn check_last_msg_timeout<'a, S, I, J>(
    _ws: &'a mut WsStream,
    state: &'a mut S,
    timeout: Duration,
) -> BoxFuture<'a, HandlerOutcome>
where
    S: Selector<LastMsg, I> + Selector<ContextState, J> + Send + 'static,
{
    async move {
        let timeout_occurred = {
            let last: &mut LastMsg = state.get_mut();
            Utc::now()
                .signed_duration_since(last.last_msg)
                .to_std()
                .unwrap_or(Duration::ZERO)
                > timeout
        };

        if timeout_occurred {
            let ctx: &ContextState = state.get();
            warn!(
                "[{}] ⏰ No message received for {:?}! Reconnecting...",
                ctx.context, timeout
            );
            return HandlerOutcome::Reconnect;
        }
        HandlerOutcome::Continue
    }
    .boxed()
}

pub fn update_last_msg<'a, S, I>(
    _ws: &'a mut WsStream,
    state: &'a mut S,
    _msg: &'a Message,
) -> BoxFuture<'a, Result<HandlerOutcome>>
where
    S: Selector<LastMsg, I> + Send + 'static,
{
    async move {
        let last: &mut LastMsg = state.get_mut();
        last.last_msg = Utc::now();
        Ok(HandlerOutcome::Continue)
    }
    .boxed()
}
