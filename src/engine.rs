use crate::{
    handler::WsHandler,
    types::{Action, ConnectHandler, ContextState, HandlerOutcome, WsStream},
};
use anyhow::Result;
use frunk::hlist::Selector;
use futures::{
    StreamExt,
    future::BoxFuture,
    stream::{BoxStream, SelectAll},
};
use std::{sync::Arc, time::Duration};
use tokio::time::sleep;
use tokio_tungstenite::connect_async;
use tracing::{error, info, warn};

pub fn bind_stream<S, M, St, F>(stream: St, logic: F) -> BoxStream<'static, Action<S>>
where
    S: 'static,
    M: Send + 'static,
    St: futures::Stream<Item = M> + Send + 'static,
    F: for<'a> Fn(&'a mut WsStream, &'a mut S, M) -> BoxFuture<'a, HandlerOutcome>
        + Send
        + Sync
        + 'static,
{
    let logic = Arc::new(logic);

    stream
        .map(move |item| {
            let logic = logic.clone();
            let action: Action<S> = Box::new(move |ws, state| logic(ws, state, item));
            action
        })
        .boxed()
}

pub async fn run_ws_loop<S, H, I>(
    url: String,
    mut state: S,
    on_connect: Vec<ConnectHandler<S>>,
    handler: H,
    input_streams: Vec<BoxStream<'static, Action<S>>>,
) -> Result<()>
where
    H: WsHandler<S>,
    S: Selector<ContextState, I> + Send + 'static,
{
    let mut combined_actions: SelectAll<_> = input_streams.into_iter().collect();

    loop {
        {
            let ctx: &ContextState = state.get();
            info!("[{}] 🔌 Connecting to {url}...", ctx.context);
        }
        let (mut stream, _) = match connect_async(&url).await {
            Ok(s) => s,
            Err(e) => {
                let ctx: &ContextState = state.get();
                error!("[{}] Connection failed: {e}, retrying in 2s…", ctx.context);
                sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        {
            let ctx: &ContextState = state.get();
            info!("[{}] Connected {url} ✅", ctx.context);
        }

        // Run on_connect handlers
        for connect_handler in &on_connect {
            connect_handler(&mut stream, &mut state).await;
        }

        loop {
            tokio::select! {
                maybe_msg = stream.next() => {
                    match maybe_msg {
                        Some(Ok(msg)) => match handler.handle(&mut stream, &mut state, &msg).await? {
                            HandlerOutcome::Continue => {}
                            HandlerOutcome::Reconnect => break,
                            HandlerOutcome::Stop => return Ok(()),
                        },
                        Some(Err(e)) => {
                            let ctx: &ContextState = state.get();
                            error!("[{}] WS Error: {e}", ctx.context);
                            break;
                        }
                        None => { break; }
                    }
                }
                Some(action) = combined_actions.next() => {
                    match action(&mut stream, &mut state).await {
                        HandlerOutcome::Continue => {}
                        HandlerOutcome::Reconnect => break,
                        HandlerOutcome::Stop => return Ok(()),
                    }
                }
            }
        }
        {
            let ctx: &ContextState = state.get();
            warn!("[{}] ⚠️ Connection lost, retrying...", ctx.context);
        }
        sleep(Duration::from_secs(2)).await;
    }
}
