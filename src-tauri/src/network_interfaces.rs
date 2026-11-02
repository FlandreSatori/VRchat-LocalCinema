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

/// 按 IPv4 在网卡列表中查找网卡的结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterMatchError {
    /// 没有任何网卡配置该 IP（通常表示 EasyTier 虚拟网卡尚未就绪）
    NotFound,
    /// 多张网卡配置了同一 IP，无法唯一确定
    Ambiguous,
}

/// 按给定 IPv4 在网卡列表中查找唯一对应的网卡。
///
/// EasyTier 虚拟 IP 用「EasyTier 报告的本机 IP」来收敛：即使本机残留多张 `et_*`
/// 网卡（历史实例 / 其他网络），也只有当前网络的网卡会配置这个 IP。
/// 若多张网卡共用同一 IP，仅当其中恰好一张是 EasyTier 虚拟网卡时才视为唯一。
pub fn find_adapter_by_ip(
    interfaces: &[NetInterface],
    ip: &str,
) -> Result<NetInterface, AdapterMatchError> {
    let matched: Vec<&NetInterface> = interfaces
        .iter()
        .filter(|iface| iface.ips.iter().any(|candidate| candidate.trim() == ip))
        .collect();
    match matched.len() {
        0 => Err(AdapterMatchError::NotFound),
        1 => Ok(matched[0].clone()),
        _ => {
            let et_matched: Vec<&NetInterface> = matched
                .iter()
                .copied()
                .filter(|iface| is_easytier_adapter_name(&iface.name))
                .collect();
            if et_matched.len() == 1 {
                Ok(et_matched[0].clone())
            } else {
                Err(AdapterMatchError::Ambiguous)
            }
        }
    }
}

fn is_usable_lan_ipv4(ip: Ipv4Addr) -> bool {
    !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() && !ip.is_multicast()
}

/// EasyTier 的 Wintun 虚拟网卡名形如 `et_9_elby`、`EasyTier`、`eton...`；
/// 注意不能用 `et` 前缀匹配，否则会把 `Ethernet`（物理网卡）误判为虚拟网卡。
fn is_easytier_adapter_name(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    name.starts_with("et_")
        || name.starts_with("eton")
        || name.contains("easytier")
        || name.contains("wintun")
}

fn is_virtual_name(name: &str) -> bool {
    is_easytier_adapter_name(name)
        || {
            let name = name.to_ascii_lowercase();
            ["virtual", "tunnel", "tailscale", "hamachi", "zerotier", "wsl", "hyper-v", "vmware", "virtualbox"]
                .iter()
                .any(|part| name.contains(part))
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(name: &str, ips: &[&str]) -> NetInterface {
        NetInterface {
            name: name.to_string(),
            ips: ips.iter().map(|ip| ip.to_string()).collect(),
        }
    }

    #[test]
    fn test_find_adapter_by_ip_unique_among_multiple_easytier_adapters() {
        // 本机残留两张 et_* 网卡（历史实例），只有当前网络那张配置了 EasyTier 报告的本机 IP
        let interfaces = vec![
            iface("Ethernet", &["192.168.1.10"]),
            iface("et_9_elby", &["10.0.0.7"]),
            iface("et_5_uz7x", &["10.126.126.5"]),
        ];
        let matched = find_adapter_by_ip(&interfaces, "10.126.126.5").unwrap();
        assert_eq!(matched.name, "et_5_uz7x");
    }

    #[test]
    fn test_find_adapter_by_ip_not_found() {
        let interfaces = vec![
            iface("Ethernet", &["192.168.1.10"]),
            iface("et_9_elby", &["10.0.0.7"]),
        ];
        assert!(matches!(
            find_adapter_by_ip(&interfaces, "10.126.126.5"),
            Err(AdapterMatchError::NotFound)
        ));
    }

    #[test]
    fn test_find_adapter_by_ip_ambiguous_is_not_guessed() {
        let interfaces = vec![
            iface("et_1_aaaa", &["10.126.126.5"]),
            iface("et_2_bbbb", &["10.126.126.5"]),
        ];
        assert!(matches!(
            find_adapter_by_ip(&interfaces, "10.126.126.5"),
            Err(AdapterMatchError::Ambiguous)
        ));
    }

    #[test]
    fn test_easytier_adapter_name_does_not_match_ethernet() {
        assert!(is_easytier_adapter_name("et_9_elby"));
        assert!(is_easytier_adapter_name("EasyTier"));
        assert!(is_easytier_adapter_name("eton0"));
        assert!(!is_easytier_adapter_name("Ethernet"));
        assert!(!is_easytier_adapter_name("以太网"));
        assert!(is_virtual_name("et_9_elby"));
        assert!(!is_virtual_name("Ethernet"));
    }
}

