use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

/// A `MakeWriter` for `tracing_subscriber` that routes log events to a chat
/// channel when debug is enabled. When disabled, events are silently discarded.
pub struct ChatMakeWriter {
    enabled: Arc<AtomicBool>,
    sender: mpsc::UnboundedSender<String>,
}

/// The writer that buffers a single log event and sends it to the channel on flush.
pub struct ChatWriter {
    enabled: bool,
    sender: mpsc::UnboundedSender<String>,
    buffer: Vec<u8>,
}

impl Write for ChatWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if !self.enabled {
            return Ok(buf.len());
        }
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        if !self.buffer.is_empty() {
            let s = String::from_utf8_lossy(&self.buffer);
            let trimmed = s.trim_end();
            if !trimmed.is_empty() {
                let _ = self.sender.send(trimmed.to_string());
            }
            self.buffer.clear();
        }
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ChatMakeWriter {
    type Writer = ChatWriter;

    fn make_writer(&'a self) -> Self::Writer {
        ChatWriter {
            enabled: self.enabled.load(Ordering::Relaxed),
            sender: self.sender.clone(),
            buffer: Vec::new(),
        }
    }
}

impl ChatMakeWriter {
    pub fn new(enabled: Arc<AtomicBool>, sender: mpsc::UnboundedSender<String>) -> Self {
        Self { enabled, sender }
    }
}
