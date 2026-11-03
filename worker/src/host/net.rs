//! Address classification for the egress gate. Embedded IPv4 follows the embedded address,
//! so a wrapper (v4-mapped, NAT64, 6to4) never hides a loopback or metadata target.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressClass {
    Public,
    Loopback,
    Private,
    /// 100.64.0.0/10 shared address space.
    Shared,
    UniqueLocal,
    LinkLocal,
    /// Cloud instance-metadata endpoints.
    Metadata,
    Unspecified,
    Multicast,
    Broadcast,
    /// Documentation, benchmarking, Teredo, future-use and similar special ranges.
    Reserved,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reach {
    Open,
    /// Reachable only through an allow-list entry that is the IP literal itself.
    GrantOnly,
    /// Never reachable, whatever the grant.
    Never,
}

pub fn reach(class: AddressClass) -> Reach {
    match class {
        AddressClass::Public => Reach::Open,
        AddressClass::Loopback
        | AddressClass::Private
        | AddressClass::Shared
        | AddressClass::UniqueLocal => Reach::GrantOnly,
        _ => Reach::Never,
    }
}

pub fn classify(ip: IpAddr) -> AddressClass {
    match ip {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

fn classify_v4(ip: Ipv4Addr) -> AddressClass {
    let [a, b, c, _] = ip.octets();
    match (a, b, c) {
        _ if ip == Ipv4Addr::BROADCAST => AddressClass::Broadcast,
        (169, 254, 169) if ip.octets()[3] == 254 => AddressClass::Metadata,
        (100, 100, 100) if ip.octets()[3] == 200 => AddressClass::Metadata,
        (0, ..) => AddressClass::Unspecified,
        (10, ..) | (192, 168, _) => AddressClass::Private,
        (172, 16..=31, _) => AddressClass::Private,
        (100, 64..=127, _) => AddressClass::Shared,
        (127, ..) => AddressClass::Loopback,
        (169, 254, _) => AddressClass::LinkLocal,
        (192, 0, 0 | 2) | (198, 51, 100) | (203, 0, 113) | (198, 18 | 19, _) => {
            AddressClass::Reserved
        }
        (224..=239, ..) => AddressClass::Multicast,
        (240.., ..) => AddressClass::Reserved,
        _ => AddressClass::Public,
    }
}

fn classify_v6(ip: Ipv6Addr) -> AddressClass {
    let s = ip.segments();
    let embedded = |hi: u16, lo: u16| {
        classify_v4(Ipv4Addr::new(
            (hi >> 8) as u8,
            hi as u8,
            (lo >> 8) as u8,
            lo as u8,
        ))
    };
    if ip == Ipv6Addr::UNSPECIFIED {
        AddressClass::Unspecified
    } else if ip == Ipv6Addr::LOCALHOST {
        AddressClass::Loopback
    } else if ip == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254) {
        AddressClass::Metadata
    } else if s[..5] == [0; 5] && s[5] == 0xffff {
        // ::ffff:a.b.c.d (v4-mapped)
        embedded(s[6], s[7])
    } else if s[..6] == [0; 6] {
        // ::a.b.c.d (deprecated v4-compatible)
        embedded(s[6], s[7])
    } else if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6] == [0; 4] {
        // 64:ff9b::/96 (NAT64)
        embedded(s[6], s[7])
    } else if s[0] == 0x2002 {
        // 2002::/16 (6to4)
        embedded(s[1], s[2])
    } else if s[0] == 0x2001 && s[1] == 0 {
        // 2001::/32 (Teredo)
        AddressClass::Reserved
    } else if s[0] == 0x2001 && s[1] == 0x0db8 {
        AddressClass::Reserved
    } else if s[0] & 0xff00 == 0xff00 {
        AddressClass::Multicast
    } else if s[0] & 0xffc0 == 0xfe80 {
        AddressClass::LinkLocal
    } else if s[0] & 0xffc0 == 0xfec0 || s[0] & 0xfe00 == 0xfc00 {
        AddressClass::UniqueLocal
    } else {
        AddressClass::Public
    }
}
