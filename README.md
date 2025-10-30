# Snapchain Listener

A Rust-based listener for Farcaster's Snapchain gossip network that streams casts and reactions via WebSocket.

## Features

- Connects to Farcaster Snapchain mainnet bootstrap peers
- Listens to the gossip mempool for all casts and reactions
- Deduplicates messages using TTL-based caching
- Streams data to WebSocket clients in real-time
- Bearer token authentication for WebSocket connections

## Requirements

- Rust (latest stable)

## Configuration

Set the following environment variables:

- `BEARER_TOKEN` (optional): Authentication token for WebSocket clients. If not set, authentication is disabled.
- `WS_PORT` (optional): WebSocket server port (default: 8080)
- `RUST_LOG` (optional): Log level (`info` or `debug`)

## Usage

```bash
# Optional: Set environment variables
export BEARER_TOKEN="your-secure-token-here"  # Optional - enables authentication
export WS_PORT=8080                            # Optional - defaults to 8080
export RUST_LOG=debug                          # Use 'debug' to see cast/reaction messages, 'info' for connection events only

# Build and run
cargo build --release
cargo run --release
```

**Note:** Set `RUST_LOG=debug` to see the actual cast and reaction messages in the console. With `RUST_LOG=info`, you'll only see connection events.

## WebSocket Protocol

### Authentication

**With Bearer Token (if `BEARER_TOKEN` is set):**

Connect to `ws://localhost:8080` and send the bearer token as the first message:

```
your-secure-token-here
```

Upon successful authentication, you'll receive:

```json
{"status": "authenticated"}
```

**Without Bearer Token:**

If no `BEARER_TOKEN` is configured, authentication is disabled and clients can connect directly without sending any authentication message.

### Message Format

#### Cast Message
```json
{
  "type": "cast",
  "fid": 12345,
  "text": "Hello Farcaster!",
  "embeds": ["https://example.com/image.png"],
  "hash": "0xabcd..."
}
```

#### Reaction Message
```json
{
  "type": "reaction",
  "fid": 67890,
  "reaction_type": "like",
  "target_cast_fid": 12345,
  "target_cast_hash": "0xabcd...",
  "hash": "0xdef..."
}
```

## Known Issues

**Connection Stability:** The listener may experience connection losses to the gossip network over time. As a temporary workaround, users are advised to periodically restart the node (e.g., using a cronjob to restart every few hours). This issue can likely be fixed with proper connection management and automatic reconnection logic.

## License

GNU General Public License v3.0 - See LICENSE file for details
