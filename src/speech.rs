//! Read-aloud backend for the `tts-native` branch: the OS-native speech
//! engine via the `tts` crate (NSSS on macOS, SAPI on Windows, Speech
//! Dispatcher on Linux). No models, no network, no audio files.
//!
//! The public shape of this module (``SpeechEngine::new/speak/stop``,
//! ``poll_finished/take_error``, ``blocks_to_speech``) is intentionally
//! identical to the `piper-neural` branch so the two can be compared fairly
//! and the winner merged easily.

use crate::epub_utils::Block;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

/// Flatten a chapter label plus blocks into speakable plain text. Images
/// carry no text and are skipped; everything else reads in order.
pub fn blocks_to_speech(label: &str, blocks: &[Block]) -> String {
    let mut parts = Vec::new();
    if !label.trim().is_empty() {
        parts.push(label.trim().to_string());
    }
    for b in blocks {
        let s = match b {
            Block::Heading(_, t)
            | Block::Paragraph(t)
            | Block::Quote(t)
            | Block::ListItem(t)
            | Block::Pre(t) => t.trim(),
            Block::Image { .. } => "",
        };
        if !s.is_empty() {
            parts.push(s.to_string());
        }
    }
    parts.join("\n\n")
}

enum Cmd {
    Speak { text: String, gen: u64 },
    Stop,
}

enum Ev {
    Ready(Result<(), String>),
    Finished(u64),
    Error(String),
}

/// OS TTS engine handle. All backend calls run on one dedicated worker
/// thread (the `tts` handle is `Rc`-based: never touch it from two threads,
/// and callbacks only forward an id through a channel).
pub struct SpeechEngine {
    tx: Sender<Cmd>,
    rx: Receiver<Ev>,
    gen: u64,
    ready: bool,
    error: Option<String>,
    finished: bool,
}

impl Default for SpeechEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeechEngine {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let (ev_tx, ev_rx) = mpsc::channel::<Ev>();
        std::thread::spawn(move || Self::worker(cmd_rx, ev_tx));
        Self {
            tx: cmd_tx,
            rx: ev_rx,
            gen: 0,
            ready: false,
            error: None,
            finished: false,
        }
    }

    /// Speak `text`, interrupting anything currently playing.
    pub fn speak(&mut self, text: String) {
        self.gen += 1;
        let _ = self.tx.send(Cmd::Speak {
            text,
            gen: self.gen,
        });
    }

    /// Stop playback and invalidate any in-flight finish events.
    pub fn stop(&mut self) {
        self.gen += 1;
        let _ = self.tx.send(Cmd::Stop);
    }

    /// Drain backend events. Returns true if an utterance finished since the
    /// last call *for the current generation* (stale finishes from before a
    /// `stop`/new `speak` are ignored).
    pub fn poll_finished(&mut self) -> bool {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Ev::Ready(Ok(())) => self.ready = true,
                Ev::Ready(Err(e)) | Ev::Error(e) => {
                    if self.error.is_none() {
                        self.error = Some(e);
                    }
                }
                Ev::Finished(g) => {
                    if g == self.gen {
                        self.finished = true;
                    }
                }
            }
        }
        std::mem::replace(&mut self.finished, false)
    }

    /// Take the pending backend error, if any (init failure or a failed
    /// `speak`/`stop`), so the app can show it once.
    pub fn take_error(&mut self) -> Option<String> {
        // Pump the channel first: an init failure may not have been drained.
        let _ = self.poll_finished();
        self.error.take()
    }

    /// Whether the backend initialized. Speaking before ready just queues.
    pub fn ready(&self) -> bool {
        self.ready
    }

    fn worker(cmd_rx: Receiver<Cmd>, ev_tx: Sender<Ev>) {
        let mut tts = match tts::Tts::default() {
            Ok(t) => t,
            Err(e) => {
                let _ = ev_tx.send(Ev::Ready(Err(format!("speech engine: {e}"))));
                return;
            }
        };
        // Tag every finish with the generation active when speaking started.
        // The closure only forwards through the channel (never touches `tts`
        // or the UI: callbacks run on backend threads and must not re-enter).
        let current = Arc::new(AtomicU64::new(0));
        let current_cb = current.clone();
        let ev_cb = ev_tx.clone();
        if tts
            .on_utterance_end(Some(Box::new(move |_id| {
                let _ = ev_cb.send(Ev::Finished(current_cb.load(Ordering::SeqCst)));
            })))
            .is_err()
        {
            // No utterance callbacks on this backend: single pages still
            // read, but auto-advance can't fire. Not fatal.
        }
        let _ = ev_tx.send(Ev::Ready(Ok(())));
        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                Cmd::Speak { text, gen } => {
                    current.store(gen, Ordering::SeqCst);
                    // Stop-then-speak: uniform interrupt semantics even where
                    // a backend's own interrupt flag differs.
                    let _ = tts.stop();
                    if let Err(e) = tts.speak(text, false) {
                        let _ = ev_tx.send(Ev::Error(format!("speak failed: {e}")));
                    }
                }
                Cmd::Stop => {
                    if let Err(e) = tts.stop() {
                        // Some backends lack `stop`; not worth surfacing.
                        let _ = e;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speech_blocks() -> Vec<Block> {
        vec![
            Block::Heading(1, "Chapter One".to_string()),
            Block::Paragraph("Hello world.".to_string()),
            Block::Quote("quoted words".to_string()),
            Block::ListItem("first item".to_string()),
            Block::Image {
                bytes: vec![],
                alt: "A pic".to_string(),
                hash: 7,
                w: 10,
                h: 10,
            },
        ]
    }

    #[test]
    fn blocks_flatten_in_order_without_images() {
        let s = blocks_to_speech("Top", &speech_blocks());
        assert_eq!(
            s,
            "Top\n\nChapter One\n\nHello world.\n\nquoted words\n\nfirst item"
        );
        assert!(!s.contains("A pic"));
    }

    #[test]
    fn empty_blocks_give_empty_text() {
        assert_eq!(blocks_to_speech("", &[]), "");
        assert_eq!(blocks_to_speech("  ", &[Block::Paragraph("  ".into())]), "");
    }

    #[test]
    fn engine_constructs_and_stops_safely() {
        // No audio assertions: construction, stop, and polling must never
        // panic, even where no speech backend exists.
        let mut eng = SpeechEngine::new();
        assert!(!eng.poll_finished());
        eng.stop();
        let _ = eng.take_error();
        assert!(!eng.poll_finished());
    }
}
