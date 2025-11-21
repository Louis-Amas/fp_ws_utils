use futures::future::BoxFuture;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub type Action<S> =
    Box<dyn for<'a> FnOnce(&'a mut WsStream, &'a mut S) -> BoxFuture<'a, ()> + Send>;

pub enum HandlerOutcome {
    Continue,
    Reconnect,
    Stop,
}
