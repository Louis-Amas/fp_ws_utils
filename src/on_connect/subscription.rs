use crate::types::{ContextState, WsStream};
use frunk::hlist::Selector;
use futures::{
    SinkExt,
    future::{BoxFuture, FutureExt},
};
use tokio_tungstenite::tungstenite::Message;
use tracing::info;

#[derive(Clone, Debug, Default)]
pub struct SubscriptionState {
    pub subscriptions: Vec<String>,
}

// Action to send subscriptions
pub fn send_subscriptions<'a, S, I, J>(ws: &'a mut WsStream, state: &'a mut S) -> BoxFuture<'a, ()>
where
    S: Selector<SubscriptionState, I> + Selector<ContextState, J> + Send + 'static,
{
    async move {
        let ctx: &ContextState = state.get();
        let sub_state: &SubscriptionState = state.get();
        if !sub_state.subscriptions.is_empty() {
            for sub in &sub_state.subscriptions {
                info!("[{}] 📡 Subscribing: {}", ctx.context, sub);
                let _ = ws.send(Message::Text(sub.into())).await;
            }
        }
    }
    .boxed()
}
