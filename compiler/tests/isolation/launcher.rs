use super::support::{MockKernel, TempRoot};
use falcone_serverless_compiler::isolate::*;

#[test]
fn one_control_connection_and_one_build_with_fresh_workspaces_and_cleanup() {
    let root = TempRoot::new();
    let mut launcher = Launcher::new(root.config(), MockKernel::default());
    assert!(launcher.check_readiness().ready);
    assert!(launcher.connect(1));
    assert!(!launcher.connect(2));
    assert_eq!(launcher.build(2, Profile::Rust), BuildOutcome::Refused);
    assert_eq!(launcher.build(1, Profile::Rust), BuildOutcome::Started);
    let first = launcher.kernel().spawned[0].workspace.clone();
    assert!(first.is_dir());
    assert_eq!(launcher.build(1, Profile::Go), BuildOutcome::Refused);
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
    launcher.kernel_mut().finished = Some(true);
    assert_eq!(launcher.poll().unwrap(), BuildOutcome::Finished(true));
    assert!(!first.exists());
    launcher.kernel_mut().finished = None;
    assert_eq!(launcher.build(1, Profile::Go), BuildOutcome::Started);
    let second = launcher.kernel().spawned[1].workspace.clone();
    assert_ne!(first, second);
    launcher.kernel_mut().finished = Some(false);
    assert_eq!(launcher.poll().unwrap(), BuildOutcome::Finished(false));
    assert!(!second.exists());
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
}

#[test]
fn spawn_failure_and_disconnect_clean_up_and_disconnect_is_terminal() {
    let root = TempRoot::new();
    let mut launcher = Launcher::new(root.config(), MockKernel::default());
    launcher.connect(1);
    launcher.kernel_mut().fail = Some(Failure::Spawn);
    assert_eq!(launcher.build(1, Profile::Python), BuildOutcome::NotReady);
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
    launcher.kernel_mut().fail = None;
    assert_eq!(launcher.build(1, Profile::Python), BuildOutcome::Started);
    launcher.disconnect(2).unwrap();
    assert!(!launcher.exited());
    launcher.disconnect(1).unwrap();
    assert!(launcher.exited());
    assert!(!launcher.connect(3));
    assert_eq!(launcher.build(1, Profile::Rust), BuildOutcome::Refused);
    assert!(launcher.kernel().events.contains(&"terminate"));
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
}

#[derive(Default)]
struct MockChannel {
    bound: Vec<String>,
    events: std::collections::VecDeque<ControlEvent>,
    replies: Vec<(u64, ControlReply)>,
}
impl ControlChannel for MockChannel {
    fn bind_abstract(&mut self, name: &str) -> Result<()> {
        self.bound.push(name.into());
        Ok(())
    }
    fn next_event(&mut self) -> Result<ControlEvent> {
        Ok(self.events.pop_front().unwrap_or(ControlEvent::Idle))
    }
    fn reply(&mut self, connection: u64, reply: ControlReply) -> Result<()> {
        self.replies.push((connection, reply));
        Ok(())
    }
}
#[test]
fn transport_binds_only_abstract_name_closes_second_connection_and_exits_on_drop() {
    let root = TempRoot::new();
    let launcher = Launcher::new(root.config(), MockKernel::default());
    let channel = MockChannel {
        events: [
            ControlEvent::Connected(1),
            ControlEvent::Connected(2),
            ControlEvent::Build(1, Profile::Rust),
            ControlEvent::Build(1, Profile::Go),
            ControlEvent::Disconnected(1),
        ]
        .into(),
        ..Default::default()
    };
    let mut server = ControlServer::new(launcher, channel);
    server.start().unwrap();
    for _ in 0..4 {
        assert!(server.tick().unwrap());
    }
    assert!(!server.tick().unwrap());
    let (launcher, channel) = server.into_parts();
    assert_eq!(channel.bound, ["compiler-control"]);
    assert_eq!(
        channel.replies,
        [
            (1, ControlReply::Accepted),
            (2, ControlReply::Close),
            (1, ControlReply::Build(BuildOutcome::Started)),
            (1, ControlReply::Build(BuildOutcome::Refused))
        ]
    );
    assert!(launcher.exited());
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 0);
}

#[test]
fn control_address_is_abstract_and_protocol_accepts_only_fixed_profiles() {
    use std::os::linux::net::SocketAddrExt;
    let address = control_address("compiler-control").unwrap();
    assert_eq!(
        address.as_abstract_name(),
        Some(b"compiler-control".as_slice())
    );
    assert!(address.as_pathname().is_none());
    assert!(control_address("/tmp/control.sock").is_err());
    assert_eq!(parse_control_frame(1, b"READY"), ControlEvent::Readiness(1));
    assert_eq!(
        parse_control_frame(1, b"BUILD rust"),
        ControlEvent::Build(1, Profile::Rust)
    );
    for frame in [
        b"BUILD /tmp/tenant-binary".as_slice(),
        b"BUILD rust --hook",
        b"BUILD rust\0",
        b"ENV SECRET=value",
        b"BUILD unsupported",
    ] {
        assert_eq!(parse_control_frame(1, frame), ControlEvent::Invalid(1));
    }
}
