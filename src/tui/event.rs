use crossterm::event::{Event as CtEvent, EventStream, KeyEvent};
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::perfetto::capture::{CaptureEvent, CaptureResult};
use crate::tui::screens::device_picker::DeviceEntry;

pub enum AppEvent {
    Key(KeyEvent),
    Tick,
    DevicesLoaded(Result<Vec<DeviceEntry>, String>),
    PackagesLoaded(Result<Vec<String>, String>),
    Capture(CaptureEvent),
    CaptureDone(Result<CaptureResult, String>),
}

pub struct EventBus {
    pub rx: mpsc::UnboundedReceiver<AppEvent>,
    pub tx: mpsc::UnboundedSender<AppEvent>,
}

pub fn start() -> EventBus {
    let (tx, rx) = mpsc::unbounded_channel::<AppEvent>();

    let tx_tick = tx.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(250));
        loop {
            ticker.tick().await;
            if tx_tick.send(AppEvent::Tick).is_err() {
                break;
            }
        }
    });

    let tx_key = tx.clone();
    tokio::spawn(async move {
        let mut events = EventStream::new();
        while let Some(Ok(ev)) = events.next().await {
            if let CtEvent::Key(k) = ev {
                if tx_key.send(AppEvent::Key(k)).is_err() {
                    break;
                }
            }
        }
    });

    EventBus { rx, tx }
}
