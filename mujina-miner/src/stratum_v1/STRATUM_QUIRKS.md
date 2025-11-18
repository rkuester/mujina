# Stratum v1 Protocol Quirks

This document describes non-obvious aspects of the Stratum v1 mining protocol
that differ from standard Bitcoin data formats.

## Block Hash Encoding ("Word-Swap")

### The Problem

Stratum v1 uses a peculiar encoding for the `prev_blockhash` field in
`mining.notify` that differs from both human-readable and internal
representations.

### Three Representations

A block hash exists in three formats:

1. **Human-readable (display)**: Big-endian, as shown in block explorers
   ```
   000000000000000000015296bc96391d0d67f4a301f2d4fc6db962c16b6455fd
   ```

2. **Internal (byte array)**: Little-endian, used in Bitcoin's wire protocol
   ```
   [0xfd, 0x55, 0x64, 0x6b, 0xc1, 0x62, 0xb9, 0x6d, ...]
   ```

3. **Stratum v1**: "Word-swapped" - 8 little-endian 4-byte words, transmitted
   as big-endian hex within each word
   ```
   6b6455fd6db962c101f2d4fc0d67f4a3bc96391d000152960000000000000000
   ```

### Visual Breakdown

```
Stratum:  "6b6455fd 6db962c1 01f2d4fc 0d67f4a3 bc96391d 00015296 00000000 00000000"
           |------| |------| |------| |------| |------| |------| |------| |------|
             W0       W1       W2       W3       W4       W5       W6       W7
           (each word is 4 bytes shown in big-endian hex)

Word-swap each 4-byte chunk:
Internal: [fd 55 64 6b] [c1 62 b9 6d] [fc d4 f2 01] [a3 f4 67 0d] ...
           |--reversed-|  |--reversed-|  |--reversed-|  |--reversed-|

Display (reverse entire hash):
          000000000000000000015296bc96391d0d67f4a301f2d4fc6db962c16b6455fd
```

### Why This Encoding?

Historical accident. Early Stratum implementations (circa 2012) were optimized
for 32-bit systems and treated the 256-bit hash as an array of eight 32-bit
unsigned integers. Each integer was little-endian internally, but when
serialized to hex for JSON transmission, the bytes within each word appeared in
big-endian order.

### Conversion Algorithm

To convert from Stratum format to internal `BlockHash`:

```rust
let mut bytes = hex::decode(stratum_hex)?;
for chunk in bytes.chunks_mut(4) {
    chunk.reverse();  // Swap bytes within each 4-byte word
}
let hash = BlockHash::from_slice(&bytes)?;
```

### Validation

Our implementation is validated against real mining pool captures in
`test_data::esp_miner_job`. The test
`test_job_notification_from_real_bitaxe_capture` verifies that we correctly
parse a Stratum job that resulted in an accepted share at difficulty 29588.

## Other Fields

### Merkle Branches

**Not word-swapped**. Merkle branches are transmitted as straightforward hex
strings in internal byte order.

```
Stratum: "21af451ddb51e887ff1feb5592b87290098565035eb8500031aedcc776d4e72a"
Decode:  [0x21, 0xaf, 0x45, 0x1d, 0xdb, 0x51, ...] (no reversal needed)
```

### Version, nbits, ntime

Transmitted as **big-endian hex strings**:

- `version`: `"20000000"` -> `0x20000000` -> `Version::from_consensus(0x20000000)`
- `nbits`: `"17023a04"` -> `0x17023a04` -> `CompactTarget::from_consensus(0x17023a04)`
- `ntime`: `"685468d7"` -> `0x685468d7` (Unix timestamp)

### Extranonce2, Nonce

When submitting shares, these are transmitted as **little-endian hex strings**:

```rust
let nonce: u32 = 0x12345678;
let hex = format!("{:08x}", nonce);  // "12345678" (little-endian representation)
```

## References

- Real pool capture: `asic/bm13xx/test_data.rs`
- Stratum v1 spec: https://en.bitcoin.it/wiki/Stratum_mining_protocol
- Word-swap discussion: https://github.com/slushpool/stratumprotocol/issues/9
- Implementation: `stratum_v1/messages.rs::parse_block_hash()`

## Testing Strategy

Always validate protocol parsing against known-good data from real mining
round-trips. The `test_data` module serves as our rosetta stone, showing the
exact transformations between:

1. Stratum v1 JSON strings
2. Rust Bitcoin types
3. BM13xx wire protocol bytes

Any changes to parsing logic should be validated against this real capture to
ensure correctness.
