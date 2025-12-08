use futures::future::BoxFuture;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub type Action<S> =
    Box<dyn for<'a> FnOnce(&'a mut WsStream, &'a mut S) -> BoxFuture<'a, HandlerOutcome> + Send>;

pub type ConnectHandler<S> =
    Box<dyn for<'a> Fn(&'a mut WsStream, &'a mut S) -> BoxFuture<'a, ()> + Send + Sync>;

pub enum HandlerOutcome {
    Continue,
    Reconnect,
    Stop,
}

#[derive(Clone, Debug)]
pub struct ContextState {
    pub context: String,
}

impl ContextState {
    pub fn new(context: impl Into<String>) -> Self {
        Self {
            context: context.into(),
        }
    }
}
