#![allow(dead_code)]

use std::time::Duration;

use crossterm::event::{Event, KeyEvent};
use tokio::sync::mpsc;

use super::detail::ImageDetail;

#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    Resize(u16, u16),
    Tick,
    ReposPage(Vec<String>, bool),
    ReposError(String),
    TagsPage(String, Vec<String>, bool),
    TagsError(String),
    DetailLoaded {
        repo: String,
        tag: String,
        detail: Box<ImageDetail>,
    },
    DetailError(String),
    DeleteTagSuccess {
        repo: String,
        tag: String,
    },
    DeleteTagError(String),
}

/// Spawn a blocking thread that forwards crossterm events to `tx`.
///
/// The thread exits automatically when `tx` is closed (i.e. when the app quits
/// and the receiver is dropped).
pub fn spawn_event_reader(tx: mpsc::Sender<AppEvent>) {
    std::thread::spawn(move || {
        loop {
            match crossterm::event::poll(Duration::from_millis(20)) {
                Ok(true) => match crossterm::event::read() {
                    Ok(Event::Key(k)) => {
                        if tx.blocking_send(AppEvent::Key(k)).is_err() {
                            break;
                        }
                    }
                    Ok(Event::Resize(w, h))
                        if tx.blocking_send(AppEvent::Resize(w, h)).is_err() =>
                    {
                        break;
                    }
                    Ok(Event::Resize(_, _)) => {}
                    _ => {}
                },
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });
}
