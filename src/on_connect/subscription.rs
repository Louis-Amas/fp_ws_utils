use frunk::hlist::Selector;
use futures::{
    SinkExt,
    future::{BoxFuture, FutureExt},
};
use tokio_tungstenite::tungstenite::Message;

use crate::types::WsStream;

#[derive(Clone, Debug, Default)]
pub struct SubscriptionState {
    pub subscriptions: Vec<String>,
}

// Action to send subscriptions
pub fn send_subscriptions<'a, S, I>(
    ws: &'a mut WsStream,
    state: &'a mut S,
) -> BoxFuture<'a, ()>
where
    S: Selector<SubscriptionState, I> + Send + 'static,
{
    async move {
        let sub_state: &mut SubscriptionState = state.get_mut();
        if !sub_state.subscriptions.is_empty() {
            for sub in &sub_state.subscriptions {
                println!("📡 Subscribing: {}", sub);
                let _ = ws.send(Message::Text(sub.clone().into())).await;
            }
        }
    }
    .boxed()
}
