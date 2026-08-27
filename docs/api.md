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
