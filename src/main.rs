use futures::{SinkExt, StreamExt};
use libp2p::{
    gossipsub, identity, noise, swarm::SwarmEvent, tcp, yamux, Multiaddr, PeerId,
};
use prost::Message as ProstMessage;
use serde::{Deserialize, Serialize};
use std::env;
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{error, info, warn};
use ttl_cache::TtlCache;

// Include generated protobuf code
// Note: Many structs are unused but we keep them all in case we want to stream
// other message types (e.g., links, verifications, user data) in the future
#[allow(dead_code)]
mod proto {
    include!(concat!(env!("OUT_DIR"), "/_.rs"));
}

const MEMPOOL_TOPIC: &str = "mempool";
const DEFAULT_WS_PORT: u16 = 8080;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum StreamMessage {
    #[serde(rename = "cast")]
    Cast {
        fid: u64,
        text: String,
        embeds: Vec<String>,
        hash: Option<String>,
    },
    #[serde(rename = "reaction")]
    Reaction {
        fid: u64,
        reaction_type: String, // "like" or "recast"
        target_cast_fid: Option<u64>,
        target_cast_hash: Option<String>,
        hash: Option<String>,
    },
}

// Mainnet bootstrap peers
const BOOTSTRAP_PEERS: &[&str] = &[
    "/ip4/54.236.164.51/udp/3382/quic-v1",
    "/ip4/54.87.204.167/udp/3382/quic-v1",
    "/ip4/44.197.255.20/udp/3382/quic-v1",
    "/ip4/34.195.157.114/udp/3382/quic-v1",
    "/ip4/107.20.169.236/udp/3382/quic-v1",
];

async fn handle_websocket(
    stream: TcpStream,
    mut rx: broadcast::Receiver<StreamMessage>,
    bearer_token: Arc<Option<String>>,
) {
    let ws_stream = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            error!("WebSocket handshake failed: {}", e);
            return;
        }
    };

    let (mut write, mut read) = ws_stream.split();

    // If bearer token is configured, require authentication
    if let Some(token) = bearer_token.as_ref() {
        // Wait for authentication message
        let auth_msg = match read.next().await {
            Some(Ok(WsMessage::Text(msg))) => msg,
            _ => {
                warn!("Failed to receive auth message");
                return;
            }
        };

        if auth_msg != *token {
            warn!("Invalid bearer token");
            let _ = write
                .send(WsMessage::Text("Unauthorized".to_string()))
                .await;
            return;
        }

        info!("✓ WebSocket client authenticated");

        // Send authenticated confirmation
        if let Err(e) = write
            .send(WsMessage::Text(
                serde_json::json!({"status": "authenticated"}).to_string(),
            ))
            .await
        {
            error!("Failed to send auth confirmation: {}", e);
            return;
        }
    } else {
        info!("✓ WebSocket client connected (no authentication)");
    }

    // Stream messages to client
    loop {
        match rx.recv().await {
            Ok(stream_msg) => {
                let json = match serde_json::to_string(&stream_msg) {
                    Ok(j) => j,
                    Err(e) => {
                        error!("Failed to serialize stream message: {}", e);
                        continue;
                    }
                };

                if let Err(e) = write.send(WsMessage::Text(json)).await {
                    error!("Failed to send message to client: {}", e);
                    break;
                }
            }
            Err(e) => {
                error!("Broadcast channel error: {}", e);
                break;
            }
        }
    }

    info!("WebSocket connection closed");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Setup logging
    // When RUST_LOG=debug, only show debug for snapchain_listener, keep others at info
    // Otherwise just use info for everything
    let filter = match std::env::var("RUST_LOG") {
        Ok(level) if level.to_lowercase() == "debug" => {
            tracing_subscriber::EnvFilter::new("snapchain_listener=debug,info")
        }
        Ok(custom) => tracing_subscriber::EnvFilter::new(&custom),
        Err(_) => tracing_subscriber::EnvFilter::new("info"),
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .init();

    info!("🚀 Starting Snapchain Gossip Listener...\n");

    // Get bearer token from environment variable (optional)
    let bearer_token = Arc::new(env::var("BEARER_TOKEN").ok());
    if bearer_token.is_some() {
        info!("🔑 Bearer token authentication enabled");
    } else {
        info!("⚠️  No bearer token configured - WebSocket authentication disabled");
    }

    // Get WebSocket port from environment or use default
    let ws_port = env::var("WS_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_WS_PORT);

    // Create broadcast channel for stream messages (casts and reactions)
    let (tx, _rx) = broadcast::channel::<StreamMessage>(100);
    let tx = Arc::new(tx);

    // Spawn WebSocket server
    let ws_bearer_token = bearer_token.clone();
    let ws_tx = tx.clone();
    tokio::spawn(async move {
        let addr = format!("0.0.0.0:{}", ws_port);
        let listener = TcpListener::bind(&addr).await.expect("Failed to bind");
        info!("🌐 WebSocket server listening on ws://{}", addr);

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    info!("New WebSocket connection from {}", addr);
                    let rx = ws_tx.subscribe();
                    let token = ws_bearer_token.clone();
                    tokio::spawn(handle_websocket(stream, rx, token));
                }
                Err(e) => error!("Failed to accept connection: {}", e),
            }
        }
    });

    info!("🚀 Starting Snapchain Gossip Listener...\n");

    // Generate a random peer identity
    let local_key = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(local_key.public());
    info!("📡 Local peer id: {}", local_peer_id);

    // Setup gossipsub
    let gossipsub_config = gossipsub::ConfigBuilder::default()
        .heartbeat_interval(Duration::from_millis(500))
        .validation_mode(gossipsub::ValidationMode::Strict)
        .max_transmit_size(10 * 1024 * 1024) // 10MB
        .build()
        .expect("Valid gossipsub config");

    let mut gossipsub: gossipsub::Behaviour = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(local_key.clone()),
        gossipsub_config,
    )
    .expect("Valid gossipsub behaviour");

    // Subscribe to mempool topic
    let topic = gossipsub::IdentTopic::new(MEMPOOL_TOPIC);
    gossipsub.subscribe(&topic)?;
    info!("📬 Subscribed to topic: \"{}\"", MEMPOOL_TOPIC);

    // Build the transport (both TCP and QUIC like snapchain does)
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(local_key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_behaviour(|_key| gossipsub)?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(300)))
        .build();

    // Listen on a random port
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    // Connect to bootstrap peers
    info!("🔗 Connecting to bootstrap peers...");
    for peer_addr in BOOTSTRAP_PEERS {
        match peer_addr.parse::<Multiaddr>() {
            Ok(addr) => {
                if let Err(e) = swarm.dial(addr.clone()) {
                    error!("   ✗ Failed to dial {}: {}", peer_addr, e);
                } else {
                    info!("   ⏳ Dialing {}...", peer_addr);
                }
            }
            Err(e) => error!("   ✗ Invalid address {}: {}", peer_addr, e),
        }
    }

    info!("\n👂 Listening for user messages...\n");
    info!("{}", "─".repeat(80));

    let mut message_count = 0u64;

    // TTL cache for deduplicating messages (10 second expiry)
    // Prevents processing the same message multiple times from different peers
    let mut seen_hashes: TtlCache<String, ()> = TtlCache::new(10000);

    // Event loop
    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!("🎧 Listening on {}", address);
            }
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                info!("✓ Connected to peer: {}", peer_id);
            }
            SwarmEvent::ConnectionClosed { peer_id, cause, .. } => {
                info!("✗ Connection closed with {}: {:?}", peer_id, cause);
            }
            SwarmEvent::Behaviour(gossipsub::Event::Message {
                propagation_source: _,
                message,
                ..
            }) => {
                if message.topic != topic.hash() {
                    continue;
                }

                // Try to decode and process message
                if let Ok(gossip_msg) = proto::GossipMessage::decode(message.data.as_slice()) {
                    if let Some(proto::gossip_message::GossipMessage::MempoolMessage(mempool_msg)) = gossip_msg.gossip_message {
                        if let Some(proto::mempool_message::MempoolMessage::UserMessage(user_msg)) = mempool_msg.mempool_message {
                            if let Some(data) = &user_msg.data {
                                // Get message hash (convert from bytes to hex string)
                                let hash_hex = user_msg.hash.iter()
                                    .map(|b| format!("{:02x}", b))
                                    .collect::<String>();

                                // Deduplicate: skip if we've seen this hash recently
                                if seen_hashes.contains_key(&hash_hex) {
                                    continue;
                                }
                                seen_hashes.insert(hash_hex.clone(), (), Duration::from_secs(10));

                                // Check if it's a cast (MESSAGE_TYPE_CAST_ADD = 1)
                                if data.r#type == 1 {
                                    if let Some(proto::message_data::Body::CastAddBody(cast)) = &data.body {
                                        message_count += 1;

                                        // Collect embed URLs (only actual URLs, not cast references)
                                        let mut embed_urls = Vec::new();
                                        for embed in &cast.embeds {
                                            if let Some(proto::embed::Embed::Url(url)) = &embed.embed {
                                                embed_urls.push(url.clone());
                                            }
                                        }

                                        // Create cast message
                                        let stream_msg = StreamMessage::Cast {
                                            fid: data.fid,
                                            text: cast.text.clone(),
                                            embeds: embed_urls.clone(),
                                            hash: Some(format!("0x{}", hash_hex)),
                                        };

                                        // Broadcast to WebSocket clients
                                        let _ = tx.send(stream_msg);

                                        // Log to console at debug level (use RUST_LOG=debug to see)
                                        tracing::debug!("\n💬 Cast #{} (FID: {})", message_count, data.fid);
                                        tracing::debug!("   {}", cast.text);
                                        if !embed_urls.is_empty() {
                                            tracing::debug!("   Embeds:");
                                            for url in &embed_urls {
                                                tracing::debug!("     - {}", url);
                                            }
                                        }
                                        tracing::debug!("{}", "─".repeat(80));
                                    }
                                }
                                // Check if it's a reaction (MESSAGE_TYPE_REACTION_ADD = 3)
                                else if data.r#type == 3 {
                                    if let Some(proto::message_data::Body::ReactionBody(reaction)) = &data.body {
                                        let reaction_type = match reaction.r#type {
                                            1 => "like",
                                            2 => "recast",
                                            _ => continue,
                                        };

                                        // Extract target cast info
                                        let (target_fid, target_hash) = if let Some(proto::reaction_body::Target::TargetCastId(cast_id)) = &reaction.target {
                                            let target_hash_hex = cast_id.hash.iter()
                                                .map(|b| format!("{:02x}", b))
                                                .collect::<String>();
                                            (Some(cast_id.fid), Some(format!("0x{}", target_hash_hex)))
                                        } else {
                                            (None, None)
                                        };

                                        // Create reaction message
                                        let stream_msg = StreamMessage::Reaction {
                                            fid: data.fid,
                                            reaction_type: reaction_type.to_string(),
                                            target_cast_fid: target_fid,
                                            target_cast_hash: target_hash.clone(),
                                            hash: Some(format!("0x{}", hash_hex)),
                                        };

                                        // Broadcast to WebSocket clients
                                        let _ = tx.send(stream_msg);

                                        // Log reactions at debug level (use RUST_LOG=debug to see)
                                        let emoji = if reaction_type == "like" { "❤️" } else { "🔄" };
                                        tracing::debug!("{} Reaction: {} by FID {} on cast {:?}",
                                            emoji, reaction_type, data.fid, target_hash);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            SwarmEvent::Behaviour(gossipsub::Event::Subscribed { peer_id, topic }) => {
                info!("Peer {} subscribed to topic: {}", peer_id, topic);
            }
            _ => {}
        }
    }
}
