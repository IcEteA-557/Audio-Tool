use std::collections::HashMap;
use std::net::Ipv4Addr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NicKind {
    WiFi,
    DirectCable,
    Ethernet,
    Other,
}

impl NicKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::WiFi => "Wi-Fi",
            Self::DirectCable => "有线直连",
            Self::Ethernet => "以太网",
            Self::Other => "其他",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NicInfo {
    pub name: String,
    pub kind: NicKind,
    pub ipv4: Option<Ipv4Addr>,
    pub netmask: Option<Ipv4Addr>,
    pub hardware_port: String,
    pub active: bool,
}

pub fn classify_nic(name: &str, hardware_port: &str, description: &str) -> NicKind {
    let name_l = name.to_ascii_lowercase();
    if name_l.starts_with("utun")
        || name_l.starts_with("awdl")
        || name_l.starts_with("llw")
        || name_l.starts_with("lo")
        || name_l.starts_with("gif")
        || name_l.starts_with("stf")
        || name_l.starts_with("anpi")
        || name_l.starts_with("ap")
    {
        return NicKind::Other;
    }
    let text = format!("{name} {hardware_port} {description}").to_ascii_lowercase();
    if text.contains("wi-fi")
        || text.contains("wifi")
        || text.contains("wlan")
        || text.contains("wireless")
        || text.contains("airport")
        || text.contains("802.11")
    {
        return NicKind::WiFi;
    }
    if text.contains("thunderbolt")
        || text.contains("remote ndis")
        || text.contains("rndis")
        || text.contains("usb")
    {
        return NicKind::DirectCable;
    }
    if text.contains("ethernet") || name_l.starts_with("en") || name_l.starts_with("eth") {
        return NicKind::Ethernet;
    }
    NicKind::Other
}

pub fn ipv4_same_subnet(ip: Ipv4Addr, mask: Ipv4Addr, other: Ipv4Addr) -> bool {
    let mask = u32::from(mask);
    if mask == 0 {
        return false;
    }
    u32::from(ip) & mask == u32::from(other) & mask
}

pub fn broadcast_v4(ip: Ipv4Addr, mask: Ipv4Addr) -> Ipv4Addr {
    let mask = u32::from(mask);
    Ipv4Addr::from(u32::from(ip) | !mask)
}

pub fn transport_ip(nics: &[NicInfo], prefer_wired: bool) -> Ipv4Addr {
    if prefer_wired {
        if let Some(index) = select_transport(nics, true) {
            if nics[index].kind == NicKind::DirectCable {
                if let Some(ip) = nics[index].ipv4 {
                    return ip;
                }
            }
        }
    }
    Ipv4Addr::UNSPECIFIED
}

pub fn select_transport(nics: &[NicInfo], prefer_wired: bool) -> Option<usize> {
    if prefer_wired {
        if let Some(index) = nics.iter().position(|nic| {
            nic.kind == NicKind::DirectCable && nic.ipv4.is_some()
        }) {
            return Some(index);
        }
    }
    nics.iter().position(|nic| {
        nic.ipv4.is_some() && matches!(nic.kind, NicKind::WiFi | NicKind::Ethernet | NicKind::DirectCable)
    })
}

pub fn parse_hardware_ports(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut port = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("Hardware Port:") {
            port = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Device:") {
            if let Some(port) = port.take() {
                map.insert(value.trim().to_string(), port);
            }
        }
    }
    map
}

#[cfg(not(target_os = "windows"))]
pub fn list_nics() -> Vec<NicInfo> {
    let ports = hardware_ports();
    let mut grouped: Vec<NicInfo> = Vec::new();
    let Ok(addrs) = if_addrs::get_if_addrs() else {
        return grouped;
    };
    for iface in addrs {
        if iface.is_loopback() {
            continue;
        }
        let (ipv4, netmask) = match iface.addr {
            if_addrs::IfAddr::V4(addr) => (Some(addr.ip), Some(addr.netmask)),
            if_addrs::IfAddr::V6(_) => (None, None),
        };
        let Some(ipv4) = ipv4 else {
            continue;
        };
        let hardware_port = ports.get(&iface.name).cloned().unwrap_or_default();
        let kind = classify_nic(&iface.name, &hardware_port, "");
        if let Some(existing) = grouped.iter_mut().find(|nic| nic.name == iface.name) {
            if existing.ipv4.is_none() {
                existing.ipv4 = Some(ipv4);
                existing.netmask = netmask;
            }
            continue;
        }
        grouped.push(NicInfo {
            name: iface.name,
            kind,
            ipv4: Some(ipv4),
            netmask,
            hardware_port,
            active: false,
        });
    }
    grouped.sort_by(|a, b| a.name.cmp(&b.name));
    grouped
}

#[cfg(target_os = "macos")]
fn hardware_ports() -> HashMap<String, String> {
    let Ok(output) = std::process::Command::new("networksetup")
        .arg("-listallhardwareports")
        .output()
    else {
        return HashMap::new();
    };
    if !output.status.success() {
        return HashMap::new();
    }
    parse_hardware_ports(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn hardware_ports() -> HashMap<String, String> {
    HashMap::new()
}

#[cfg(target_os = "windows")]
fn windows_nics() -> Vec<WindowsNic> {
    use windows::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
    use windows::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_INCLUDE_PREFIX, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
    const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;

    let mut size = 15_000u32;
    let mut buffer = vec![0u8; size as usize];
    let mut nics = Vec::new();
    unsafe {
        let result = GetAdaptersAddresses(
            AF_INET.0 as u32,
            GAA_FLAG_INCLUDE_PREFIX,
            None,
            Some(buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
            &mut size,
        );
        if result == ERROR_BUFFER_OVERFLOW.0 {
            buffer.resize(size as usize, 0);
            let result = GetAdaptersAddresses(
                AF_INET.0 as u32,
                GAA_FLAG_INCLUDE_PREFIX,
                None,
                Some(buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
                &mut size,
            );
            if result != 0 {
                return nics;
            }
        } else if result != 0 {
            return nics;
        }
        let mut current = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !current.is_null() {
            let adapter = &*current;
            if adapter.IfType != IF_TYPE_SOFTWARE_LOOPBACK {
                let friendly = wide_string(adapter.FriendlyName);
                let description = wide_string(adapter.Description);
                let mut unicast = adapter.FirstUnicastAddress;
                while !unicast.is_null() {
                    let address = &*unicast;
                    let sockaddr = address.Address.lpSockaddr;
                    if !sockaddr.is_null() && (*sockaddr).sa_family == AF_INET {
                        let v4 = &*(sockaddr as *const SOCKADDR_IN);
                        let ip = Ipv4Addr::from(u32::from_be(v4.sin_addr.S_un.S_addr));
                        let prefix = address.OnLinkPrefixLength;
                        nics.push(WindowsNic {
                            name: friendly.clone(),
                            description: description.clone(),
                            ipv4: ip,
                            netmask: prefix_mask(prefix),
                            if_type: adapter.IfType,
                        });
                    }
                    unicast = address.Next;
                }
            }
            current = adapter.Next;
        }
    }
    nics
}

#[cfg(target_os = "windows")]
struct WindowsNic {
    name: String,
    description: String,
    ipv4: Ipv4Addr,
    netmask: Ipv4Addr,
    if_type: u32,
}

#[cfg(target_os = "windows")]
fn prefix_mask(prefix: u8) -> Ipv4Addr {
    if prefix == 0 {
        return Ipv4Addr::UNSPECIFIED;
    }
    let shift = 32 - u32::from(prefix.min(32));
    Ipv4Addr::from(u32::MAX.wrapping_shl(shift))
}

#[cfg(target_os = "windows")]
fn wide_string(value: windows::core::PWSTR) -> String {
    if value.is_null() {
        return String::new();
    }
    unsafe { value.to_string().unwrap_or_default() }
}

#[cfg(target_os = "windows")]
pub fn list_nics() -> Vec<NicInfo> {
    windows_nics()
        .into_iter()
        .map(|nic| {
            let hardware = if nic.if_type == 71 {
                "Wi-Fi".to_string()
            } else {
                String::new()
            };
            NicInfo {
                name: nic.name.clone(),
                kind: classify_nic(&nic.name, &hardware, &nic.description),
                ipv4: Some(nic.ipv4),
                netmask: Some(nic.netmask),
                hardware_port: if hardware.is_empty() {
                    nic.description
                } else {
                    hardware
                },
                active: false,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_links() {
        assert_eq!(classify_nic("en0", "Wi-Fi", ""), NicKind::WiFi);
        assert_eq!(
            classify_nic("bridge0", "Thunderbolt Bridge", ""),
            NicKind::DirectCable
        );
        assert_eq!(
            classify_nic("en7", "USB 10/100/1000 LAN", ""),
            NicKind::DirectCable
        );
        assert_eq!(
            classify_nic("ethernet", "Ethernet", "Remote NDIS Compatible Device"),
            NicKind::DirectCable
        );
        assert_eq!(classify_nic("en5", "Ethernet", "Intel I226"), NicKind::Ethernet);
        assert_eq!(classify_nic("utun0", "VPN", ""), NicKind::Other);
    }

    #[test]
    fn parses_hardware_ports_and_subnet() {
        let text = "\
Hardware Port: Wi-Fi
Device: en0
Ethernet Address: aa

Hardware Port: Thunderbolt Bridge
Device: bridge0
Ethernet Address: bb
";
        let ports = parse_hardware_ports(text);
        assert_eq!(ports.get("en0").map(String::as_str), Some("Wi-Fi"));
        assert_eq!(
            ports.get("bridge0").map(String::as_str),
            Some("Thunderbolt Bridge")
        );
        let ip = Ipv4Addr::new(192, 168, 1, 5);
        let mask = Ipv4Addr::new(255, 255, 255, 0);
        assert!(ipv4_same_subnet(ip, mask, Ipv4Addr::new(192, 168, 1, 9)));
        assert!(!ipv4_same_subnet(ip, mask, Ipv4Addr::new(10, 0, 0, 2)));
        assert_eq!(broadcast_v4(ip, mask), Ipv4Addr::new(192, 168, 1, 255));
    }

    #[test]
    fn prefers_direct_cable_when_asked() {
        let nics = vec![
            NicInfo {
                name: "en0".into(),
                kind: NicKind::WiFi,
                ipv4: Some(Ipv4Addr::new(192, 168, 1, 5)),
                netmask: None,
                hardware_port: "Wi-Fi".into(),
                active: false,
            },
            NicInfo {
                name: "bridge0".into(),
                kind: NicKind::DirectCable,
                ipv4: Some(Ipv4Addr::new(169, 254, 1, 2)),
                netmask: None,
                hardware_port: "Thunderbolt Bridge".into(),
                active: false,
            },
        ];
        assert_eq!(select_transport(&nics, true), Some(1));
        assert_eq!(select_transport(&nics, false), Some(0));
    }
}
