use chrono::{DateTime, Utc};
use frunk::{HCons, HNil, hlist};
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct LastMsg {
    pub last_msg: DateTime<Utc>,
}
#[derive(Clone, Debug)]
pub struct Heartbeat {
    pub last_pong: Instant,
}
#[derive(Clone, Debug)]
pub struct Conn {
    pub connected: bool,
}

pub type WsState = HCons<LastMsg, HCons<Heartbeat, HCons<Conn, HNil>>>;

pub fn make_state() -> WsState {
    hlist![
        LastMsg {
            last_msg: Utc::now()
        },
        Heartbeat {
            last_pong: Instant::now()
        },
        Conn { connected: false },
    ]
}
