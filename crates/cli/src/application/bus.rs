//! In-process event bus: closed [`DomainEvent`] vocabulary, best-effort fan-out to subscriber
//! channels. Publishers never block on subscribers.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Mutex;

use companion_core::domain::events::DomainEvent;

#[derive(Default)]
pub struct EventBus {
    subscribers: Mutex<Vec<Sender<DomainEvent>>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribes; the receiver sees events published after this call.
    pub fn subscribe(&self) -> Receiver<DomainEvent> {
        let (sender, receiver) = std::sync::mpsc::channel();
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.push(sender);
        }
        receiver
    }

    /// Publishes to every live subscriber; a slow or closed subscriber is dropped, never
    /// allowed to stall the publisher.
    pub fn publish(&self, event: DomainEvent) {
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.retain(|subscriber| subscriber.send(event.clone()).is_ok());
        }
    }
}
