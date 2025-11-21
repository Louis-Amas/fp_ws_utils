use anyhow::Result;
use chrono::Utc;
use frunk::hlist::Selector;
use std::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use crate::types::HandlerOutcome;
use crate::state::{LastMsg, Heartbeat};

pub fn log_text<S, I>(state: &mut S, msg: &Message) -> Result<HandlerOutcome>
where
    S: Selector<LastMsg, I>,
{
    if let Message::Text(t) = msg {
        println!("📜 Received: {}", t.as_str());
        let last: &mut LastMsg = state.get_mut();
        last.last_msg = Utc::now();
    }
    Ok(HandlerOutcome::Continue)
}

pub fn update_pong<S, I>(state: &mut S, msg: &Message) -> Result<HandlerOutcome>
where
    S: Selector<Heartbeat, I>,
{
    if matches!(msg, Message::Pong(_)) {
        let hb: &mut Heartbeat = state.get_mut();
        hb.last_pong = Instant::now();
    }
    Ok(HandlerOutcome::Continue)
}
