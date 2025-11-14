use rust_udp_multicast_test::multicast_service::start_service;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let player_name = args.get(1).cloned().unwrap_or_else(|| "Player".into());

    start_service(player_name).await;
}
