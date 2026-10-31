use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlEvent {
    Connected(u64),
    Disconnected(u64),
    Readiness(u64),
    Build(u64, Profile),
    Invalid(u64),
    Idle,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlReply {
    Accepted,
    Close,
    Ready(bool),
    Build(BuildOutcome),
}
/// A close reply must close that connection without writing any response.
pub trait ControlChannel {
    fn bind_abstract(&mut self, name: &str) -> Result<()>;
    fn next_event(&mut self) -> Result<ControlEvent>;
    fn reply(&mut self, connection: u64, reply: ControlReply) -> Result<()>;
}
pub struct ControlServer<K, C> {
    launcher: Launcher<K>,
    channel: C,
}
impl<K: KernelInterface, C: ControlChannel> ControlServer<K, C> {
    pub fn new(launcher: Launcher<K>, channel: C) -> Self {
        Self { launcher, channel }
    }
    pub fn into_parts(self) -> (Launcher<K>, C) {
        (self.launcher, self.channel)
    }
    pub fn start(&mut self) -> Result<()> {
        self.launcher.config().validate()?;
        self.launcher.check_readiness();
        self.channel
            .bind_abstract(&self.launcher.config().control_socket)
    }
    pub fn tick(&mut self) -> Result<bool> {
        match self.tick_inner() {
            Ok(running) => Ok(running),
            Err(error) => {
                self.launcher.shutdown()?;
                Err(error)
            }
        }
    }
    fn tick_inner(&mut self) -> Result<bool> {
        if self.launcher.exited() {
            return Ok(false);
        }
        let outcome = self.launcher.poll()?;
        if matches!(outcome, BuildOutcome::Finished(_))
            && let Some(connection) = self.launcher.connection()
        {
            self.channel
                .reply(connection, ControlReply::Build(outcome))?;
        }
        match self.channel.next_event()? {
            ControlEvent::Connected(id) => {
                let reply = if self.launcher.connect(id) {
                    ControlReply::Accepted
                } else {
                    ControlReply::Close
                };
                self.channel.reply(id, reply)?;
            }
            ControlEvent::Disconnected(id) => self.launcher.disconnect(id)?,
            ControlEvent::Readiness(id) => {
                if self.launcher.connection() != Some(id) {
                    self.channel.reply(id, ControlReply::Close)?;
                } else {
                    let ready = self.launcher.check_readiness().ready;
                    self.channel.reply(id, ControlReply::Ready(ready))?;
                }
            }
            ControlEvent::Build(id, profile) => {
                let outcome = self.launcher.build(id, profile);
                self.channel.reply(id, ControlReply::Build(outcome))?;
            }
            ControlEvent::Invalid(id) => self
                .channel
                .reply(id, ControlReply::Build(BuildOutcome::Refused))?,
            ControlEvent::Idle => {}
        }
        Ok(!self.launcher.exited())
    }
}

pub const MAX_CONTROL_FRAME: usize = 64;
pub fn control_address(name: &str) -> Result<std::os::unix::net::SocketAddr> {
    use std::os::linux::net::SocketAddrExt;
    if name.is_empty() || name.len() > 107 || name.contains(['/', '\0']) {
        return Err(Failure::Configuration);
    }
    std::os::unix::net::SocketAddr::from_abstract_name(name).map_err(|_| Failure::Control)
}
/// Internal runner transport only: no request-supplied executable, arguments, env or paths.
/// Service staging/collection and the public build API are DEP-01.t3.
pub fn parse_control_frame(connection: u64, frame: &[u8]) -> ControlEvent {
    if frame.len() >= MAX_CONTROL_FRAME {
        return ControlEvent::Invalid(connection);
    }
    match std::str::from_utf8(frame) {
        Ok("READY") => ControlEvent::Readiness(connection),
        Ok(line) => match line
            .strip_prefix("BUILD ")
            .and_then(|name| Profile::parse(name).ok())
        {
            Some(profile) => ControlEvent::Build(connection, profile),
            None => ControlEvent::Invalid(connection),
        },
        Err(_) => ControlEvent::Invalid(connection),
    }
}
