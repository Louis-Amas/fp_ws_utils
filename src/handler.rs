use anyhow::Result;
use frunk::{HCons, HNil};
use tokio_tungstenite::tungstenite::Message;
use crate::types::HandlerOutcome;

pub trait WsHandler<S> {
    fn handle(&self, state: &mut S, msg: &Message) -> Result<HandlerOutcome>;
}

impl<F, S> WsHandler<S> for F
where
    F: Fn(&mut S, &Message) -> Result<HandlerOutcome>,
{
    fn handle(&self, state: &mut S, msg: &Message) -> Result<HandlerOutcome> {
        self(state, msg)
    }
}

impl<S, Head, Tail> WsHandler<S> for HCons<Head, Tail>
where
    Head: WsHandler<S>,
    Tail: WsHandler<S>,
{
    fn handle(&self, state: &mut S, msg: &Message) -> Result<HandlerOutcome> {
        match self.head.handle(state, msg)? {
            HandlerOutcome::Continue => self.tail.handle(state, msg),
            outcome => Ok(outcome),
        }
    }
}

impl<S> WsHandler<S> for HNil {
    fn handle(&self, _state: &mut S, _msg: &Message) -> Result<HandlerOutcome> {
        Ok(HandlerOutcome::Continue)
    }
}
