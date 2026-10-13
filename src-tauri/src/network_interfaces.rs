use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetInterface {
    pub name: String,
    pub ips: Vec<String>,
}

/// 枚举本机网卡上的 IPv4 地址（排除回环）
pub fn list_ipv4_interfaces() -> Vec<NetInterface> {
    let mut out = Vec::new();
    if let Ok(ifaces) = NetworkInterface::show() {
        for iface in ifaces {
            let ips: Vec<String> = iface
                .addr
                .iter()
                .filter_map(|a| match a {
                    network_interface::Addr::V4(v4) => {
                        let ip = v4.ip;
                        if ip.is_loopback() || ip.is_link_local() {
                            None
                        } else {
                            Some(ip.to_string())
                        }
                    }
                    _ => None,
                })
                .collect();
            if !ips.is_empty() {
                out.push(NetInterface {
                    name: iface.name,
                    ips,
                });
            }
        }
    }
    out
}

/// 取实际默认路由使用的物理网卡 IPv4；失败时按稳定排序选择物理候选。
pub fn first_lan_ipv4() -> Option<String> {
    let mut candidates: Vec<String> = list_ipv4_interfaces()
        .into_iter()
        .filter(|iface| !is_virtual_name(&iface.name))
        .flat_map(|i| i.ips)
        .filter(|ip| ip.parse::<Ipv4Addr>().map(is_usable_lan_ipv4).unwrap_or(false))
        .collect();
    candidates.sort();
    if let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) {
        if socket.connect(SocketAddr::from(([1, 1, 1, 1], 80))).is_ok() {
            if let Ok(local) = socket.local_addr() {
                if let SocketAddr::V4(addr) = local {
                    let ip = *addr.ip();
                    if candidates.contains(&ip.to_string()) {
                        return Some(ip.to_string());
                    }
                }
            }
        }
    }
    candidates.into_iter().next()
}

fn is_usable_lan_ipv4(ip: Ipv4Addr) -> bool {
    !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() && !ip.is_multicast()
}

fn is_virtual_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["virtual", "tunnel", "easytier", "tailscale", "hamachi", "zerotier", "wsl", "hyper-v", "vmware", "virtualbox"]
        .iter()
        .any(|part| name.contains(part))
}

