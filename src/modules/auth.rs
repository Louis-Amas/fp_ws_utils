use frunk::hlist::Selector;
use futures::{
    SinkExt,
    future::{BoxFuture, FutureExt},
};
use tokio_tungstenite::tungstenite::Message;

use crate::types::WsStream;

#[derive(Clone, Debug)]
pub struct AuthState {
    pub auth_message: Option<String>,
    pub authenticated: bool,
}

impl Default for AuthState {
    fn default() -> Self {
        Self {
            auth_message: None,
            authenticated: false,
        }
    }
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
        if !auth_state.authenticated {
            if let Some(msg) = &auth_state.auth_message {
                println!("🔐 Sending Auth: {}", msg);
                let _ = ws.send(Message::Text(msg.clone().into())).await;
                auth_state.authenticated = true; // Optimistic update, or wait for response in a Handler
            }
        }
    }
    .boxed()
}
