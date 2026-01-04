# Polymarket Trading Bot Infrastructure

Production-grade foundation for building high-performance Polymarket trading bots. Clean async architecture with concurrent asset execution, real-time market data streaming, and event-driven design.

## What This Is

Core infrastructure for algorithmic trading on Polymarket. Handles some of the hard parts (WebSocket management, concurrent execution, event normalization) so you can focus on strategy logic.

Each asset (BTC, ETH, SOL, XRP) runs in complete isolation with its own async task, state machine, and watcher instances. A failure in one asset doesn't cascade to others.

## Key Features

**Concurrent Execution Model**
- Independent async task per asset (BTC, ETH, SOL, XRP)
- No shared mutable state across await points
- Isolated failure domains - one asset crash doesn't kill others

**Real-Time Round Detection**
- Sub-100ms round transition detection
- Zero-delay rollover alerts via event emission
- Dedicated watcher per asset

**WebSocket Order Book Streaming**
- Persistent connections with automatic reconnection
- Heartbeat monitoring to detect stale connections
- REST reconciliation after disconnect/reconnect
- Clean state rebuild without data loss

**Event-Driven Architecture**
- Raw market data → Normalized internal events
- Type-safe event transformations
- Execution logic decoupled from data ingestion

## Architecture
```
┌─────────────────────────────────────────────────┐
│              Asset Executor (BTC)               │
│  ┌──────────────┐  ┌──────────────┐            │
│  │ Round        │  │ Order Book   │            │
│  │ Watcher      │  │ Watcher      │            │
│  └──────────────┘  └──────────────┘            │
│         │                  │                    │
│         └──────┬───────────┘                    │
│                │                                │
│         ┌──────▼──────┐                        │
│         │   Events    │                        │
│         └─────────────┘                        │
└─────────────────────────────────────────────────┘
│              (Same for ETH, SOL, XRP)           │
└─────────────────────────────────────────────────┘
                       │
                       ▼
              Message Passing (mpsc)
                       │
                       ▼
              Normalized Event Layer
```

Each asset operates in complete isolation:
- Async execution task
- State machine instance
- Dedicated watchers (round + order book)
- Independent order flow
- Isolated exposure accounting

## Technical Details

### Concurrent Asset Execution

Four independent Tokio tasks run concurrently. Each handles:
- Round transition monitoring
- Order book updates
- Order execution
- Position tracking

No blocking between assets. BTC processing never waits on ETH.

### Round Transition Detection

Watches for 15-minute round rollovers. When detected:
1. Emit `RoundStarted` event
2. Trigger strategy re-evaluation
3. Update internal state

Target latency: <100ms from actual rollover.

### WebSocket Connection Management

Order book watchers maintain persistent WebSocket connections:
```
Connect → Subscribe → Stream Updates → Heartbeat Check
                             │              │
                             │         (timeout?)
                             │              │
                             │              ▼
                             │         Reconnect
                             │              │
                             │              ▼
                             │      REST Reconciliation
                             │              │
                             └──────────────┘
```

Reconnection logic:
- Detect stale connection via heartbeat timeout
- Clean disconnect
- Re-establish WebSocket
- Fetch current state via REST
- Resume streaming

### Event Normalization

Raw market data transforms through type-safe layer before reaching execution logic:
```rust
RawWebSocketMessage → Parse → Validate → NormalizedEvent
```

Execution logic never sees raw external data. Everything goes through normalization.

## Project Structure
```
src/
├── assets/
│   ├── executor.rs       - Per-asset async execution task
│   └── mod.rs           - Asset definitions (BTC, ETH, SOL, XRP)
├── connectors/          - External API integrations
├── events/              - Event types and normalization
├── watchers/
│   ├── round.rs         - Round transition detection
│   └── orderbook.rs     - WebSocket streaming + reconnection
└── utils/               - Shared utilities
```

## Building
```bash
# Development build
cargo build

# Release build (optimized)
cargo build --release

# Run tests
cargo test

# Run with logging
RUST_LOG=debug cargo run
```

## Configuration

Set up `.env`:
```bash
POLYMARKET_API_KEY=your_api_key_here
WS_HEARTBEAT_INTERVAL=30
ROUND_CHECK_INTERVAL=1000
```

## Testing Scenarios

**Concurrent Execution**
- Start all four assets simultaneously
- Verify independent operation
- Confirm no blocking between assets

**WebSocket Disconnect Recovery**
- Simulate network failure during active trading
- Verify reconnection logic
- Confirm state rebuild via REST reconciliation
- Check no data loss

**Event Normalization**
- Log raw WebSocket messages
- Track transformation to normalized events
- Verify type safety throughout pipeline

## What's Not Included

This is infrastructure, not a complete bot. For custom solutions, contact me:
- Trading strategies
- Risk management
- Position sizing
- PnL tracking
- Backtesting framework

The foundation is here. Build your strategy on top.

## Performance Characteristics

- Round detection latency: <100ms
- WebSocket message processing: <1ms
- Reconnection time: 2-5 seconds
- Memory per asset: ~10-20MB
- CPU usage (idle): <1% per asset

## Known Issues

- WebSocket heartbeat interval hardcoded (make configurable)
- No metrics/monitoring yet (add Prometheus?)
- Order book depth limited to top N levels
- No historical data replay for backtesting

## Dependencies

- `tokio` - Async runtime
- `tokio-tungstenite` - WebSocket client
- `serde` - Serialization
- `reqwest` - HTTP client
- `tracing` - Structured logging

## Why Rust?

- Zero-cost abstractions for high-frequency data
- Memory safety without GC pauses
- Fearless concurrency via ownership system
- Compile-time guarantees on async code

Perfect for systems that need both performance and reliability.

## Contributing

This is the infrastructure layer I built for my own trading. Open sourced because good Polymarket tooling is scarce.

If you find bugs or have improvements:
1. Open an issue first
2. Fork and create feature branch
3. Add tests if relevant
4. Submit PR

## License

MIT - use it however you want

## Disclaimer

This is infrastructure code, not investment advice. Trading crypto is risky. Only trade what you can afford to lose. Past performance doesn't indicate future results. Do your own research.

## Author

Built by someone who got tired of poorly architected trading bots.
