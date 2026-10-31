use super::*;
use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};

struct Connection {
    id: u64,
    stream: UnixStream,
    buffer: Vec<u8>,
}
#[derive(Default)]
pub struct AbstractControl {
    listener: Option<UnixListener>,
    connection: Option<Connection>,
    next_id: u64,
}
impl ControlChannel for AbstractControl {
    fn bind_abstract(&mut self, name: &str) -> Result<()> {
        let listener =
            UnixListener::bind_addr(&control_address(name)?).map_err(|_| Failure::Control)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| Failure::Control)?;
        self.listener = Some(listener);
        Ok(())
    }
    fn next_event(&mut self) -> Result<ControlEvent> {
        match self.listener.as_ref().ok_or(Failure::Control)?.accept() {
            Ok((stream, _)) if self.connection.is_some() => {
                // Drop the second connection immediately; never read a byte from it.
                let _ = stream.shutdown(Shutdown::Both);
            }
            Ok((stream, _)) => {
                stream.set_nonblocking(true).map_err(|_| Failure::Control)?;
                self.next_id += 1;
                self.connection = Some(Connection {
                    id: self.next_id,
                    stream,
                    buffer: Vec::new(),
                });
                return Ok(ControlEvent::Connected(self.next_id));
            }
            Err(error)
                if error.kind() == ErrorKind::WouldBlock
                    || error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return Err(Failure::Control),
        }
        let Some(connection) = &mut self.connection else {
            return Ok(ControlEvent::Idle);
        };
        if !connection.buffer.contains(&b'\n') {
            let mut bytes = [0; MAX_CONTROL_FRAME];
            let remaining = MAX_CONTROL_FRAME - connection.buffer.len();
            let disconnected = match connection.stream.read(&mut bytes[..remaining]) {
                Ok(0) => true,
                Ok(length) => {
                    connection.buffer.extend_from_slice(&bytes[..length]);
                    false
                }
                Err(error)
                    if error.kind() == ErrorKind::WouldBlock
                        || error.kind() == ErrorKind::Interrupted =>
                {
                    false
                }
                Err(_) => true,
            };
            if disconnected
                || (connection.buffer.len() >= MAX_CONTROL_FRAME
                    && !connection.buffer.contains(&b'\n'))
            {
                let id = connection.id;
                self.connection = None;
                return Ok(ControlEvent::Disconnected(id));
            }
        }
        if let Some(end) = connection.buffer.iter().position(|byte| *byte == b'\n') {
            let event = parse_control_frame(connection.id, &connection.buffer[..end]);
            connection.buffer.drain(..=end);
            return Ok(event);
        }
        Ok(ControlEvent::Idle)
    }
    fn reply(&mut self, id: u64, reply: ControlReply) -> Result<()> {
        let Some(connection) = &mut self.connection else {
            return Err(Failure::Control);
        };
        if connection.id != id {
            return Err(Failure::Control);
        }
        if reply == ControlReply::Close {
            let _ = connection.stream.shutdown(Shutdown::Both);
            self.connection = None;
            return Ok(());
        }
        let text = match reply {
            ControlReply::Accepted => "connected\n",
            ControlReply::Ready(true) => "ready\n",
            ControlReply::Ready(false) => "not-ready\n",
            ControlReply::Build(BuildOutcome::Started) => "started\n",
            ControlReply::Build(BuildOutcome::Finished(true)) => "succeeded\n",
            ControlReply::Build(BuildOutcome::Finished(false)) => "failed\n",
            ControlReply::Build(BuildOutcome::NotReady) => "not-ready\n",
            _ => "refused\n",
        };
        connection
            .stream
            .write_all(text.as_bytes())
            .map_err(|_| Failure::Control)
    }
}
