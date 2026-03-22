use ewebsock::{WsEvent, WsMessage, WsReceiver, WsSender};
use shared::messages::{ClientMessage, ServerMessage};
use std::collections::VecDeque;

const INITIAL_RECONNECT_DELAY: u32 = 10; // ~1s at 10Hz repaint
const MAX_RECONNECT_DELAY: u32 = 300; // ~30s at 10Hz repaint

pub struct WsClient {
    sender: Option<WsSender>,
    receiver: Option<WsReceiver>,
    pub incoming: VecDeque<ServerMessage>,
    url: String,
    connected: bool,
    reconnect_cooldown: u32,
    reconnect_delay: u32,
}

impl WsClient {
    pub fn new() -> Self {
        Self {
            sender: None,
            receiver: None,
            incoming: VecDeque::new(),
            url: String::new(),
            connected: false,
            reconnect_cooldown: 0,
            reconnect_delay: INITIAL_RECONNECT_DELAY,
        }
    }

    pub fn connect(&mut self, url: &str) {
        self.url = url.to_string();
        match ewebsock::connect(url, ewebsock::Options::default()) {
            Ok((sender, receiver)) => {
                self.sender = Some(sender);
                self.receiver = Some(receiver);
                self.connected = true;
                log::info!("WebSocket connecting to {url}");
            }
            Err(e) => {
                log::error!("WebSocket connect failed: {e}");
                self.connected = false;
            }
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Poll for new messages and drain into self.incoming
    pub fn poll(&mut self) {
        let mut closed = false;

        if let Some(receiver) = &self.receiver {
            while let Some(event) = receiver.try_recv() {
                match event {
                    WsEvent::Message(WsMessage::Text(text)) => {
                        if let Ok(msg) = serde_json::from_str::<ServerMessage>(&text) {
                            self.incoming.push_back(msg);
                        }
                    }
                    WsEvent::Opened => {
                        log::info!("WebSocket connected");
                        self.connected = true;
                        self.reconnect_delay = INITIAL_RECONNECT_DELAY;
                        self.reconnect_cooldown = 0;
                    }
                    WsEvent::Closed => {
                        log::warn!("WebSocket closed");
                        closed = true;
                    }
                    WsEvent::Error(e) => {
                        log::error!("WebSocket error: {e}");
                        closed = true;
                    }
                    _ => {}
                }
            }
        }

        if closed {
            self.connected = false;
            self.sender = None;
            self.receiver = None;
        }

        // Auto-reconnect with exponential backoff
        if !self.connected && self.sender.is_none() && !self.url.is_empty() {
            if self.reconnect_cooldown > 0 {
                self.reconnect_cooldown -= 1;
            } else {
                self.connect(&self.url.clone());
                if !self.connected {
                    self.reconnect_cooldown = self.reconnect_delay;
                    self.reconnect_delay =
                        (self.reconnect_delay * 2).min(MAX_RECONNECT_DELAY);
                }
            }
        }
    }

    pub fn send(&mut self, msg: &ClientMessage) {
        if let Some(sender) = &mut self.sender {
            if let Ok(json) = serde_json::to_string(msg) {
                sender.send(WsMessage::Text(json));
            }
        }
    }
}
