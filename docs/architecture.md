# Mujina Miner Architecture

This document describes the high-level architecture of mujina-miner,
Bitcoin mining software built in Rust.

## Overview

Mujina-miner is organized as a single Rust crate with well-defined modules
that separate concerns while maintaining simplicity. The architecture is
fully async using Tokio for concurrent I/O operations.

### Key Dependencies

- **tokio**: Async runtime for concurrent I/O operations
- **tokio-serial**: Async serial port communication
- **rust-bitcoin**: Core Bitcoin types and utilities
- **axum**: HTTP server framework
- **tracing**: Structured logging and diagnostics

## Module Structure

```
src/
├── bin/              # Binary entry points
│   ├── minerd.rs     # mujina-minerd - Main daemon
│   ├── cli.rs        # mujina-cli - Command line interface
│   └── tui.rs        # mujina-tui - Terminal UI
├── lib.rs            # Library root
├── error.rs          # Common error types
├── types.rs          # Core types (Job, Share, Nonce, etc.)
├── config.rs         # Configuration loading and validation
├── daemon.rs         # Daemon lifecycle management
├── board/            # Hash board implementations
├── board_io/         # Physical connections to boards
├── board_protocol/   # Board control protocols
├── chip_io/          # Hardware interface traits (I2C, SPI, GPIO)
├── misc_chips/       # Peripheral chip drivers
├── asic/             # Mining ASIC drivers
├── board_manager.rs  # Board lifecycle and hotplug management
├── scheduler.rs      # Work scheduling and distribution
├── job_generator.rs  # Local job generation (testing/solo)
├── pool/             # Mining pool connectivity
├── api/              # HTTP API and WebSocket
├── api_client/       # Shared API client library
│   ├── mod.rs        # Client implementation
│   └── types.rs      # API DTOs and models
└── tracing.rs        # Logging and observability
```

## Module Descriptions

### Core Modules

#### `bin/minerd.rs`
The main daemon binary entry point. Handles:
- Signal handling (SIGINT/SIGTERM)
- Tokio runtime initialization
- Graceful shutdown coordination
- Top-level task spawning

#### `bin/cli.rs`
Command-line interface for controlling the miner:
- Uses the `api_client` module to communicate with the daemon
- Provides commands for status, configuration, pool management
- Suitable for scripting and automation

#### `bin/tui.rs`
Terminal user interface for interactive monitoring:
- Uses the `api_client` module to communicate with the daemon
- Built with ratatui for rich terminal graphics
- Real-time hashrate graphs and statistics
- Keyboard-driven interface for operators
- Connects via API WebSocket for live updates

#### `error.rs` (new)
Centralized error types using `thiserror`. Provides a unified `Error` enum
for the entire crate with conversions from underlying error types.

#### `types.rs` (new)
Core domain types shared across modules. This module re-exports commonly
used types from rust-bitcoin and defines mining-specific types. Using
rust-bitcoin provides battle-tested implementations of Bitcoin primitives
while avoiding reinventing fundamental types.

#### `config.rs` (new)
Configuration management:
- TOML file parsing with serde
- Config validation
- Hot-reload support via file watching
- Default values and config merging

#### `daemon.rs` (new)
Daemon lifecycle management:
- systemd notification support
- PID file handling
- Resource cleanup
- Health monitoring

### Hardware Communication Layer

The hardware communication layer is organized in distinct levels, each
with a specific responsibility. This design enables maximum code reuse and
testability.

```
┌──────────────────────────────────────────────────────────────┐
│                     Board Implementation                     │
│   orchestrates all components for a specific board model     │
└──────────────────────────────────────────────────────────────┘
               │                               │                
               │Peripherals                    │ASIC Chain      
               │                               │                
┌─────────────────────────────┐ ┌──────────────────────────────┐
│         Misc Chips          │ │            ASIC              │
│     peripheral drivers      │ │   ┌──────────────────────┐   │
│ ┌──────┐ ┌───────┐┌───────┐ │ │   │     BM13xx Family    │   │
│ │ TMP75│ │INA260 ││EMC2101│ │ │   │  ┌──────┐ ┌──────┐   │   │
│ └───┬──┘ └───┬───┘└───┬───┘ │ │   │  │BM1370│ │BM1362│   │   │
└─────────────────────────────┘ │   │  └───┬──┘ └───┬──┘   │   │
      │        │        │       │   └──────────────────────┘   │
      └────────┼────────┘       └──────────────────────────────┘
               │                           └────┬───┘           
        ┌─────────────┐                         │               
        │  Chip I/O   │                         │               
┌───────└─────────────┘───────┐                 │               
│    Board Protocol Adapters  │                 │               
│ ┌────────────┐ ┌──────────┐ │                 │               
│ │I2c via     │ │Gpio via  │ │                 │               
│ │bitaxe_raw  │ │bitaxe_raw│ │                 │               
│ └────┬───────┘ └─────┬────┘ │                 │               
└─────────────────────────────┘                 │               
       └───────┬───────┘                        │               
               │                                │               
    ┌─────────────────────┐           ┌──────────────────┐      
    │   Control Channel   │           │   Data Channel   │      
    │   board protocols   │           │  direct  serial  │      
    └─────────────────────┘           └──────────────────┘      
              │                                 │               
              └────────────────┬────────────────┘               
                               │                                
┌──────────────────────────────────────────────────────────────┐
│                         Board I/O                            │
│              Physical connections to boards                  │
│    ┌─────────────────────┐       ┌──────────────────────┐    │
│    │     USB Serial      │       │  PCIe, Ethernet, ... │    │
│    └─────────────────────┘       └──────────────────────┘    │
└──────────────────────────────────────────────────────────────┘
```

#### Key Insight: Two Separate Communication Paths

Mining boards typically have two distinct communication paths:

1. **Control Channel** (via control protocol): For board management
   - Temperature sensors, fan control, power monitoring
   - Reset lines, LED control
   - Uses protocols like bitaxe-raw over USB serial

2. **Data Channel** (direct to ASICs): For mining operations
   - Sending work to ASIC chips
   - Receiving nonces from ASIC chips
   - Uses chip-specific protocols (BM13xx, etc.)
   - Often a separate serial port connected directly to the ASIC chain

#### `board_io/`
Physical connections to hash boards. This layer handles:
- USB device discovery and enumeration
- Hotplug detection and events
- Opening and configuring serial ports
- Managing dual-channel devices (control + data channels)
- No protocol knowledge - just raw byte streams
- Emits `BoardConnected`/`BoardDisconnected` events

#### `board_protocol/`
Protocol implementations for hash board control channels. This layer:
- Implements specific packet formats (e.g., bitaxe-raw's 7-byte header)
- Provides protocol operations: GPIO control, ADC readings, I2C passthrough
- Handles command/response sequencing and error checking
- Translates high-level operations into protocol packets
- Provides adapters that implement `chip_io` traits over protocols

#### `chip_io/`
Hardware interface traits and native implementations. This layer:
- Defines traits like `I2c`, `Gpio`, `Adc` that peripheral drivers use
- Provides native Linux implementations for local buses
- Allows the same driver to work with Linux I2C or I2C-over-protocol

#### `misc_chips/`
Reusable drivers for peripheral chips (not mining ASICs). These drivers:
- Are generic over `chip_io` traits (work with any `I2c` implementation)
- Can be tested with mock implementations
- Used on both control boards and hash boards

#### `asic/` (Mining ASIC protocols)
Mining ASIC communication protocols - the heart of mining operations:
- Implements protocols for different ASIC families (BM13xx, etc.)
- Manages chip initialization, frequency control, and status
- Communicates directly via the data channel

### Example: EmberOne Board Implementation

Here's how these layers work together in practice:

```rust
// board/ember_one.rs
use crate::board_io::UsbSerial;
use crate::board_protocol::bitaxe_raw::{Protocol, I2c as BitaxeI2c};
use crate::misc_chips::{TMP75, INA260};
use crate::asic::bm13xx::{BM1370, ChipChain};

pub struct EmberOneBoard {
    transport: UsbSerial,
    protocol: Protocol,
    asic_chain: ChipChain<BM1370>,
    temp_sensor: TMP75<BitaxeI2c>,
    power_monitor: INA260<BitaxeI2c>,
}

impl EmberOneBoard {
    pub async fn new(control_port: &str, data_port: &str) -> Result<Self> {
        // 1. Create board I/O layer (dual serial ports)
        let transport = UsbSerial::open_dual(control_port, data_port)
            .await
            .context("Failed to open serial ports")?;
        
        // 2. Create board protocol handler for board management
        let mut protocol = Protocol::new(transport.control_channel());
        
        // 3. Initialize the board via control protocol
        protocol.set_gpio(ASIC_RESET_PIN, false).await?; // Reset ASICs
        tokio::time::sleep(Duration::from_millis(100)).await;
        protocol.set_gpio(ASIC_RESET_PIN, true).await?;  // Release reset
        
        // 4. Create ASIC chain on the data channel
        let asic_chain = ChipChain::<BM1370>::new(
            transport.data_channel(),
            1  // Single chip on EmberOne
        );
        
        // 5. Initialize ASICs
        asic_chain.enumerate_chips().await?;
        asic_chain.set_frequency(500.0).await?; // 500 MHz
        
        // 6. Create chip_io adapter for board peripherals
        let i2c = BitaxeI2c::new(&mut protocol);
        
        // 7. Create drivers for peripheral chips
        let temp_sensor = TMP75::new(i2c.clone(), 0x48);
        let power_monitor = INA260::new(i2c, 0x40);
        
        Ok(Self {
            transport,
            protocol,
            asic_chain,
            temp_sensor,
            power_monitor,
        })
    }
    
    pub async fn send_work(&mut self, job: MiningJob) -> Result<()> {
        // Send mining work directly to ASICs via data channel
        self.asic_chain.send_job(0, job).await
    }
    
    pub async fn check_for_nonces(&mut self) -> Result<Vec<Nonce>> {
        // Poll ASICs for any found nonces
        self.asic_chain.read_nonces().await
    }
    
    pub async fn read_diagnostics(&mut self) -> Result<Diagnostics> {
        // Read from board peripherals via control protocol
        let temp = self.temp_sensor.read_temperature().await?;
        let power = self.power_monitor.read_power().await?;
        let hashrate = self.asic_chain.estimate_hashrate();
        
        Ok(Diagnostics { temp, power, hashrate })
    }
}
```

This architecture has these aims and objectives:

1. **Reusability**: Drivers work with any `chip_io` implementation, ASIC protocols work with any serial port
2. **Testability**: Each layer can be tested in isolation with mocks
3. **Flexibility**: New boards can mix and match components:
   - Different ASIC chips (BM1370, BM1397, etc.)
   - Different board protocols (bitaxe-raw, custom protocols)
   - Different peripheral chips (various temp sensors, power monitors)
4. **Maintainability**: Clear boundaries between connections, protocols, and business logic
5. **Hotplug support**: Handles dynamic board connections

### Mining Logic

#### `board/`
Hash board implementations that compose all hardware elements:
- `Board` trait defining the interface for all hash boards
- `bitaxe.rs` - Original Bitaxe board implementation
- `ember_one.rs` - EmberOne board using layered architecture
- `registry.rs` - Board type registry for dynamic instantiation
- Manages: ASIC chains, cooling, power delivery, monitoring

#### `asic/`
Mining ASIC protocols and implementations:
- Current: `bm13xx/` family with protocol documentation
- Future: Other ASIC families (BM1397, etc.)
- Handles: work distribution, nonce collection, frequency control

#### `board_manager.rs`
Manages board lifecycle and hotplug events:
- Listens to `board_io` discovery events
- Identifies board types (USB VID/PID or probing)
- Creates/destroys board instances
- Maintains active board registry
- Routes boards to/from scheduler

#### `pool/`
Mining pool client implementations:
- `traits.rs` - `PoolClient` trait
- `stratum_v1.rs` - Stratum v1 protocol (most common)
- `stratum_v2.rs` - Stratum v2 protocol (future)
- `manager.rs` - Pool failover and switching logic
- Handles: work fetching, share submission, difficulty adjustments

#### `scheduler.rs`
Orchestrates the mining operation:
- Receives work from pools
- Distributes work to boards/chips
- Collects and routes shares
- Implements work scheduling strategies
- Manages board lifecycle

#### `job_generator.rs`
Local job generation for testing and solo mining:
- Generates valid block templates
- Updates timestamp/nonce fields
- Useful for hardware testing without pools

### API and Observability

#### `api/`
HTTP API server (new):
- Built on Axum (async web framework)
- RESTful endpoints for status, control, configuration
- WebSocket support for real-time updates
- OpenTelemetry integration
- Prometheus metrics endpoint

#### `tracing.rs`
Structured logging and observability:
- tracing subscriber setup
- journald or stdout output
- Log level configuration
- Performance tracing spans

## Data Flow

```
Mining Pool <--[Stratum]--> pool::PoolClient
                                   |
                                   v
                            scheduler::Scheduler
                                   |
                    +--------------+--------------+
                    |                             |
                    v                             v
             board::Board                  board::Board
                    |                             |
                    v                             v
          asic::BM13xxChip              asic::BM13xxChip
                    |                             |
                    v                             v
       board_io::UsbSerial         board_io::UsbSerial


Hotplug Flow:
USB Device ──> board_io ──> BoardConnected Event ──> board_manager
                                                           |
                                                           v
                                                    Creates Board
                                                           |
                                                           v
                                                  Registers with Scheduler
                                                           |
                                                           v
                                              Scheduler talks directly to Board
```

## Async Patterns

All I/O operations are async using Tokio:
- Serial communication uses `tokio-serial`
- HTTP server uses `axum` (built on Tokio)
- Background tasks use `tokio::spawn`
- Graceful shutdown via `CancellationToken`
- Concurrent operations via `TaskTracker`

## Extension Points

The architecture supports extension through several mechanisms:

1. **New Board Types**: Implement the `Board` trait
2. **New ASIC Families**: Add modules under `asic/`
3. **New Pool Protocols**: Implement `PoolClient` trait
4. **New Board Protocols**: Add under `board_protocol/`
5. **Custom Schedulers**: Pluggable scheduling strategies
6. **Additional Peripheral Chips**: Add drivers to `misc_chips/`
7. **New Connection Types**: Extend `board_io/` (PCIe, Ethernet)

## Configuration

Configuration is managed through TOML files with hot-reload support:
- `/etc/mujina/mujina.toml` - System configuration
- Board-specific settings
- Pool credentials and priorities
- Temperature limits and safety settings
- API server configuration

## Security Considerations

- No hardcoded credentials
- TLS support for API endpoints
- Privilege dropping after startup
- Isolated board control (no direct chip access from API)
- Rate limiting on API endpoints

## User Interfaces

Mujina-miner provides multiple interfaces for different use cases:

### Web Application (Separate Repository)
The primary user interface is a modern web application that lives in a
separate repository (`mujina-web`):
- Built with modern web technologies (React/Vue/Svelte)
- Communicates exclusively through the HTTP API
- Provides rich visualizations and easy configuration
- Suitable for remote management
- Can be served by any web server or CDN

**Repository**: `github.com/mujina/mujina-web` (example)

### Command Line Interface (CLI)
Included in this repository as `mujina-cli`:
- Direct API client for automation and scripting
- Supports all daemon operations
- JSON output mode for parsing
- Configuration file management

### Terminal User Interface (TUI)
Included in this repository as `mujina-tui`:
- Interactive terminal dashboard
- Real-time monitoring without web browser
- Ideal for SSH sessions
- Keyboard shortcuts for common operations

### API Client Library
The `api_client` module provides:
- Rust types for all API requests/responses
- Async HTTP client using reqwest
- WebSocket support for real-time data
- Shared between CLI and TUI
- Could be published as separate crate for third-party tools

## Repository Structure

This repository contains the core miner daemon and terminal-based tools:
```
mujina-miner/
├── Cargo.toml
├── README.md
├── docs/
│   ├── architecture.md    # This file
│   ├── api.md            # API documentation
│   └── deployment.md     # Installation guide
├── configs/
│   └── example.toml      # Example configuration
├── src/                  # Rust source code
├── systemd/
│   └── mujina-minerd.service
└── debian/               # Debian packaging
```

The web interface lives in a separate repository to allow:
- Independent development cycles
- Different programming languages
- Separate CI/CD pipelines
- Alternative web UIs from the community
