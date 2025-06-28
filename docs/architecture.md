# Mujina Miner Architecture

This document describes the high-level architecture of mujina-miner, an
async Bitcoin mining software built on Tokio.

## Overview

Mujina-miner is organized as a single Rust crate with well-defined modules
that separate concerns while maintaining simplicity. The architecture is
fully async using Tokio for concurrent I/O operations.

### Key Dependencies

- **tokio**: Async runtime for concurrent I/O operations
- **rust-bitcoin**: Core Bitcoin types and utilities
- **tokio-serial**: Async serial port communication
- **axum**: HTTP server framework (future)
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
├── board/            # Mining board abstractions
├── chip/             # ASIC chip protocols
├── transport/        # USB/Serial communication layer
├── control/          # Hashboard control protocols
├── hal/              # Hardware abstraction layer
├── drivers/          # Peripheral device drivers
├── pool/             # Mining pool connectivity
├── scheduler.rs      # Work scheduling and distribution
├── job_generator.rs  # Local job generation (testing/solo)
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

#### `transport/`
Low-level transport abstractions for communicating with mining hardware:
- `usb.rs` - USB device discovery and enumeration
- `serial.rs` - Async serial port handling via tokio-serial
- Manages the dual-channel (control + data) USB serial devices

#### `control/`
Hashboard control protocols (distinct from ASIC protocols):
- `traits.rs` - `ControlProtocol` trait for different board types
- `bitaxe_raw.rs` - Implementation of the bitaxe-raw protocol
  - 7-byte packet format
  - GPIO control (reset lines)
  - ADC readings (voltage, temperature)
  - I2C passthrough

#### `hal/`
Hardware Abstraction Layer providing async traits:
- `i2c.rs` - Async I2C traits and adapters (I2C-over-control-protocol)
- `gpio.rs` - Async GPIO traits (GPIO-over-control-protocol)
- `adc.rs` - ADC channel traits
- Allows drivers to work with both direct hardware and protocol-proxied
  peripherals

#### `drivers/`
Reusable drivers for common mining peripherals:
- Temperature sensors (TMP75, EMC2101)
- Power monitors (INA260)
- Fan controllers
- Voltage regulators
- Works with any `hal::I2c` implementation

### Mining Logic

#### `board/`
**Existing module - expanded scope**

Mining board abstractions that compose all hardware elements:
- Unchanged: `Board` trait defining the interface
- Unchanged: `bitaxe.rs` - Bitaxe board family
- New: `generic_usb.rs` - Auto-detecting USB boards
- New: `registry.rs` - Board type registration
- Manages: chip chains, cooling, power delivery

#### `chip/`
**Existing module - unchanged location**

ASIC chip protocols and implementations:
- Current: `bm13xx/` family with protocol documentation
- Future: Other ASIC families
- Handles: work distribution, nonce collection, frequency control

#### `pool/`
Mining pool client implementations:
- `traits.rs` - `PoolClient` trait
- `stratum_v1.rs` - Stratum v1 protocol (most common)
- `stratum_v2.rs` - Stratum v2 protocol (future)
- `manager.rs` - Pool failover and switching logic
- Handles: work fetching, share submission, difficulty adjustments

#### `scheduler.rs`
**Existing module - enhanced**

Orchestrates the mining operation:
- Receives work from pools
- Distributes work to boards/chips
- Collects and routes shares
- Implements work scheduling strategies
- Manages board lifecycle

#### `job_generator.rs`
**Existing module - unchanged**

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
**Existing module - unchanged**

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
          chip::BM13xxChip              chip::BM13xxChip
                    |                             |
                    v                             v
    transport::SerialPort          transport::SerialPort
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
2. **New Chip Families**: Add modules under `chip/`
3. **New Pool Protocols**: Implement `PoolClient` trait
4. **New Control Protocols**: Implement `ControlProtocol` trait
5. **Custom Schedulers**: Pluggable scheduling strategies
6. **Additional Drivers**: Add I2C/SPI device drivers

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