#[derive(Clone, Debug)]
pub struct Conn {
    pub connected: bool,
}

impl Default for Conn {
    fn default() -> Self {
        Self { connected: false }
    }
}
