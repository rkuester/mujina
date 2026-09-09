# REST API

Mujina exposes an HTTP API on port 7785 (ASCII "MU") for
monitoring and control. It binds to localhost by default. Set
`MUJINA_API_LISTEN` to override the listen address:

```bash
MUJINA_API_LISTEN="0.0.0.0" cargo run
```

The port defaults to 7785 if not specified, or you can
override it with `MUJINA_API_LISTEN="0.0.0.0:9000"`.

The API currently has no authentication or encryption, so
binding to a non-localhost address exposes it to the network
without access control. These will be addressed later.

An OpenAPI spec is served at `/api/v0/openapi.json`. A Swagger
UI is available at `/swagger-ui` for interactive browsing.

## Versioning

All endpoints live under `/api/v0/`. The v0 prefix signals an
unstable API: breaking changes are expected until Mujina
matures. Once the API stabilizes it will move to v1, and
backwards-incompatible changes will increment the version
number from there.

## Data model

The API is a view over the systree, the one tree holding every
value the miner has. A URL below `/api/v0` is the JSON Pointer
into that tree, so `/boards` returns every board and
`/boards/bitaxe-gamma-e2f56f9b/regulators/core/voltage_v` returns
one reading. The same URL a client reads is the URL it writes.

Two documents are not in the tree yet and answer at their own
paths. `/miner` holds the scheduler's totals and `/sources` holds
the job sources. Both move into the tree when it holds what they
report.

## Conventions

### Null values

Sensor readings (`rpm`, `temperature_c`, `voltage_v`, etc.) are
nullable. A null value means the hardware read failed or the
sensor is not present. Clients should treat null as "unknown,"
not zero.

Setpoints (`target_voltage_v`, `target_duty_percent`) are also
nullable. Null means no client has set the value and the daemon
decides it, which for a fan is full power until a thermal control
loop exists. Null is a value a client can write, not an absence.

### Units

All values are in raw SI-ish units. Clients are responsible for
formatting and unit conversion.

| Field suffix | Unit                   |
|--------------|------------------------|
| `_secs`      | seconds                |
| `_c`         | degrees Celsius        |
| `_v`         | volts                  |
| `_a`         | amperes                |
| `_w`         | watts                  |
| `rpm`        | revolutions per minute |
| `_per_s`     | events per second      |
| `_percent`   | percent                |

An attribute's name carries its unit, and its Rust type supplies
the suffix, so every attribute of one type takes the same one.
Hashrate is `hashes_per_s`.

### Chips

A thread over a chain of BM13xx chips holds a `chips` subtree with
one node per chip, keyed by chain index. Each node reports the
chip's `address`, the `nonces` it has reported since the thread
started, the `hardware_errors` among them (a hash short of the
chip's own ticket mask), the `core_register_reports` it sent with
no read from the host, and `hashes_per_s` estimated from its
nonces over the last five minutes. A BM1362 stamps its address
into each nonce, so every nonce is attributed; a BM1370 does not,
so its nonces are attributed only on a chain of one chip.

### Power

`target_power_fraction` at the root of the tree is the share of
full power a client asks of the whole miner, 0.0 to 1.0, where
0.0 is off and 1.0, the default, is full. The scheduler gives
every thread that share. Each thread has a `target_power_fraction`
of its own, unset by default; a value written there pins that
thread over the miner's share while it stands, and writing `null`
or deleting it returns the thread to the miner's share.

What a share of full power means is the thread's business. A
BM13xx thread models its power as core voltage squared times
clock along its voltage curve, picks the frequency whose modeled
power is that share of full, and ramps there; at 0.0 it holds
the chain in reset with the rail off and keeps its work for when
the share comes back. A CPU thread scales its default duty. Each
thread reports the rate it expects at its operating point in
`hashes_per_s`.

A board may cap what its thread holds. Each thread reports
`power_ceiling`, the most of its full power the board allows,
and `power_fraction`, the share it holds, which is the smaller
of what it was asked and the ceiling. An EmberOne holds its chip
diode at `chip_target_c`, a setting on the board defaulting to
68 C, by moving the ceiling half a percent a second for every
degree the chip sits from the target, down when over and up when
under. The ceiling creeps rather than jumps, so it settles where
the chip holds its target instead of hunting with the die. A
thread reporting a ceiling under 1.0 is throttled by heat; the
ask stands and is honored again as the chip cools.

```bash
# Run the whole miner at half power
curl -X PUT -H 'content-type: application/json' -d '0.5' \
  http://127.0.0.1:7785/api/v0/target_power_fraction

# Hold one thread at three quarters whatever the miner's share
curl -X PUT -H 'content-type: application/json' -d '0.75' \
  http://127.0.0.1:7785/api/v0/boards/emberone-00-184d7fe4/threads/0/target_power_fraction
```

### Naming

A board's key is its model and its serial joined, one segment, as
in `bitaxe-gamma-e2f56f9b`. Sources are identified by a
URL-friendly `name` field.

## Endpoints

The OpenAPI spec is the authoritative endpoint reference. This
table is a summary and may not be exhaustive.

### Miner (aggregate)

| Method | Path         | Description                    |
|--------|--------------|--------------------------------|
| GET    | `/miner`     | Full state snapshot            |
| PATCH  | `/miner`     | Update miner config (e.g. pause) |

### Sources

| Method | Path              | Description          |
|--------|-------------------|----------------------|
| GET    | `/sources`        | List job sources     |
| GET    | `/sources/{name}` | Single source detail |

### Health

| Method | Path      | Description          |
|--------|-----------|----------------------|
| GET    | `/health` | Returns "OK"         |

### The systree

Every other path under `/api/v0` is a JSON Pointer into the
systree. One mechanism serves a single attribute, a whole board,
or the entire miner.

| Method | Path      | Description                                 |
|--------|-----------|---------------------------------------------|
| GET    | `/{path}` | The subtree at this path                    |
| PUT    | `/{path}` | Replace the value at this path              |
| PATCH  | `/{path}` | Replace the values a partial document names |
| DELETE | `/{path}` | Forget what a client wrote at this path     |

```bash
curl http://127.0.0.1:7785/api/v0/boards/bitaxe-gamma-e2f56f9b
curl -X PUT -H 'content-type: application/json' -d '1.2' \
  http://127.0.0.1:7785/api/v0/boards/bitaxe-gamma-e2f56f9b/regulators/core/target_voltage_v
```

A board's key is its model and its serial joined, one segment, so
`/boards` stays a flat list. Reading with `?origins=true` pairs the
values with the configuration layer each setting came from.

A `*` in a write is a standing rule. It applies to every matching
attribute, including attributes that appear later. A `*` alone
stands for any one name, and a `*` beside other text matches
within that one segment, so `/boards/emberone-00-*/threads/*` names
every thread of every EmberOne. Where two rules match, the one
spelling out more of the path wins. A DELETE forgets what a client
wrote so the layer beneath takes over again.

The `/miner` and `/sources` documents above are more specific
routes, so they shadow the tree at those two paths until the tree
holds what they report.

All paths are relative to `/api/v0`.

## Types

The `/miner` and `/sources` documents are defined in Rust in the
`api_client::types` module
(`mujina-miner/src/api_client/types.rs`), and the OpenAPI schema
is derived from them automatically. Everything under the systree
is typed at its attribute rather than in a document struct, so it
has no schema entry.
