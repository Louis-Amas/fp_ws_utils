use frunk::hlist::Selector;
use futures::{
    SinkExt,
    future::{BoxFuture, FutureExt},
};
use tokio_tungstenite::tungstenite::Message;

use crate::types::WsStream;

#[derive(Clone, Debug, Default)]
pub struct AuthState {
    pub auth_message: Option<String>,
}

// This is an Action, not a Handler, because it triggers the auth
pub fn send_auth<'a, S, I>(
    ws: &'a mut WsStream,
    state: &'a mut S,
    _trigger: (),
) -> BoxFuture<'a, ()>
where
    S: Selector<AuthState, I> + Send + 'static,
{
    async move {
        let auth_state: &mut AuthState = state.get_mut();
        if let Some(msg) = &auth_state.auth_message {
            println!("🔐 Sending Auth: {}", msg);
            let _ = ws.send(Message::Text(msg.clone().into())).await;
        }
    }
    .boxed()
}
