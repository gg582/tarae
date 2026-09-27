//! Event queue — key input, config changes and job results all arrive through one channel.
//!
//! Principle 4 (non-blocking): slow work runs on a thread via `Jobs::spawn`, and the result comes back as
//! "a closure to apply to the editor" that runs on the main loop. The main thread never blocks except
//! while waiting on the channel.
//! Zero dependencies — std threads + mpsc. (An async runtime only if LSP/LLM streaming proves it necessary.)

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crate::editor::Editor;
use crate::key::Key;

pub type Apply = Box<dyn FnOnce(&mut Editor) + Send>;

pub enum Event {
    Key(Key),
    Mouse(Mouse),
    /// Terminal focus (regained = true).
    Focus(bool),
    Resize,
    Job(Apply),
}

/// One mouse action (screen cell coordinates). Press, drag, release, wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mouse {
    pub kind: MouseKind,
    pub x: u16,
    pub y: u16,
    /// alt+press = add a cursor.
    pub alt: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    Down,
    Drag,
    Up,
    ScrollUp,
    ScrollDown,
}

/// The queue the editor holds. The sending side is cloned and handed out to threads.
pub struct Queue {
    tx: Sender<Event>,
    rx: Receiver<Event>,
}

impl Default for Queue {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self { tx, rx }
    }
}

impl Queue {
    pub fn sender(&self) -> Sender<Event> {
        self.tx.clone()
    }

    pub fn jobs(&self) -> Jobs {
        Jobs { tx: self.tx.clone() }
    }

    pub fn recv(&self) -> Option<Event> {
        self.rx.recv().ok()
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.rx.try_recv().ok()
    }

    #[cfg(test)]
    pub fn recv_timeout(&self, d: std::time::Duration) -> Option<Event> {
        self.rx.recv_timeout(d).ok()
    }
}

#[derive(Clone)]
pub struct Jobs {
    tx: Sender<Event>,
}

impl Jobs {
    /// Runs `work` on a thread and sends its result (a closure to apply to the editor) to the main loop.
    pub fn spawn<W, A>(&self, work: W)
    where
        W: FnOnce() -> A + Send + 'static,
        A: FnOnce(&mut Editor) + Send + 'static,
    {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let apply = work();
            let _ = tx.send(Event::Job(Box::new(apply)));
        });
    }
}
