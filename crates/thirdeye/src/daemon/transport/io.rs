//! A transport over any reader and writer.
//!
//! Covers the PoC's unix socket, and a named pipe or a TCP stream unchanged —
//! which is the point of the seam being `Read + Write` rather than a socket.

use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Write};

use super::Transport;

/// A transport over anything readable and writable — in practice a
/// `UnixStream` and a clone of it.
pub struct IoTransport<R, W> {
    reader: BufReader<R>,
    writer: W,
}

impl<R: std::io::Read, W: Write> IoTransport<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
        }
    }
}

impl<R: std::io::Read, W: Write> Transport for IoTransport<R, W> {
    fn send_line(&mut self, line: &str) -> Result<()> {
        writeln!(self.writer, "{line}").context("writing a message")?;
        self.writer.flush().context("flushing a message")
    }

    fn recv_line(&mut self) -> Result<Option<String>> {
        let mut line = String::new();
        let read = self
            .reader
            .read_line(&mut line)
            .context("reading a message")?;
        if read == 0 {
            return Ok(None);
        }
        Ok(Some(line.trim_end().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_io_transport_strips_the_delimiter_it_added() {
        // Round-tripping through the real implementation: what `send_line`
        // writes, `recv_line` must hand back without the newline.
        let mut written = Vec::new();
        {
            let mut out = IoTransport::new(&b""[..], &mut written);
            out.send_line(r#"{"message":"close"}"#).unwrap();
        }
        assert_eq!(written, b"{\"message\":\"close\"}\n");

        let mut input = IoTransport::new(&written[..], Vec::new());
        assert_eq!(
            input.recv_line().unwrap().as_deref(),
            Some(r#"{"message":"close"}"#)
        );
        assert_eq!(input.recv_line().unwrap(), None, "EOF reads as hangup");
    }
}
