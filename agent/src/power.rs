//! Sleep, wake and network changes, reported to the core so sessions end
//! in order before this machine sleeps and come back as soon as it wakes or
//! the network changes.
//!
//! - Sleep and wake: logind's `PrepareForSleep` on the system bus.
//! - Network: the set of addresses, read every few seconds with
//!   `getifaddrs`; a change (Wi-Fi joined, DHCP, a VPN, a cable) goes to the
//!   core, which debounces on its own.

use rinflow_core::Core;
use std::collections::BTreeSet;
use std::sync::Weak;
use std::time::Duration;

const NETWORK_POLL: Duration = Duration::from_secs(3);

pub fn spawn(core: Weak<Core>) {
    tokio::task::spawn_local(watch_sleep(core.clone()));
    tokio::task::spawn_local(watch_network(core));
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1Manager {
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

async fn watch_sleep(core: Weak<Core>) {
    use futures_util::StreamExt;
    let connection = match zbus::Connection::system().await {
        Ok(connection) => connection,
        Err(error) => {
            log::info!("no system bus, so no sleep notifications: {error}");
            return;
        }
    };
    let proxy = match Login1ManagerProxy::new(&connection).await {
        Ok(proxy) => proxy,
        Err(error) => {
            log::info!("logind not reachable: {error}");
            return;
        }
    };
    let mut signals = match proxy.receive_prepare_for_sleep().await {
        Ok(signals) => signals,
        Err(error) => {
            log::info!("cannot listen for PrepareForSleep: {error}");
            return;
        }
    };
    while let Some(signal) = signals.next().await {
        let Ok(args) = signal.args() else { continue };
        let Some(core) = core.upgrade() else { return };
        if args.start {
            log::info!("system is going to sleep");
            let _ = core.system_will_sleep();
        } else {
            log::info!("system woke up");
            let _ = core.network_changed();
        }
    }
}

async fn watch_network(core: Weak<Core>) {
    let mut known = addresses();
    loop {
        tokio::time::sleep(NETWORK_POLL).await;
        let now = addresses();
        if now != known {
            known = now;
            let Some(core) = core.upgrade() else { return };
            log::info!("network addresses changed");
            let _ = core.network_changed();
        }
    }
}

/// Every IPv4 and IPv6 address of every interface that is up, as text.
fn addresses() -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list` with a linked list that freeifaddrs
    // releases; every node is read before that.
    unsafe {
        if libc::getifaddrs(&mut list) != 0 {
            return set;
        }
        let mut node = list;
        while !node.is_null() {
            let entry = &*node;
            if !entry.ifa_addr.is_null() && entry.ifa_flags & (libc::IFF_UP as u32) != 0 {
                let family = i32::from((*entry.ifa_addr).sa_family);
                if family == libc::AF_INET {
                    let address = &*(entry.ifa_addr as *const libc::sockaddr_in);
                    let bytes = address.sin_addr.s_addr.to_ne_bytes();
                    set.insert(format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3]));
                } else if family == libc::AF_INET6 {
                    let address = &*(entry.ifa_addr as *const libc::sockaddr_in6);
                    let bytes = address.sin6_addr.s6_addr;
                    // Link-local addresses come and go with every interface
                    // flap and say nothing about reachability.
                    if bytes[0] == 0xfe && bytes[1] & 0xc0 == 0x80 {
                        node = entry.ifa_next;
                        continue;
                    }
                    set.insert(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
                }
            }
            node = entry.ifa_next;
        }
        libc::freeifaddrs(list);
    }
    set
}
