use ewebsock::{WsEvent, WsMessage, WsReceiver, WsSender};
use shared::messages::{ClientMessage, ServerMessage};
use std::collections::VecDeque;

pub struct WsClient {
    sender: Option<WsSender>,
    receiver: Option<WsReceiver>,
    pub incoming: VecDeque<ServerMessage>,
    url: String,
    connected: bool,
}

impl WsClient {
    pub fn new() -> Self {
        Self {
            sender: None,
            receiver: None,
            incoming: VecDeque::new(),
            url: String::new(),
            connected: false,
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

        // Auto-reconnect
        if !self.connected && self.sender.is_none() && !self.url.is_empty() {
            self.connect(&self.url.clone());
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
