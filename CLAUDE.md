# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A Rust rewrite of the CLARA charge device monitoring app. An axum server subscribes
to EPICS PVs, keeps rolling buffers of charge data, and streams chart snapshots over
WebSocket to an egui/WASM frontend that also issues device-control commands. See
`README.md` for the deployment-facing overview; this file covers what you need to work
on the code.

## Build & test

The workspace `default-members` is `shared` + `server` only — **`cargo build` does not
build the frontend.** The frontend is a separate WASM target built with Trunk.

```bash
# Server (native)
cargo build --release -p server

# Frontend (WASM) — outputs to frontend_dist/ at the workspace root (see Trunk.toml)
cd crates/frontend && trunk build --release

# Run (from workspace root, after building both)
./target/release/server            # serves http://localhost:49195

# Tests — all live in #[cfg(test)] modules in server + shared (frontend has none)
cargo test                         # whole workspace
cargo test -p server               # one crate
cargo test -p server commands::    # tests in one module
cargo test valid_config_loads      # one test by name
```

Frontend prerequisites: `rustup target add wasm32-unknown-unknown` and `cargo install trunk`.

Docker build/deploy wrappers live in `build.sh` and `build_and_deploy.sh` (they build the
image, push to the DL registry, and restart the `clara-chg-overview` container with
`--network host`). The `Dockerfile` builds both server and WASM in one multi-stage image.

## Architecture

Three crates (`crates/`):

- **`shared`** — the WebSocket wire protocol (`messages.rs`: `ServerMessage` /
  `ClientMessage`, both `#[serde(tag = "type")]`) and YAML config types (`config.rs`).
  Compiled into both server and frontend, so it must stay `wasm`-safe (no tokio/std-net).
  **Changing a message or config type is a protocol change on both ends** — update server
  and frontend together.
- **`server`** — axum backend, EPICS integration, all device logic.
- **`frontend`** — eframe/egui app compiled to WASM (`app.rs` UI loop, `controls.rs`
  device controls, `strip_chart.rs` plots, `ws_client.rs` socket).

### Data flow

`main.rs` wires everything as long-lived tokio tasks over one shared
`AppState = Arc<RwLock<InnerState>>`:

- **Reads (EPICS → state):** `epics.rs` spawns one `persistent_monitor` task per device's
  `charge` PV using the native `epicars` CA client. Each reconnects with exponential
  backoff on any failure. Updates flow through an mpsc channel into `InnerState`.
- **Broadcast (state → clients):** `ws.rs` `spawn_chart_broadcaster` samples all buffers
  at 10 Hz and pushes `ChartData` snapshots to every connected client.
- **Commands (clients → hardware):** `ws.rs` receives `ClientMessage`s;
  `commands.rs` executes them (see write paths below).
- **Front-end events (box → state):** `fe_events.rs` spawns one SSE listener per device
  with a front-end box (`GET ip:56000/events`, reconnecting with the same backoff). The box
  streams its full `Settings` on *any* setting change from *any* client, so this catches a
  sensitivity changed outside this program (device web UI, front panel) — it reconciles
  `current_sensitivity` and pushes a `StateUpdate` + notification. Our own writes are
  de-duped implicitly: `handle_set_sensitivity` updates `current_sensitivity` before the
  echo arrives, so the index already matches and no notification fires.
- **Watchdog:** marks a device disconnected after `WATCHDOG_STALE_SECS` (60s) with no data.
- **Front-end ping:** every 30s, TCP-connects to each device `ip:56000` to set `fe_alive`.
- **Persistence:** every 30s, `state.json` is written atomically (temp + rename). Holds
  selected sensitivities, buffer size, device order. Corrupt files are backed up; defaults used.
- **Audit:** `audit.rs` append-only JSON-lines log of connections/commands, rotates at 100 MB.

### Two distinct hardware write paths (important)

Device control targets two different transports:

1. **`hardware.rs`** — POSTs to the device front-end box's **HTTP API** at **`ip:56000`**
   (the `clara-chg-fe-2` firmware; see its `API_REFERENCE.md`). This is how sensitivity/gain
   and clear-calibration are applied, via per-field endpoints (`POST /settings/integrator`,
   `POST /settings/io/input`) using a `reqwest` client (`set_sensitivity`, `clear_calibration`).
   Per-field writes dodge the device's CAL-mode gate that would reject a full-object POST. This
   is *not* EPICS at all. **Dual-protocol:** each control action first `detect_api`s the box
   (a short `GET /settings` probe); older front-ends that don't speak HTTP fall back to the
   `legacy` module's raw-TCP JSON `Settings` blob on the same port. Transitional — drop the
   fallback once every box runs `clara-chg-fe-2`.
2. **`epics::caput`** — writes scalar PVs over Channel Access using the **native
   `epicars` client** (`Client::write_pv`): `corrA`/`corrB` (zero-WCM), `DQcal`,
   sweep-timing windows, restore-defaults. An `f64` becomes a `DbrValue::Double`.

So EPICS is both **read and written** through the native `epicars` client. The name
`caput` is kept for the function only because it's the vocabulary operators use — no
external binary is involved, nothing needs to be on `PATH`, and the Docker image ships
no EPICS base.

Writes share **one lazily-built `Client`** behind a mutex (`WRITE_CLIENT` in `epics.rs`).
Building a `Client` costs ~83 ms (CA startup), while a write on an existing one costs
~0.3 ms — so a fresh client per write would be *slower than the old `caput` shell-out*
(~34 ms). A failed write clears the cached client so the next attempt reconnects rather
than reusing a dead circuit. Writes are bounded by `WRITE_TIMEOUT` (5s).

Note `persistent_monitor` and `collect_waveforms` still build their own clients — they are
long-lived subscriptions, not per-call operations.

`epics.rs` has an end-to-end test (`caput_writes_scalar_over_channel_access`) that stands
up an in-process `epicars` CA server via `IntercomProvider`, points the client at it with
the standard `EPICS_CA_*` env vars, and asserts the value actually lands — no EPICS base
or external IOC required. It sets process-wide env, so don't run another CA test beside it.

> History: `97e06fd` introduced the native write path, but merge `f74d46e` reverted just
> the `epics.rs` half while keeping the rest, silently returning the code to shelling out
> to `caput`. It was reapplied deliberately, and the EPICS-from-source Docker stage
> (added in `de315be` to make the shell-out work) was removed again.

### Device model & sensitivity

- Devices are defined in `config/charge_devices.yaml`: `type` (`wcm`/`dq`/`fcup`/`ict`),
  `digitizer`, `ip`, a `sensitivities` array, a `pvs` map (must contain `charge`), and a
  `defaults` map. Config is validated on load (`server/config.rs`).
- `current_sensitivity` is an **index into the `sensitivities` array**, not the gain value.
  It defaults to the *last* index (max sensitivity) when nothing is persisted.
- **ICT devices have no `sensitivities`** (the field is `#[serde(default)]`, so it is an
  empty vec). They are excluded from sensitivity validation, `SetSensitivity`, and
  `ClearCalibration`, and they have no front-end box (`ip` is empty).
- `defaults` values are `DefaultValue` (untagged: scalar or per-sensitivity array).
  `for_sensitivity(index)` returns the array element for the current sensitivity (or the
  scalar / last element). Many calibration constants are per-sensitivity arrays, and their
  length must match `sensitivities`.
- **WCM ↔ DQ coupling:** setting a WCM device's sensitivity also writes the `DQcal` PV of
  its companion `{name}:DQ` device. Keep this in mind when touching `handle_set_sensitivity`.

### Config & environment

- `config/network.yaml` provides EPICS CA env vars for two networks (`PHYSICAL` / `VIRTUAL`).
  `apply_epics_env` sets them as process env before the client starts.
- **`VIRTUAL=1`** selects the virtual network config (test/sim); default is physical.
- Other env: `PORT` (49195), `CHARGE_CONFIG`, `NETWORK_CONFIG`, `FRONTEND_DIR`, `AUDIT_LOG`.

## Porting context

This is a rewrite of an earlier Python app; some features are still being ported. The
authoritative pre-rewrite Python source location is noted in Claude's memory
(`chg-overview-python-reference`) — consult it when reproducing legacy behavior.
