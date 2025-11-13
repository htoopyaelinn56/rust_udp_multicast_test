# rust_udp_multicast_test

Simple LAN peer discovery in Rust using UDP multicast.

What this does
- Announces your presence on the local network every 2 seconds
- Listens for other peers doing the same
- Keeps a list of "alive" peers (seen within the last 10 seconds)
- Prints the peers it can see every 5 seconds

How it works (in plain English)
- Everyone sends a tiny JSON message with their name and a port number to a multicast group.
- Everyone also listens to that multicast group.
- When a message is received, the sender is added/updated in a shared list of peers.
- Old entries expire if they haven’t been heard from for a while.

Multicast details
- Group: 239.255.255.250
- Port: 9999
- IPv4 only, TTL 1 (stays within your local network)
- Message format (JSON): { "name": "PlayerName", "port": 8080 }

Requirements
- Rust (stable)
- A network that allows IPv4 multicast on the local link
- Tested on macOS; should work on Linux and Windows too (firewalls/VPNs can interfere)

Build
- cargo build

Run
- cargo run -- PlayerA
- In another terminal on the same machine or LAN: cargo run -- PlayerB

What you should see
- Each instance prints the peers it sees every 5 seconds. After both are running, each should list the other.

Configuration
- Player name: first command-line argument (defaults to "Player").
- Service port announced: 8080 by default. You can change it in src/lib.rs (start_service -> LanDiscovery::new).

Notes and tips
- If you don’t see peers:
  - Make sure all devices are on the same Wi‑Fi/Ethernet network (no guest networks).
  - Some routers and corporate networks block multicast/broadcast.
  - macOS: allow the app in System Settings > Network > Firewall.
  - Disable VPNs/traffic filters that may block multicast.
  - TTL is 1, so peers on another subnet won’t see you.
- The code picks the first non-loopback IPv4 interface it finds.

Project structure
- src/main.rs: parses the optional name and starts the service.
- src/lib.rs: multicast announce/listen logic with Tokio tasks.

Tech used
- Tokio for async networking
- socket2 for low-level socket options
- serde/serde_json for message encoding

License
- Use as you like. If you publish changes, consider sharing them back.

