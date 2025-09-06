
# Solana Shredstream Decoder Client

This is a performance-focused Rust client that decodes Solana shred packets directly from Turbine block propagation layer. Unlike Geyser RPC or standard RPC requests, this client ensures you get transactions the instant they are propagated by the leader validator, and this provides you with an speed advantage.

This decoder deserializes `buy`, `create` transactions from Pumpfun and token migrations from Pumpfun -> Raydium when the `initialize2` instruction is involved and the migration pubkey (`39azUYFWPz3VHgKCf3VChUwbpURdCHRxjWVowf5jUJjg`) is also involved.

# Features:  

- **Optimized-advanced UDP buffer**  – To prevent packet loss, the application binds a raw UDP socket and configures `SO_RCVBUF` to a larger size, allowing it to handle bursts of incoming shreds without dropping packets.
- **Deserializing** – Pulls transactions directly from raw shreds so that you receive them before they are visible on Geyser RPC. It can be used with Jito Shredstream Proxy or directly with your validator node.
- **Parallel processing** – Using `tokio` for I/O operations and `rayon` for CPU bound as multi-threading, DashMap for concurrency, and Mimalloc for memory allocation optimization.
- **Integrated gRPC Server** – Streams decoded transactions directly to your listener/bot or trading strategy via GRPC under 0.1 ms latency if it runs on same machine as the listener.
- **Reed-Solomon FEC Recovery** – Ensures complete block reconstruction so that there is no loss of any transaction if any `Data` shred is missing.
- **Slot transactions stats** – Automatically logs extracted transactions per slot and succesefull/failed FecBlocks statistics to `slot_txns_stats.json`. In 99% of cases all transactions from a slot are received, it doesn't skip txns.
- **Slot Delta Calculation** – For debugging, it uses gRPC to get current slot updates to calculate a Slot Delta with the slot of the completed `FecBlocks`.

# Who is it for?

- Bot users looking for the fastest transaction feed possible for Pumpfun or Raydium (Sniping, Arbitrage, etc).
- Validators who want an edge by decoding shreds locally.

# Setting up

## Environment Variables

Before run, you will need to add the following environment variables to your `.env` file:

- `GRPC_ENDPOINT` - Your Geyser RPC endpoint url.

- `GRPC_X_TOKEN` - Leave it set to `None` if your Geyser RPC does not require a token for authentication.

- `UDP_BUFFER_SOCKET` - Default is set to `0.0.0.0:8002`. It can also be `8001` depending on what port is returned by running `get_tvu_port.sh` from Jito Shredstream Proxy. You can also use your validator's shred receiving port if you use this decoder to deserialize the shreds received by your validator.

- `GRPC_SERVER_ENDPOINT` - The address of its gRPC server. By default is set at `0.0.0.0:50051`.

## Run Command

```
RUSTFLAGS="-C target-cpu=native" RUST_LOG=info cargo run --release --bin shredstream-decoder
```

# Source code

If you are really interested in the source code, please contact me for details and demo on Discord: `.xanr`.
