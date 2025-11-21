use anyhow::Result;
use frunk::{HCons, HNil};
use futures::future::{BoxFuture, FutureExt};
use tokio_tungstenite::tungstenite::Message;

use crate::types::{HandlerOutcome, WsStream};

pub trait WsHandler<S> {
    fn handle<'a>(
        &'a self,
        ws: &'a mut WsStream,
        state: &'a mut S,
        msg: &'a Message,
    ) -> BoxFuture<'a, Result<HandlerOutcome>>;
}

impl<F, S> WsHandler<S> for F
where
    F: for<'a> Fn(
        &'a mut WsStream,
        &'a mut S,
        &'a Message,
    ) -> BoxFuture<'a, Result<HandlerOutcome>>,
{
    fn handle<'a>(
        &'a self,
        ws: &'a mut WsStream,
        state: &'a mut S,
        msg: &'a Message,
    ) -> BoxFuture<'a, Result<HandlerOutcome>> {
        self(ws, state, msg)
    }
}

impl<S, Head, Tail> WsHandler<S> for HCons<Head, Tail>
where
    Head: WsHandler<S> + Sync,
    Tail: WsHandler<S> + Sync,
    S: Send + 'static,
{
    fn handle<'a>(
        &'a self,
        ws: &'a mut WsStream,
        state: &'a mut S,
        msg: &'a Message,
    ) -> BoxFuture<'a, Result<HandlerOutcome>> {
        async move {
            match self.head.handle(ws, state, msg).await? {
                HandlerOutcome::Continue => self.tail.handle(ws, state, msg).await,
                outcome => Ok(outcome),
            }
        }
        .boxed()
    }
}

impl<S> WsHandler<S> for HNil {
    fn handle<'a>(
        &'a self,
        _ws: &'a mut WsStream,
        _state: &'a mut S,
        _msg: &'a Message,
    ) -> BoxFuture<'a, Result<HandlerOutcome>> {
        async { Ok(HandlerOutcome::Continue) }.boxed()
    }
}
