# BM13xx Hash Thread Domain Model Design

This document describes the domain model for the BM13xx hash thread
implementation. The model captures the physical and electrical organization of
mining ASICs on a hash board.

**Note**: Code snippets in this document are illustrative, not prescriptive.
They communicate design intent; the actual implementation may differ in
details.

**MSRV**: This design assumes Rust 1.91+, enabling modern features like return
position impl trait in traits (RPITIT).

## Architecture Overview

### HashThread: Interface vs Implementation

`HashThread` is the trait the scheduler uses---the contract for something that
can receive jobs and produce shares:

```rust
#[async_trait]
pub trait HashThread {
    fn name(&self) -> &str;
    fn capabilities(&self) -> &HashThreadCapabilities;
    async fn update_task(&mut self, task: HashTask) -> Result<Option<HashTask>, Error>;
    async fn replace_task(&mut self, task: HashTask) -> Result<Option<HashTask>, Error>;
    async fn go_idle(&mut self) -> Result<Option<HashTask>, Error>;
    fn take_event_receiver(&mut self) -> Option<mpsc::Receiver<HashThreadEvent>>;
    fn status(&self) -> HashThreadStatus;
}
```

The implementation behind this interface is a **collaboration of objects**:

```
┌─────────────────────────────────────────────────────────────────┐
│                    Scheduler's View                             │
│                         HashThread (trait)                      │
└─────────────────────────────┬───────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                BM13xx Implementation                            │
│                                                                 │
│  ┌──────────────┐                  ┌──────────────────────┐    │
│  │ BM13xxThread │  ◄── commands ──►│    Actor Task        │    │
│  │   (facade)   │                  │                      │    │
│  └──────────────┘                  │  ┌────────────────┐  │    │
│                                    │  │     Chain      │  │    │
│  ┌──────────────┐                  │  │  ┌──────────┐  │  │    │
│  │ ChainConfig  │ ── configures ──►│  │  │ Domains  │  │  │    │
│  │  (input)     │                  │  │  │  Chips   │  │  │    │
│  └──────────────┘                  │  │  └──────────┘  │  │    │
│                                    │  └────────────────┘  │    │
│                                    │                      │    │
│                                    │  ┌────────────────┐  │    │
│                                    │  │   Sequencer    │  │    │
│                                    │  │  ┌──────────┐  │  │    │
│                                    │  │  │ChipConfig│  │  │    │
│                                    │  │  └──────────┘  │  │    │
│                                    │  └────────────────┘  │    │
│                                    └──────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
```

The scheduler depends only on the `HashThread` trait. The internal structure
can evolve (single actor, multiple actors, etc.) without affecting the
interface.

## Domain Concepts

### Chip

Individual BM13xx ASIC chip. Contains both static topology information and
runtime state:

```rust
struct Chip {
    // Topology (set during chain construction, immutable)
    address: u8,
    domain: DomainId,

    // Runtime state (updated during operation)
    stats: ChipStats,
}

#[derive(Default)]
struct ChipStats {
    shares_found: u64,
    last_share_time: Option<Instant>,
    estimated_hashrate: HashRate,
    healthy: bool,
}
```

Different chip models (BM1362, BM1370, etc.) have different:
- Register initialization values
- Maximum frequencies and baud rates
- Domain configuration requirements

### Domain (Voltage Domain)

A group of chips sharing the same input/output voltage rails. Each domain has
its own voltage regulator. Domain boundaries require special configuration for
signal integrity (IO driver strength, UART relay).

```rust
struct Domain {
    id: DomainId,
    chips: Vec<ChipId>,  // Chips in this domain, ordered by chain position
}
```

A domain's chips may not be contiguous in the serial chain---the chain routing
is independent of domain grouping.

### Chain

The serial communication daisy chain. Contains all chips and domains:

```rust
struct Chain {
    chips: Vec<Chip>,      // Indexed by ChipId, in chain order
    domains: Vec<Domain>,  // Indexed by DomainId
}
```

The Chain is the "live model" of the hardware:
- Structure reflects physical wiring (from board topology)
- State reflects current operational status (updated during operation)

### Chain vs Domain: Two Independent Axes

The chain is the **communication** order (serial daisy chain routing). Domains
are the **electrical** grouping (shared voltage rails). These are orthogonal:

```
Example: Chain routing perpendicular to domain boundaries

Physical layout:          Chain routing (numbered by position):
┌─────────────────────┐
│ D0: [A] [B] [C] [D] │   A(0) ──► E(1) ──► I(2)
│ D1: [E] [F] [G] [H] │           │
│ D2: [I] [J] [K] [L] │   J(3) ◄── F(4) ◄── B(5)
└─────────────────────┘           │
                          C(6) ──► G(7) ──► K(8)
                                  │
                          L(9) ◄── H(10)◄── D(11)

Domain 0's chips: A(chain 0), B(chain 5), C(chain 6), D(chain 11)
  - Not contiguous in chain order!
  - First in chain: A (position 0)
  - Last in chain: D (position 11)
```

Domain boundary configuration targets the first/last chip of each domain **in
chain order**, not physical adjacency.

**Note**: The serpentine routing example above is hypothetical---no known
hardware uses non-contiguous domain routing. We keep the generality for
future-proofing, but since real hardware won't exercise these code paths,
unit tests must cover non-contiguous domain scenarios explicitly.

## Type-Safe Indices

We use newtype indices rather than pointers to reference objects in the
hierarchy:

```rust
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct ChipId(usize);

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct DomainId(usize);
```

**Why indices instead of pointers?** In C or kernel code, you'd use embedded
pointers (`struct domain *domain` in Chip, `struct chip **chips` in Domain).
In Rust, this creates ownership and lifetime complexity:

- Circular references between Chip and Domain fight the borrow checker
- Self-referential structs (Chain owning Chips that reference Domains that
  Chain also owns) require unsafe code or `Rc<RefCell<...>>`
- Lifetime parameters would infect the entire API

Indices sidestep all of this. `ChipId` and `DomainId` are `Copy`---no
lifetimes, no ownership issues. `Chain` is the single owner of all data.
Navigation requires going through `Chain`, which makes ownership explicit.

This pattern is idiomatic Rust, used by rustc (`DefId`, `NodeId`), petgraph
(`NodeIndex`, `EdgeIndex`), and ECS game engines (Bevy, specs).

The newtype wrappers add type safety: you can't accidentally pass a `ChipId`
where a `DomainId` is expected.

## Chain Access Patterns

```rust
impl Chain {
    pub fn chip(&self, id: ChipId) -> &Chip {
        &self.chips[id.0]
    }

    pub fn chip_mut(&mut self, id: ChipId) -> &mut Chip {
        &mut self.chips[id.0]
    }

    pub fn domain(&self, id: DomainId) -> &Domain {
        &self.domains[id.0]
    }

    /// All chips in chain order
    pub fn chips(&self) -> impl Iterator<Item = (ChipId, &Chip)> {
        self.chips.iter().enumerate().map(|(i, c)| (ChipId(i), c))
    }

    /// All domains
    pub fn domains(&self) -> impl Iterator<Item = &Domain> {
        self.domains.iter()
    }

    /// Domains from far end toward host (for domain configuration)
    pub fn domains_far_to_near(&self) -> impl Iterator<Item = &Domain> {
        self.domains.iter().rev()
    }

    /// First chip of domain in chain order
    pub fn domain_first(&self, id: DomainId) -> ChipId {
        *self.domain(id).chips.first().unwrap()
    }

    /// Last chip of domain in chain order
    pub fn domain_last(&self, id: DomainId) -> ChipId {
        *self.domain(id).chips.last().unwrap()
    }

    pub fn chip_count(&self) -> usize {
        self.chips.len()
    }

    pub fn domain_count(&self) -> usize {
        self.domains.len()
    }
}
```

## Configuration: What the Board Provides

### ChainConfig

The board provides complete configuration for a chain. A board may have
multiple chains, each with its own config:

```rust
/// Configuration for a BM13xx ASIC chain.
/// Provided by the board, used by the HashThread implementation.
pub struct ChainConfig {
    /// Human-readable name for logging
    pub name: String,

    /// Physical topology (chain routing, domains)
    pub topology: TopologySpec,

    /// Chip configuration with board-specific overrides
    pub chip_config: ChipConfig,

    /// Hardware control interfaces for this chain
    pub peripherals: ChainPeripherals,
}

/// Hardware interfaces for a chain.
///
/// These are trait objects with Arc because:
/// - The board may retain control over enable (shared with hash thread)
/// - Voltage regulators may be shared among multiple chains/threads
/// - Shared ownership naturally uses Arc<dyn Trait> with type erasure
///
/// Uses `tokio::sync::Mutex` rather than `std::sync::Mutex` to avoid
/// blocking worker threads. While peripheral I/O is fast, the async mutex
/// ensures we never accidentally block other tasks on the same worker.
pub struct ChainPeripherals {
    /// Enable/disable the ASIC chain.
    ///
    /// This abstraction covers different mechanisms:
    /// - Reset GPIO (assert = inactive/low-power, deassert = active)
    /// - Power enable (cut power = inactive, power on = active)
    /// - Board-specific implementations
    ///
    /// When disabled, chips are in low-power state. When enabled, chips
    /// need full re-initialization (all configuration is lost).
    pub asic_enable: Arc<tokio::sync::Mutex<dyn AsicEnable + Send>>,

    /// Voltage regulator control (optional, may be shared across chains)
    pub voltage_regulator: Option<Arc<tokio::sync::Mutex<dyn VoltageRegulator + Send>>>,
}
```

The actor creates `Sequencer` with the chip config. Build methods receive
`&Chain` to generate sequences. See the Sequencer section for details.

### TopologySpec

Describes the physical wiring---which domain each chip belongs to:

```rust
/// Describes how chips are wired on a board.
pub struct TopologySpec {
    /// For each chain position, which domain does that chip belong to?
    /// Index = chain position, Value = domain ID
    chip_domains: Vec<DomainId>,

    /// Whether this board needs domain boundary configuration
    /// (IO driver strength, UART relay)
    needs_domain_config: bool,
}

impl TopologySpec {
    pub fn expected_chip_count(&self) -> usize {
        self.chip_domains.len()
    }

    pub fn domain_count(&self) -> usize {
        self.chip_domains.iter()
            .map(|d| d.0)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0)
    }
}
```

### Convenience Constructors

```rust
impl TopologySpec {
    /// Uniform domains with equal chip counts (S21 Pro style)
    /// Chain visits all chips in domain 0, then domain 1, etc.
    pub fn uniform_domains(domain_count: usize, chips_per_domain: usize) -> Self {
        let chip_domains = (0..domain_count * chips_per_domain)
            .map(|i| DomainId(i / chips_per_domain))
            .collect();
        Self { chip_domains, needs_domain_config: true }
    }

    /// Individual domains---one per chip (EmberOne style)
    pub fn individual_domains(chip_count: usize, needs_domain_config: bool) -> Self {
        let chip_domains = (0..chip_count).map(DomainId).collect();
        Self { chip_domains, needs_domain_config }
    }

    /// Single domain for all chips (Bitaxe, simple boards)
    pub fn single_domain(chip_count: usize) -> Self {
        Self {
            chip_domains: vec![DomainId(0); chip_count],
            needs_domain_config: false,
        }
    }

    /// Explicit mapping for complex/non-standard routing.
    ///
    /// Domain IDs must be contiguous starting from 0 (e.g., [0, 0, 1, 1, 2, 2]).
    /// Returns an error if domains are sparse (e.g., [0, 2, 3] skipping 1).
    pub fn custom(chip_domains: Vec<usize>, needs_domain_config: bool) -> Result<Self, Error> {
        // Validate contiguous domain IDs
        let seen: HashSet<usize> = chip_domains.iter().copied().collect();
        let expected: HashSet<usize> = (0..seen.len()).collect();
        if seen != expected {
            return Err(Error::NonContiguousDomains { found: seen });
        }

        Ok(Self {
            chip_domains: chip_domains.into_iter().map(DomainId).collect(),
            needs_domain_config,
        })
    }
}
```

## Data Flow

```
Board                              HashThread Implementation
  │                                         │
  ├─ creates ChainConfig ──────────────────►│
  │   - name                                │
  │   - topology (TopologySpec)             │
  │   - chip_config                         │
  │   - peripherals                         │
  │                                         │
  │                                         ├─ stores config
  │                                         │
  │                                         ├─ build_chain()
  │                                         │   - create Chain from topology
  │                                         │   - assign addresses (interval 2)
  │                                         │   - initialize stats to default
  │                                         │
  │                                         ├─ create sequencer
  │                                         │   - Sequencer::new(chip_config)
  │                                         │
  │                                         ├─ execute enumeration sequence
  │                                         │   - verify chip count matches
  │                                         │   - verify chip model matches
  │                                         │
  │                                         ├─ execute domain config sequence
  │                                         │   - IO driver on domain-end chips
  │                                         │   - UART relay on domain boundaries
  │                                         │
  │                                         ├─ execute reg config sequence
  │                                         │   - per-chip register setup
  │                                         │
  │                                         ├─ ramp_frequency()
  │                                         │
  │                                         ▼ ready for mining
```

Key point: Chain structure and addresses are known from topology before any
hardware interaction. Enumeration *verifies* the expected chip count and type,
it doesn't *discover* them.

## Building the Chain

Chain is built from topology before hardware interaction. Addresses are
assigned separately:

```rust
impl Chain {
    /// Build chain structure from topology. Addresses not yet assigned.
    pub fn from_topology(spec: &TopologySpec) -> Self {
        let chips: Vec<Chip> = (0..spec.chip_count())
            .map(|i| Chip {
                address: 0,  // Assigned below
                domain: spec.chip_domain(i),
                stats: ChipStats::default(),
            })
            .collect();

        // Build domains, collecting their chips in chain order
        let domain_count = spec.domain_count();
        let mut domains: Vec<Domain> = (0..domain_count)
            .map(|i| Domain {
                id: DomainId(i),
                chips: Vec::new(),
            })
            .collect();

        for (i, chip) in chips.iter().enumerate() {
            domains[chip.domain.0].chips.push(ChipId(i));
        }

        Self { chips, domains }
    }

    /// Assign chip addresses with interval 2.
    ///
    /// Interval 2 is the convention observed in firmware captures:
    /// - S21 Pro (65 chips): addresses 0x00..0x80
    /// - S19 J Pro (126 chips): addresses 0x00..0xFA
    ///
    /// Returns error if chip count exceeds address space (max 128 chips).
    pub fn assign_addresses(&mut self) -> Result<(), Error> {
        self.assign_addresses_with_interval(2)
    }

    /// Assign addresses with explicit interval for unusual boards.
    ///
    /// Returns error if last chip address would exceed 0xFF.
    pub fn assign_addresses_with_interval(&mut self, interval: u8) -> Result<(), Error> {
        let last_address = (self.chips.len() - 1) * interval as usize;
        if last_address > 0xFF {
            return Err(Error::AddressSpaceOverflow {
                chip_count: self.chips.len(),
                interval,
            });
        }

        for (i, chip) in self.chips.iter_mut().enumerate() {
            chip.address = (i * interval as usize) as u8;
        }
        Ok(())
    }
}
```

Typical usage:

```rust
let mut chain = Chain::from_topology(&topology);
chain.assign_addresses()?;  // Standard interval 2
```

## Board Usage Example

```rust
impl EmberOneBoard {
    pub fn create_chain_config(&self) -> ChainConfig {
        ChainConfig {
            name: format!("EmberOne ({})", self.serial_number),

            // 12 BM1362 chips, each in its own domain
            topology: TopologySpec::individual_domains(12, false),

            chip_config: bm13xx::bm1362(),

            peripherals: ChainPeripherals {
                asic_enable: Arc::new(tokio::sync::Mutex::new(self.gpio_enable.clone())),
                voltage_regulator: Some(Arc::new(tokio::sync::Mutex::new(self.tps546.clone()))),
            },
        }
    }
}

// Hypothetical S21 Pro
impl S21ProBoard {
    pub fn create_chain_config(&self) -> ChainConfig {
        ChainConfig {
            name: format!("S21 Pro ({})", self.serial_number),

            // 65 BM1370 chips, 13 domains of 5
            topology: TopologySpec::uniform_domains(13, 5),

            // Uses all BM1370 defaults
            chip_config: bm13xx::bm1370(),

            peripherals: ChainPeripherals { ... },
        }
    }
}
```

## Validation Against Captures

### S21 Pro (65 chips, 13 domains of 5)

Domain-end IO driver addresses (register 0x58 with 0x0001F111):
- Expected: 0x80, 0x76, 0x6C, 0x62, 0x58, 0x4E, 0x44, 0x3A, 0x30, 0x26, 0x1C,
  0x12, 0x08

UART relay GAP_CNT values (register 0x2C, bits 31-16):
- Domain 12 (far): GAP_CNT = 5 * (13 - 12) + 14 = 19 = 0x13
- Domain 6 (mid):  GAP_CNT = 5 * (13 - 6) + 14 = 49 = 0x31
- Domain 0 (near): GAP_CNT = 5 * (13 - 0) + 14 = 79 = 0x4F

### S19 J Pro (126 chips, effectively 1 domain)

- No per-domain IO driver writes
- No UART relay writes
- All configuration is broadcast

---

## Initialization and Lifecycle

### State Machine

The hash thread cycles between disabled (low-power) and active states:

```
┌─────────────────┐
│    DISABLED     │◄──────────────────────────────┐
│  (low power)    │                               │
└────────┬────────┘                               │
         │ task arrives                           │
         ▼                                        │
┌─────────────────┐                               │
│   INITIALIZING  │                               │
│  - enable chain │                               │
│  - enumerate    │                               │
│  - configure    │                               │
└────────┬────────┘                               │
         │                                        │
         ▼                                        │
┌─────────────────┐                               │
│     READY       │                               │
│  (min frequency)│                               │
└────────┬────────┘                               │
         │ set_frequency()                        │
         ▼                                        │
┌─────────────────┐                               │
│     MINING      │                               │
│  (at requested  │                               │
│   frequency)    │                               │
└────────┬────────┘                               │
         │ idle timeout / go_idle()               │
         ▼                                        │
┌─────────────────┐                               │
│    DISABLING    │───────────────────────────────┘
│  - disable chain│
└─────────────────┘
```

When disabled (via `AsicEnable::disable()`), chips enter a low-power state.
All configuration is lost---re-enabling requires full re-initialization.

### Initialization Phases

When transitioning from DISABLED to READY:

```
1. Build Chain (before hardware interaction)
   └─ Chain::from_topology() creates structure
   └─ chain.assign_addresses() assigns addresses (interval 2)
   └─ Sequencer::new(chip_config) creates sequencer

2. Enable Chain
   └─ AsicEnable::enable()
   └─ Wait for chips to stabilize

3. Enumeration (verification, not discovery)
   └─ Execute sequencer.build_enumeration(&chain)
   └─ Send ChainInactive to reset all chips
   └─ Send SetChipAddress for each address, counting responses
   └─ Verify response count matches expected
   └─ Verify chip model matches expected (via ChipConfig::verify_chip_id)

4. Domain Configuration (if TopologySpec.needs_domain_config)
   └─ Execute sequencer.build_domain_config(&chain)
   └─ Configure IO driver strength at domain boundaries (reg 0x58)
   └─ Configure UART relay GAP_CNT for signal regeneration (reg 0x2C)
   └─ Work from far domain toward host

5. Register Configuration
   └─ Execute sequencer.build_reg_config(&chain)
   └─ PLL at minimum frequency
   └─ Other chip-specific registers
```

After these phases, the chain is in READY state at minimum frequency.

**TODO**: Validate these phases against serial captures (S21 Pro, S19 J Pro,
Bitaxe). Verify ordering, timing, and any steps we may have missed.

### Frequency Configuration (Separate Step)

Frequency adjustment is **not** part of initialization---it's a separate,
parameterized operation:

```rust
impl Actor {
    /// Initialize the chain (enumerate, configure). Leaves at min frequency.
    async fn initialize(&mut self) -> Result<(), Error>;

    /// Adjust operating frequency. Can be called multiple times.
    async fn set_frequency(&mut self, target: Frequency) -> Result<(), Error>;

    /// Enter low-power state. All configuration lost.
    async fn disable(&mut self) -> Result<(), Error>;
}
```

This separation allows:
- **POST**: Initialize, verify chain integrity, never ramp frequency
- **Mining**: Initialize, then ramp to full speed
- **Power management**: Scheduler can request frequency changes during mining
  to adjust power consumption

The scheduler communicates power/performance targets; the hash thread
translates these to frequency settings.

---

## Sequencer and ChipConfig

This section describes how initialization and configuration commands are
generated, and how boards customize chip-specific behavior.

### Core Concept: Sequences as Data

Sequences are lists of protocol commands that the hash thread iterates over.
The sequence itself is pure data---it doesn't handle errors, retries, or
execution logic. The executor (hash thread implementation) is responsible for:
- Sending commands via the protocol layer
- Handling timeouts and errors
- Managing delays between commands
- Verifying chip IDs after reads

This separation keeps sequences simple and testable.

### Step: Wrapping protocol::Command

Sequences use the existing `protocol::Command` type directly---no parallel
enum to maintain. `Step` adds timing information:

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub command: protocol::Command,
    pub wait_after: Option<Duration>,
}
```

The `protocol::Command` enum already defines the wire protocol:

```rust
// From protocol.rs (existing code, not duplicated)
pub enum Command {
    SetChipAddress { chip_address: u8 },
    ChainInactive,
    ReadRegister { broadcast: bool, chip_address: u8, register_address: RegisterAddress },
    WriteRegister { broadcast: bool, chip_address: u8, register: Register },
    JobFull { job_data: JobFullFormat },
    JobMidstate { job_data: JobMidstateFormat },
}
```

**Design decisions**:
- **Reuse protocol types**: Single source of truth. No parallel enum.
- **Addresses resolved at build time**: Sequencer receives `&Chain`, so
  addresses are filled in when building sequences, not deferred.
- **Pure data**: No closures, no handlers. Enables `Debug`, `Clone`, `PartialEq`.
- **Post-command delays**: Timing attached via `wait_after` field.
- **Response handling is executor's responsibility**: Step doesn't encode what
  responses to expect. The executor has phase-specific logic---enumeration
  counts `SetChipAddress` responses, configuration ignores them, etc.

### Sequencer

The actor owns both `Chain` (live model with stats) and `Sequencer`
(sequence generation). The sequencer holds chip config; build methods receive
`&Chain` to avoid self-referential struct issues:

```rust
pub struct Sequencer {
    chip_config: ChipConfig,
    // Optional overrides (see "Two Levels of Customization")
    enumeration_fn: Option<Box<dyn Fn(&Chain, &ChipConfig) -> Vec<Step> + Send + Sync>>,
    domain_config_fn: Option<Box<dyn Fn(&Chain, &ChipConfig) -> Vec<Step> + Send + Sync>>,
    reg_config_fn: Option<Box<dyn Fn(&Chain, &ChipConfig) -> Vec<Step> + Send + Sync>>,
}

impl Sequencer {
    pub fn new(chip_config: ChipConfig) -> Self {
        Self {
            chip_config,
            enumeration_fn: None,
            domain_config_fn: None,
            reg_config_fn: None,
        }
    }

    // --- Initialization phases ---

    /// Build enumeration sequence (ChainInactive, SetChipAddress, etc.)
    pub fn build_enumeration(&self, chain: &Chain) -> Vec<Step> {
        if let Some(f) = &self.enumeration_fn {
            f(chain, &self.chip_config)
        } else {
            self.default_enumeration(chain)
        }
    }

    /// Build domain configuration sequence (IO driver, UART relay)
    pub fn build_domain_config(&self, chain: &Chain) -> Vec<Step> {
        if let Some(f) = &self.domain_config_fn {
            f(chain, &self.chip_config)
        } else {
            self.default_domain_config(chain)
        }
    }

    /// Build per-chip register configuration sequence
    pub fn build_reg_config(&self, chain: &Chain) -> Vec<Step> {
        if let Some(f) = &self.reg_config_fn {
            f(chain, &self.chip_config)
        } else {
            self.default_reg_config(chain)
        }
    }

    /// Build frequency change sequence
    pub fn build_frequency_change(&self, chain: &Chain, target: Frequency) -> Vec<Step>;

    // --- Runtime operations ---

    /// Build job dispatch sequence for a given job
    pub fn build_job_dispatch(&self, chain: &Chain, job: &Job) -> Vec<Step>;
}
```

**Scope**: Sequences cover both configuration (init, frequency) and runtime
operations (job dispatch). Nonce responses are handled by the executor, not
sequences (they're asynchronous incoming data).

**Lifecycle**: The sequencer is created by the actor after Chain is built,
stored, and reused throughout operation.

### Default Sequence Example

```rust
impl Sequencer {
    fn default_enumeration(&self, chain: &Chain) -> Vec<Step> {
        let mut steps = vec![
            Step {
                command: protocol::Command::ChainInactive,
                wait_after: Some(Duration::from_millis(10)),
            },
        ];

        // SetChipAddress for each chip using addresses from chain
        for (_, chip) in chain.chips() {
            steps.push(Step {
                command: protocol::Command::SetChipAddress {
                    chip_address: chip.address,
                },
                wait_after: Some(Duration::from_micros(100)),
            });
        }

        steps
    }

    fn default_domain_config(&self, chain: &Chain) -> Vec<Step> {
        let mut steps = vec![];

        // Configure IO driver on last chip of each domain
        for domain in chain.domains() {
            let last_chip_id = chain.domain_last(domain.id);
            let last_chip = chain.chip(last_chip_id);

            steps.push(Step {
                command: protocol::Command::WriteRegister {
                    broadcast: false,
                    chip_address: last_chip.address,
                    register: Register::IoDriverStrength(self.chip_config.io_driver.clone()),
                },
                wait_after: None,
            });
        }

        steps
    }
}
```

### ChipConfig

All BM13xx chips share the same configurable fields---only the default values
differ. Rather than using generics and traits, we use a single struct with
free functions that return defaults per chip model:

```rust
/// Configuration for a BM13xx ASIC chip.
///
/// All chip models share the same fields. Use `bm1362()` or `bm1370()`
/// to get appropriate defaults, then modify fields as needed.
pub struct ChipConfig {
    // Chip identity
    pub chip_id: u32,
    pub min_freq: Frequency,
    pub max_freq: Frequency,

    // Configurable register values
    pub io_driver: IoDriverStrength,

    // PLL calculation parameters
    pll_params: PllParams,
}

/// BM1362 defaults (EmberOne, S19 J Pro)
pub fn bm1362() -> ChipConfig {
    ChipConfig {
        chip_id: 0x1362,
        min_freq: Frequency::mhz(50),
        max_freq: Frequency::mhz(525),
        io_driver: IoDriverStrength::normal(),
        pll_params: PllParams::bm1362(),
    }
}

/// BM1370 defaults (Bitaxe Gamma, S21 Pro)
pub fn bm1370() -> ChipConfig {
    ChipConfig {
        chip_id: 0x1370,
        min_freq: Frequency::mhz(50),
        max_freq: Frequency::mhz(600),
        io_driver: IoDriverStrength::normal(),
        pll_params: PllParams::bm1370(),
    }
}

impl ChipConfig {
    /// Calculate PLL configuration for target frequency.
    pub fn calculate_pll(&self, freq: Frequency) -> PllConfig {
        // Formula using self.pll_params
    }

    /// Verify chip ID matches expected value.
    pub fn verify_chip_id(&self, value: u32) -> Result<(), Error> {
        if value == self.chip_id {
            Ok(())
        } else {
            Err(Error::ChipMismatch {
                expected: self.chip_id,
                found: value,
            })
        }
    }
}
```

**Design decisions**:
- **No traits, no generics**: A single struct is simpler than trait + Deref +
  newtype wrappers. The executor and sequencer don't need to be generic.
- **Free functions for defaults**: `bm13xx::bm1362()` is cleaner than
  `bm13xx::ChipConfig::bm1362()`. Boards modify fields as needed.
- **PLL as data, not behavior**: PLL calculation uses stored parameters rather
  than trait method dispatch. The formula is the same; only the constants
  differ.
- **Chip identity stored as data**: `chip_id`, `min_freq`, `max_freq` are
  fields, not associated constants. Verified at runtime during enumeration.

Note: `address_interval` is not on `ChipConfig`---addressing is a topology
concern, handled by `Chain::assign_addresses()`.

### Two Levels of Customization

**Level 1 (easy): Modify config fields**

The board gets defaults and modifies fields:

```rust
let mut chip_config = bm13xx::bm1362();
chip_config.io_driver = IoDriverStrength::domain_end();  // Stronger drive

// Later, when actor creates Sequencer:
let sequencer = Sequencer::new(chip_config);
```

The sequence logic stays the same, but uses the board's values.

**Level 2 (more effort): Override entire sequences**

For unusual boards that need completely different command sequences, the
sequencer accepts closures that replace the default sequence logic:

```rust
let sequencer = Sequencer::new(chip_config)
    .with_enumeration(|chain, cfg| {
        // Completely custom enumeration sequence
        vec![...]
    })
    .with_domain_config(|chain, cfg| {
        // Custom domain configuration
        vec![...]
    });
```

Builder methods for overrides:

```rust
impl Sequencer {
    pub fn with_enumeration(
        mut self,
        f: impl Fn(&Chain, &ChipConfig) -> Vec<Step> + Send + Sync + 'static,
    ) -> Self {
        self.enumeration_fn = Some(Box::new(f));
        self
    }

    pub fn with_domain_config(
        mut self,
        f: impl Fn(&Chain, &ChipConfig) -> Vec<Step> + Send + Sync + 'static,
    ) -> Self {
        self.domain_config_fn = Some(Box::new(f));
        self
    }

    // Similar for other phases...
}
```

**Design decisions**:
- **Board modifies struct fields**: Simpler and more discoverable than
  chained methods.
- **Trusted blindly**: The sequencer doesn't validate values. The board is
  responsible for providing valid values.
- **Closures per phase**: Each build method can be replaced independently.
  A board can override enumeration but use defaults for everything else.
- **`Send + Sync` closures**: Required for async runtime.

### Testing Strategy

Sequences are pure data, making them easy to test without hardware.

**Fixture location**: Keep fixtures local to the bm13xx module, not in a
global test directory:

```rust
// In chain.rs or sequence_builder.rs

#[cfg(test)]
mod tests {
    mod fixtures {
        pub mod s21_pro {
            pub const CHIP_COUNT: usize = 65;
            pub const DOMAIN_COUNT: usize = 13;
            pub const CHIPS_PER_DOMAIN: usize = 5;
            pub const FIRST_CHIP_ADDRESS: u8 = 0x00;
            pub const LAST_CHIP_ADDRESS: u8 = 0x80;
            pub const DOMAIN_END_ADDRESSES: [u8; 13] = [
                0x08, 0x12, 0x1C, 0x26, 0x30, 0x3A, 0x44,
                0x4E, 0x58, 0x62, 0x6C, 0x76, 0x80,
            ];
        }

        pub mod s19_jpro {
            pub const CHIP_COUNT: usize = 126;
            pub const FIRST_CHIP_ADDRESS: u8 = 0x00;
            pub const LAST_CHIP_ADDRESS: u8 = 0xFA;
            // No domain boundary config (effectively single domain)
        }

        pub mod bitaxe {
            pub const CHIP_COUNT: usize = 1;
            pub const CHIP_ADDRESS: u8 = 0x00;
        }
    }
}
```

**Layered testing** (avoiding tautology):

Tests should verify *logic*, not just that we copied values correctly. The
captures tell us what real hardware expects; tests verify our domain model
produces it.

*Layer 1: Test invariants (the formula)*

```rust
#[test]
fn address_interval_is_two() {
    let topology = TopologySpec::single_domain(10);
    let mut chain = Chain::from_topology(&topology);
    chain.assign_addresses().unwrap();

    for i in 1..chain.chip_count() {
        let prev = chain.chip(ChipId(i - 1)).address;
        let curr = chain.chip(ChipId(i)).address;
        assert_eq!(curr - prev, 2);
    }
}
```

*Layer 2: Test topology interpretation*

Fixtures encode topology parameters (from hardware spec) and expected results
(from captures). Tests verify our logic correctly interprets the parameters:

```rust
#[test]
fn s21_pro_domain_ends() {
    use fixtures::s21_pro::*;

    let topology = TopologySpec::uniform_domains(DOMAIN_COUNT, CHIPS_PER_DOMAIN);
    let mut chain = Chain::from_topology(&topology);
    chain.assign_addresses().unwrap();

    let actual: Vec<u8> = (0..DOMAIN_COUNT)
        .map(|d| chain.chip(chain.domain_last(DomainId(d))).address)
        .collect();

    assert_eq!(actual, DOMAIN_END_ADDRESSES);
}
```

This tests that `domain_last()` correctly identifies domain boundaries---not
that we typed addresses twice.

*Layer 3: Test sequence structure*

Verify sequences have correct shape and derive addresses from the chain model:

```rust
#[test]
fn enumeration_sequence_structure() {
    use fixtures::s21_pro::*;

    let chip_config = bm13xx::bm1370();
    let topology = TopologySpec::uniform_domains(DOMAIN_COUNT, CHIPS_PER_DOMAIN);
    let mut chain = Chain::from_topology(&topology);
    chain.assign_addresses().unwrap();

    let sequencer = Sequencer::new(chip_config);
    let steps = sequencer.build_enumeration(&chain);

    // Structure: 1 ChainInactive + N SetChipAddress
    assert!(matches!(steps[0].command, Command::ChainInactive));
    assert_eq!(steps.len(), 1 + chain.chip_count());

    // Addresses in sequence match chain model
    for (i, step) in steps[1..].iter().enumerate() {
        if let Command::SetChipAddress { chip_address } = step.command {
            assert_eq!(chip_address, chain.chip(ChipId(i)).address);
        }
    }
}
```

**What to test**:
- Invariants (address interval, domain structure)
- Topology interpretation (parameters → correct addresses/boundaries)
- Sequence structure (command order, addresses derived from chain)
- Edge cases (single chip, max chips, overflow)

**What not to test**:
- Timing (not relevant for correctness)
- Byte-level encoding (covered by protocol layer tests)

---

## References

- S21 Pro serial capture analysis
- S19 J Pro serial capture analysis
- PROTOCOL.md: BM13xx protocol documentation