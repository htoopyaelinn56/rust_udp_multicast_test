use std::sync::Arc;
use std::time::Duration;
use rust_udp_multicast_test::LanDiscovery;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let player_name = args.get(1).cloned().unwrap_or_else(|| "Player".into());

    let discovery = Arc::new(LanDiscovery::new(8080, player_name.clone()).await.unwrap());
    discovery.clone().start().await;

    println!("LAN Discovery started for {}...", discovery.announce_payload.read().await.name);

    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let peers = discovery.get_peers().await;
        if !peers.is_empty() {
            println!("{} sees peers: {:#?}", discovery.announce_payload.read().await.name, peers);
        }
    }
}
