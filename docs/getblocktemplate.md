# Direct-to-node Mining (getblocktemplate)

mujina-miner can mine directly against your own Bitcoin Core node
instead of connecting to a Stratum pool. The source pulls block
templates over JSON-RPC (`getblocktemplate`), builds the coinbase
locally, and submits any network-target shares it finds via
`submitblock`.

This is the path to use for:

- **Solo mining** to your own mainnet node. The lottery odds at
  hackathon hashrates are statistically negligible, but the work
  is real and any win pays the operator-configured address.
- **Signet experimentation** (Mutinynet, etc.) where the local
  node validates blocks against a network-specific challenge. On
  signet without the signing key, every submit returns
  `bad-signet-blksig`---real PoW, real templates, demonstrably
  one signature short of acceptance.

## Selecting the source

The daemon picks between Stratum and getblocktemplate by URL
scheme on `MUJINA_POOL_URL`:

| Scheme | Source |
|--------|--------|
| `stratum+tcp://`, `stratum://`, `tcp://` | Stratum v1 |
| `http://`, `https://` | getblocktemplate |

```bash
MUJINA_POOL_URL="http://127.0.0.1:8332" \
MUJINA_PAYOUT_ADDRESS="bc1q..." \
cargo run --bin mujina-minerd
```

## Configuration

| Variable | Required | Default | Notes |
|----------|----------|---------|-------|
| `MUJINA_POOL_URL` | yes | --- | `http(s)://host:port` of the Bitcoin Core RPC. |
| `MUJINA_PAYOUT_ADDRESS` | yes | --- | Mainnet address for the (extremely improbable) block reward. Validated against the configured network at startup. |
| `MUJINA_POOL_USER` | no | (cookie) | HTTP-Basic username. If set with `MUJINA_POOL_PASS`, takes precedence over the cookie file. |
| `MUJINA_POOL_PASS` | no | (cookie) | HTTP-Basic password. |
| `MUJINA_BITCOIND_COOKIE` | no | `~/.bitcoin/.cookie` (mainnet), `~/.bitcoin/<net>/.cookie` (signet/regtest) | Path to Bitcoin Core's cookie file. Re-read on HTTP 401, so it survives a node restart. |
| `MUJINA_COINBASE_MESSAGE` | no | `/mujina-miner/` | Bytes inserted into the coinbase scriptSig after the BIP34 height push. Truncated to fit the 100-byte limit. |

The network is hard-coded to `bitcoin` (mainnet) for now. Auto-
detection from `getblockchaininfo` is a planned follow-up; until
then, signet/regtest use needs a small daemon-side patch.

## What it exposes via the API

In addition to the standard source fields, sources on this path
publish a snapshot of the block currently being mined:

```
GET /api/v0/sources/{name}/block
```

Returns a `BlockInProgress` (see `api_client::types`). 204 No
Content if the source hasn't fetched its first template yet; 404
if the source name doesn't exist.

The on-device display polls this endpoint to surface stats about
the block---top fee tx, biggest tx, halving era, retarget
position, payout address, coinbase message---updated each time the
node sends a fresh template (every block, plus mempool churn).

## Operational notes

- `submitblock` is called for real on every share that meets
  network target. There is no guard rail by default; a hit on
  mainnet ships the block. To dry-run, set
  `MUJINA_PAYOUT_ADDRESS` to an address you don't control.
- The source long-polls `getblocktemplate` so a new template
  arrives the moment your node sees a new tip. Stale-work
  windows match block intervals.
- Behind a TLS-terminating proxy, point `MUJINA_POOL_URL` at the
  proxy and supply user/pass; cookie-auth assumes a local node.
- See `mujina-miner/src/job_source/getblocktemplate.rs` and its
  submodules for the implementation.
