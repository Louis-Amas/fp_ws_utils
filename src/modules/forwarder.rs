use anyhow::Result;
use frunk::hlist::Selector;
use futures::future::{BoxFuture, FutureExt};
use tokio::sync::mpsc::UnboundedSender;
use tokio_tungstenite::tungstenite::Message;

use crate::types::{HandlerOutcome, WsStream};

#[derive(Clone, Debug)]
pub struct ForwarderState<T> {
    pub sender: UnboundedSender<T>,
}

pub trait MessageParser<T> {
    fn parse(&self, msg: &Message) -> Option<T>;
}

// A generic handler that parses messages and forwards them
pub fn forward_messages<'a, S, I, T, P>(
    _ws: &'a mut WsStream,
    state: &'a mut S,
    msg: &'a Message,
    parser: &'a P,
) -> BoxFuture<'a, Result<HandlerOutcome>>
where
    S: Selector<ForwarderState<T>, I> + Send + 'static,
    T: Send + 'static,
    P: MessageParser<T> + Send + Sync + 'static,
{
    async move {
        if let Some(parsed) = parser.parse(msg) {
            let fwd_state: &mut ForwarderState<T> = state.get_mut();
            let _ = fwd_state.sender.send(parsed);
        }
        Ok(HandlerOutcome::Continue)
    }
    .boxed()
}
