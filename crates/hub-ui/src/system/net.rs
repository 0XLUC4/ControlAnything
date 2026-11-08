//! Network adapters (GetAdaptersAddresses) and their byte counters (GetIfEntry2).

use std::net::IpAddr;

/// One network interface, loopback and tunnels excluded.
#[derive(Clone, Debug)]
pub struct Adapter {
    /// Stable interface identifier (NET_LUID).
    pub luid: u64,
    pub name: String,
    pub description: String,
    pub wireless: bool,
    pub up: bool,
    pub addresses: Vec<IpAddr>,
    /// "AA-BB-CC-DD-EE-FF", empty when the interface has none.
    pub mac: String,
    /// Bits per second, `None` when unknown or disconnected.
    pub link_speed: Option<u64>,
    /// Bytes received and sent since boot.
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

pub fn list() -> Result<Vec<Adapter>, String> {
    backend::list().map_err(|err| err.to_string())
}

/// Bytes per second between two readings of a counter (a reset counts as 0).
pub fn rate(before: u64, after: u64, secs: f64) -> f64 {
    if secs <= 0.0 {
        return 0.0;
    }
    after.saturating_sub(before) as f64 / secs
}

/// "512 B", "1.5 KB", "12.3 MB" (decimal units, like link speeds).
pub fn format_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes.max(0.0);
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn format_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

/// "1 Gbps", "866.7 Mbps", or "Unknown".
pub fn format_link_speed(bits_per_sec: Option<u64>) -> String {
    let Some(bps) = bits_per_sec else {
        return "Unknown".into();
    };
    let (value, unit) = if bps >= 1_000_000_000 {
        (bps as f64 / 1e9, "Gbps")
    } else {
        (bps as f64 / 1e6, "Mbps")
    };
    let text = format!("{value:.1}");
    format!("{} {unit}", text.trim_end_matches(".0"))
}

/// Windows reports an unknown speed as `u64::MAX` and a disconnected one as 0.
#[cfg_attr(not(windows), allow(dead_code))]
fn known_speed(bps: u64) -> Option<u64> {
    (bps != 0 && bps != u64::MAX).then_some(bps)
}

#[cfg(not(windows))]
mod backend {
    pub fn list() -> Result<Vec<super::Adapter>, &'static str> {
        Err(crate::system::UNAVAILABLE)
    }
}

#[cfg(windows)]
mod backend {
    use std::net::IpAddr;

    use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR, WIN32_ERROR};
    use windows::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        GetAdaptersAddresses, GetIfEntry2, IF_TYPE_IEEE80211, IF_TYPE_SOFTWARE_LOOPBACK,
        IF_TYPE_TUNNEL, IP_ADAPTER_ADDRESSES_LH, MIB_IF_ROW2,
    };
    use windows::Win32::NetworkManagement::Ndis::{IfOperStatusUp, NET_LUID_LH};
    use windows::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6, SOCKET_ADDRESS,
    };
    use windows::core::{Error, Result};

    use super::{Adapter, known_speed};

    pub fn list() -> Result<Vec<Adapter>> {
        let buffer = adapters_buffer()?;
        let mut adapters = Vec::new();
        let mut node = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        // SAFETY: GetAdaptersAddresses wrote a linked list whose nodes all live in
        // `buffer` (or end with null), and `buffer` outlives the walk.
        while let Some(adapter) = unsafe { node.as_ref() } {
            node = adapter.Next;
            if adapter.IfType != IF_TYPE_SOFTWARE_LOOPBACK && adapter.IfType != IF_TYPE_TUNNEL {
                adapters.push(read_adapter(adapter));
            }
        }
        Ok(adapters)
    }

    /// GetAdaptersAddresses output; `u64` items give the list its 8-byte alignment.
    fn adapters_buffer() -> Result<Vec<u64>> {
        let mut size: u32 = 16 * 1024;
        // Adapters can appear between the size query and the read: retry a few times.
        for _ in 0..3 {
            let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
            // SAFETY: `size` is at most the byte length of `buffer`, the only memory written.
            let code = unsafe {
                GetAdaptersAddresses(
                    AF_UNSPEC.0.into(),
                    GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER,
                    None,
                    Some(buffer.as_mut_ptr().cast()),
                    &mut size,
                )
            };
            match WIN32_ERROR(code) {
                NO_ERROR => return Ok(buffer),
                ERROR_BUFFER_OVERFLOW => continue,
                err => return Err(Error::from(err.to_hresult())),
            }
        }
        Err(Error::from(ERROR_BUFFER_OVERFLOW.to_hresult()))
    }

    fn read_adapter(adapter: &IP_ADAPTER_ADDRESSES_LH) -> Adapter {
        // SAFETY: the strings and the address list point into the same buffer as the
        // adapter (see `list`), null-terminated / null-ended by the API.
        let (name, description) = unsafe {
            (
                adapter.FriendlyName.to_string().unwrap_or_default(),
                adapter.Description.to_string().unwrap_or_default(),
            )
        };
        let mut addresses = Vec::new();
        let mut unicast = adapter.FirstUnicastAddress;
        // SAFETY: same buffer as above.
        while let Some(address) = unsafe { unicast.as_ref() } {
            addresses.extend(ip_addr(&address.Address));
            unicast = address.Next;
        }
        let mac_len = (adapter.PhysicalAddressLength as usize).min(adapter.PhysicalAddress.len());
        let mac = adapter.PhysicalAddress[..mac_len]
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join("-");
        let (rx_bytes, tx_bytes) = octets(adapter.Luid);
        Adapter {
            // SAFETY: NET_LUID_LH is a union over one u64; every bit pattern is valid.
            luid: unsafe { adapter.Luid.Value },
            name,
            description,
            wireless: adapter.IfType == IF_TYPE_IEEE80211,
            up: adapter.OperStatus == IfOperStatusUp,
            addresses,
            mac,
            link_speed: known_speed(adapter.ReceiveLinkSpeed),
            rx_bytes,
            tx_bytes,
        }
    }

    fn ip_addr(socket: &SOCKET_ADDRESS) -> Option<IpAddr> {
        // SAFETY: `lpSockaddr` is null or points to a sockaddr whose real type is given
        // by its family (SOCKADDR_IN for AF_INET, SOCKADDR_IN6 for AF_INET6).
        unsafe {
            let family = socket.lpSockaddr.as_ref()?.sa_family;
            if family == AF_INET {
                let v4 = &*socket.lpSockaddr.cast::<SOCKADDR_IN>();
                // Stored in network order: the in-memory bytes are the octets.
                Some(IpAddr::from(v4.sin_addr.S_un.S_addr.to_ne_bytes()))
            } else if family == AF_INET6 {
                let v6 = &*socket.lpSockaddr.cast::<SOCKADDR_IN6>();
                Some(IpAddr::from(v6.sin6_addr.u.Byte))
            } else {
                None
            }
        }
    }

    /// Received and sent bytes since boot, (0, 0) if the interface vanished.
    fn octets(luid: NET_LUID_LH) -> (u64, u64) {
        let mut row = MIB_IF_ROW2 {
            InterfaceLuid: luid,
            ..Default::default()
        };
        // SAFETY: `row` is a valid MIB_IF_ROW2 with its LUID set, as GetIfEntry2 requires.
        if unsafe { GetIfEntry2(&mut row) }.is_ok() {
            (row.InOctets, row.OutOctets)
        } else {
            (0, 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_per_second() {
        assert_eq!(rate(1_000, 3_000, 2.0), 1_000.0);
        // Counter reset (interface restarted): no negative rate.
        assert_eq!(rate(5_000, 100, 1.0), 0.0);
        assert_eq!(rate(0, 100, 0.0), 0.0);
    }

    #[test]
    fn bytes_formatting() {
        assert_eq!(format_bytes(512.0), "512 B");
        assert_eq!(format_bytes(1_500.0), "1.5 KB");
        assert_eq!(format_bytes(12_340_000.0), "12.3 MB");
        assert_eq!(format_rate(2_000_000_000.0), "2.0 GB/s");
    }

    #[test]
    fn link_speed_formatting() {
        assert_eq!(format_link_speed(Some(1_000_000_000)), "1 Gbps");
        assert_eq!(format_link_speed(Some(866_700_000)), "866.7 Mbps");
        assert_eq!(format_link_speed(Some(100_000_000)), "100 Mbps");
        assert_eq!(format_link_speed(None), "Unknown");
        assert_eq!(known_speed(u64::MAX), None);
        assert_eq!(known_speed(0), None);
    }
}
