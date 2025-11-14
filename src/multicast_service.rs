use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    net::UdpSocket,
    sync::RwLock,
    task,
    time::{self, interval},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Announcement {
    pub name: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct Peer {
    pub addr: SocketAddr,
    pub name: String,
    pub port: u16,
    #[serde(skip)]
    pub last_seen: Instant,
}

const MULTICAST_ADDR: &str = "239.255.255.250";
const MULTICAST_PORT: u16 = 9999;
const ANNOUNCE_INTERVAL_SECS: u64 = 2;
const PEER_TIMEOUT_SECS: u64 = 2;

pub struct LanDiscovery {
    peers: Arc<RwLock<HashMap<String, Peer>>>,
    announce_socket: UdpSocket,
    listen_socket: UdpSocket,
    pub announce_payload: Arc<RwLock<Announcement>>,
}

impl LanDiscovery {
    pub async fn new(service_port: u16, player_name: String) -> anyhow::Result<Self> {
        let multicast: Ipv4Addr = MULTICAST_ADDR.parse()?;
        let local_ip = get_local_ipv4()?;
        println!("Local interface: {}", local_ip);

        // Announce socket
        let announce_socket = {
            let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
            socket.set_reuse_address(true)?;
            socket.set_multicast_loop_v4(true)?;
            socket.set_ttl_v4(1)?;
            let bind_addr = SocketAddr::new(IpAddr::V4(local_ip), 0);
            socket.bind(&bind_addr.into())?;
            socket.set_multicast_if_v4(&local_ip)?;
            socket.set_nonblocking(true)?;
            UdpSocket::from_std(socket.into())?
        };

        // Listen socket
        let listen_socket = {
            let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
            socket.set_reuse_address(true)?;
            #[cfg(unix)]
            socket.set_reuse_port(true).ok();
            let bind_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), MULTICAST_PORT);
            socket.bind(&bind_addr.into())?;
            socket.join_multicast_v4(&multicast, &local_ip)?;
            socket.set_multicast_loop_v4(true)?;
            socket.set_ttl_v4(1)?;
            socket.set_nonblocking(true)?;
            UdpSocket::from_std(socket.into())?
        };

        let announce_payload = Announcement {
            name: player_name,
            port: service_port,
        };

        Ok(Self {
            peers: Arc::new(RwLock::new(HashMap::new())),
            announce_socket,
            listen_socket,
            announce_payload: Arc::new(RwLock::new(announce_payload)),
        })
    }

    pub async fn start(self: Arc<Self>) {
        let announcer = self.clone();
        let listener = self.clone();

        // Announcer task
        task::spawn(async move {
            announcer.run_announcer().await;
        });

        // Listener task
        task::spawn(async move {
            listener.run_listener().await;
        });

        // Cleanup expired peers
        let peers_ref = self.peers.clone();
        task::spawn(async move {
            let mut interval = interval(Duration::from_secs(3));
            loop {
                interval.tick().await;
                let mut peers = peers_ref.write().await;
                peers.retain(|_, peer| {
                    peer.last_seen.elapsed() < Duration::from_secs(PEER_TIMEOUT_SECS)
                });
            }
        });
    }

    async fn run_announcer(&self) {
        let multicast: Ipv4Addr = MULTICAST_ADDR.parse().unwrap();
        let target = SocketAddr::new(IpAddr::V4(multicast), MULTICAST_PORT);
        let mut interval = time::interval(Duration::from_secs(ANNOUNCE_INTERVAL_SECS));

        loop {
            interval.tick().await;
            let announce = self.announce_payload.read().await;
            if let Ok(data) = serde_json::to_vec(&*announce) {
                if let Err(e) = self.announce_socket.send_to(&data, &target).await {
                    eprintln!("Announce send error: {:?}", e);
                }
            }
        }
    }

    async fn run_listener(&self) {
        let mut buf = [0u8; 4096];
        loop {
            match self.listen_socket.recv_from(&mut buf).await {
                Ok((len, src)) => {
                    if let Ok(msg) = serde_json::from_slice::<Announcement>(&buf[..len]) {
                        let my_name = self.announce_payload.read().await.name.clone();
                        if msg.name == my_name {
                            continue; // skip self
                        }

                        let mut peers = self.peers.write().await;
                        peers.insert(
                            msg.name.clone(),
                            Peer {
                                addr: src,
                                name: msg.name.clone(),
                                port: msg.port,
                                last_seen: Instant::now(),
                            },
                        );
                    } else {
                        println!("Failed to parse announcement from {}", src);
                    }
                }
                Err(e) => eprintln!("Listener error: {:?}", e),
            }
        }
    }

    // Return all alive peers at once
    pub async fn get_peers(&self) -> Vec<Peer> {
        let peers = self.peers.read().await;
        peers.values().cloned().collect()
    }

    // Convenience: serialize current peers as JSON bytes
    pub async fn peers_json(&self) -> Vec<u8> {
        serde_json::to_vec(&self.get_peers().await).unwrap_or_else(|_| Vec::new())
    }
}

// Pick first non-loopback IPv4 interface
fn get_local_ipv4() -> std::io::Result<Ipv4Addr> {
    // Query available addresses and prefer a useful IPv4 address for multicast
    // - skip loopback and link-local (169.254.x.x) addresses
    // - prefer private RFC1918 ranges (10/8, 172.16/12, 192.168/16)
    let addrs = local_ip_address::list_afinet_netifas().map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::Other, format!("failed to list interfaces: {}", e))
    })?;

    // Helper to rate an address: higher is better
    fn score_addr(a: &Ipv4Addr) -> i32 {
        if a.is_loopback() || a.is_link_local() || a.is_multicast() || a.is_unspecified() {
            return -1;
        }
        let octets = a.octets();
        match octets {
            [10, _, _, _] => 80,
            [172, b, _, _] if (16..=31).contains(&b) => 90,
            [192, 168, _, _] => 100,
            _ => 10, // public/global addresses are acceptable but lower priority
        }
    }

    let mut best: Option<Ipv4Addr> = None;
    let mut best_score = -1;

    for (_iface, ip) in &addrs {
        if let IpAddr::V4(v4) = *ip {
            let sc = score_addr(&v4);
            // Skip clearly unsuitable addresses
            if sc < 0 {
                continue;
            }
            // Prefer higher score
            if sc > best_score {
                best_score = sc;
                best = Some(v4);
            }
            // If we found the highest-priority private addr (192.168.x.x),
            // break early. Don't break for lower-priority private addrs so we
            // can still discover a 192.168 address later in the list.
            if best_score >= 100 {
                break;
            }
        } else {
            // ignore IPv6 here
        }
    }

    if let Some(v4) = best {
        return Ok(v4);
    }

    // Fallback: try any non-loopback, non-link-local IPv4
    for (_iface, ip) in &addrs {
        if let IpAddr::V4(v4) = *ip {
            if !v4.is_loopback() && !v4.is_link_local() {
                return Ok(v4);
            }
        }
    }

    // Last resort: return localhost (127.0.0.1)
    Ok(Ipv4Addr::LOCALHOST)
}

pub async fn start_service(player_name : String)  {
    let discovery = Arc::new(LanDiscovery::new(8080, player_name).await.unwrap());
    discovery.clone().start().await;

    println!(
        "LAN Discovery started for {}...",
        discovery.announce_payload.read().await.name
    );

    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let peers = discovery.get_peers().await;
        if !peers.is_empty() {
            println!(
                "{} sees peers: {:#?}",
                discovery.announce_payload.read().await.name,
                peers
            );
        }
    }
}