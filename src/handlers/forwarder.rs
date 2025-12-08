use crate::types::{ContextState, HandlerOutcome, WsStream};
use anyhow::Result;
use frunk::hlist::Selector;
use futures::future::{BoxFuture, FutureExt};
use tokio::sync::mpsc::UnboundedSender;
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone, Debug)]
pub struct ForwarderState<T> {
    pub sender: UnboundedSender<T>,
}

pub trait MessageParser<T> {
    fn parse(&self, msg: &Message) -> Result<Option<T>>;
}

impl<F, T> MessageParser<T> for F
where
    F: Fn(&Message) -> Result<Option<T>> + Send + Sync,
{
    fn parse(&self, msg: &Message) -> Result<Option<T>> {
        self(msg)
    }
}

// A generic handler that parses messages and forwards them
pub fn forward_messages<'a, S, I, J, T, P, F>(
    _ws: &'a mut WsStream,
    state: &'a mut S,
    msg: &'a Message,
    parser: &'a P,
    should_ignore: F,
) -> BoxFuture<'a, Result<HandlerOutcome>>
where
    S: Selector<ForwarderState<T>, I> + Selector<ContextState, J> + Send + 'static,
    T: Send + 'static,
    P: MessageParser<T> + Send + Sync + 'static,
    F: Fn(&Message) -> bool + Send + Sync + Clone + 'static,
{
    async move {
        match parser.parse(msg) {
            Ok(Some(parsed)) => {
                let fwd_state: &mut ForwarderState<T> = state.get_mut();
                let _ = fwd_state.sender.send(parsed);
            }
            Ok(None) => {}
            Err(e) => {
                if !should_ignore(msg) {
                    let ctx: &ContextState = state.get();
                    tracing::warn!(
                        "[{}] Failed to parse message: {} error: {}",
                        ctx.context,
                        msg,
                        e
                    );
                }
            }
        }
        Ok(HandlerOutcome::Continue)
    }
    .boxed()
}

pub struct JsonParser<T>(std::marker::PhantomData<T>);

impl<T> Default for JsonParser<T> {
    fn default() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<T> JsonParser<T> {
    pub const fn new() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<T: serde::de::DeserializeOwned> MessageParser<T> for JsonParser<T> {
    fn parse(&self, msg: &Message) -> Result<Option<T>> {
        if let Message::Text(text) = msg {
            serde_json::from_str::<T>(text)
                .map(Some)
                .map_err(Into::into)
        } else {
            Ok(None)
        }
    }
}
