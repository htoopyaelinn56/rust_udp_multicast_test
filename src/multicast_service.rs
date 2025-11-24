use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use std::sync::atomic::{AtomicBool, Ordering};
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
    pub shutdown: Arc<AtomicBool>, // flag to stop outer service loop and worker tasks
}

static DISCOVERY: OnceCell<Arc<LanDiscovery>> = OnceCell::new();

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
            shutdown: Arc::new(AtomicBool::new(false)),
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
        let shutdown_ref = self.shutdown.clone();
        task::spawn(async move {
            let mut interval = interval(Duration::from_secs(3));
            loop {
                if shutdown_ref.load(Ordering::Relaxed) { break; }
                interval.tick().await;
                if shutdown_ref.load(Ordering::Relaxed) { break; }
                let mut peers = peers_ref.write().await;
                peers.retain(|_, peer| peer.last_seen.elapsed() < Duration::from_secs(PEER_TIMEOUT_SECS));
            }
            // Optional: final cleanup if needed
        });
    }

    async fn run_announcer(&self) {
        let multicast: Ipv4Addr = MULTICAST_ADDR.parse().unwrap();
        let target = SocketAddr::new(IpAddr::V4(multicast), MULTICAST_PORT);
        let mut interval = time::interval(Duration::from_secs(ANNOUNCE_INTERVAL_SECS));
        loop {
            if self.shutdown.load(Ordering::Relaxed) { break; }
            interval.tick().await;
            if self.shutdown.load(Ordering::Relaxed) { break; }
            let announce = self.announce_payload.read().await;
            if let Ok(data) = serde_json::to_vec(&*announce) {
                if let Err(e) = self.announce_socket.send_to(&data, &target).await {
                    eprintln!("Announce send error: {:?}", e);
                }
            }
        }
        // println!("Announcer task stopped");
    }

    async fn run_listener(&self) {
        let mut buf = [0u8; 4096];
        loop {
            if self.shutdown.load(Ordering::Relaxed) { break; }
            tokio::select! {
                biased;
                _ = async {
                    // Simple check path to allow early break before blocking recv
                    if self.shutdown.load(Ordering::Relaxed) { return; }
                } => { break; }
                res = self.listen_socket.recv_from(&mut buf) => {
                    if self.shutdown.load(Ordering::Relaxed) { break; }
                    match res {
                        Ok((len, src)) => {
                            if let Ok(msg) = serde_json::from_slice::<Announcement>(&buf[..len]) {
                                let my_name = self.announce_payload.read().await.name.clone();
                                if msg.name == my_name { continue; }
                                let mut peers = self.peers.write().await;
                                peers.insert(msg.name.clone(), Peer { addr: src, name: msg.name.clone(), port: msg.port, last_seen: Instant::now() });
                            } else {
                                println!("Failed to parse announcement from {}", src);
                            }
                        }
                        Err(e) => {
                            eprintln!("Listener error: {:?}", e);
                        }
                    }
                }
            }
        }
        // println!("Listener task stopped");
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
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("failed to list interfaces: {}", e),
        )
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

/// Get local IPv4 address as a string.
pub fn get_local_ipv4_in_string() -> String {
    match get_local_ipv4() {
        Ok(ip) => ip.to_string(),
        Err(_) => "".to_string(),
    }
}

/// Get current peers as a JSON string from the global discovery instance.
pub async fn get_peers() -> String {
    if let Some(discovery) = DISCOVERY.get().cloned() {
        // Use the instance method to get JSON bytes and convert to String.
        let bytes = discovery.peers_json().await;
        String::from_utf8(bytes).unwrap_or_else(|_| "[]".to_string())
    } else {
        // If discovery is not started yet, return an empty JSON array.
        "[]".to_string()
    }
}

pub fn get_peers_sync() -> String {
    futures::executor::block_on(get_peers())
}

pub async fn start_service(player_name: String) {
    let discovery = Arc::new(LanDiscovery::new(8080, player_name).await.unwrap());

    // Try to set the global instance. If it's already set, we just reuse it.
    let discovery = match DISCOVERY.set(discovery.clone()) {
        Ok(()) => discovery,
        Err(_) => {
            let existing = DISCOVERY.get().unwrap().clone();
            // Reset shutdown flag for restart
            existing.shutdown.store(false, Ordering::Relaxed);
            existing
        }
    };

    discovery.clone().start().await;

    println!(
        "LAN Discovery started for {}...",
        discovery.announce_payload.read().await.name
    );

    while !discovery.shutdown.load(Ordering::Relaxed) {
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

    println!("LAN Discovery loop stopped.");
}

pub fn start_service_sync(player_name: String) {
    futures::executor::block_on(start_service(player_name));
}

pub async fn stop_service() {
    if let Some(discovery) = DISCOVERY.get() {
        discovery.shutdown.store(true, Ordering::Relaxed);
    }
}

pub fn stop_service_sync() {
    futures::executor::block_on(stop_service());
}
