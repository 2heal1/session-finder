use crate::cli::Request;
use std::{
    io::{self, IsTerminal, Write},
    sync::mpsc::{self, Sender},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// A stderr-only status line. Dropping the guard joins and clears it before
/// output/errors; quick queries never draw a line and need no terminal cleanup.
pub(crate) struct Loading {
    sender: Sender<&'static str>,
    thread: Option<JoinHandle<()>>,
}

impl Loading {
    pub fn start(request: &Request) -> Option<Self> {
        Self::start_since(request, Instant::now())
    }

    pub fn start_since(request: &Request, start: Instant) -> Option<Self> {
        if request.options.json
            || request.options.no_progress
            || !io::stdout().is_terminal()
            || !io::stderr().is_terminal()
            || std::env::var("TERM").is_ok_and(|v| v == "dumb")
        {
            return None;
        }
        let (sender, receiver) = mpsc::channel();
        let thread = thread::spawn(move || {
            let visible_after = Instant::now() + Duration::from_millis(150);
            let mut phase = "Reading session catalog";
            let mut frame = 0;
            let mut drawn = false;
            loop {
                match receiver.recv_timeout(Duration::from_millis(100)) {
                    Ok("") | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Ok(next) => phase = next,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if Instant::now() < visible_after {
                    continue;
                }
                let mut stderr = io::stderr().lock();
                let _ = write!(
                    stderr,
                    "\r\x1b[2K{} {} · {:.1}s",
                    ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"][frame % 8],
                    phase,
                    start.elapsed().as_secs_f64()
                );
                let _ = stderr.flush();
                frame += 1;
                drawn = true;
            }
            if drawn {
                let mut stderr = io::stderr().lock();
                let _ = write!(stderr, "\r\x1b[2K");
                let _ = stderr.flush();
            }
        });
        Some(Self {
            sender,
            thread: Some(thread),
        })
    }

    pub fn phase(&self, phase: &'static str) {
        let _ = self.sender.send(phase);
    }
}

impl Drop for Loading {
    fn drop(&mut self) {
        let _ = self.sender.send("");
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
