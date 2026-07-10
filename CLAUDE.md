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
- **Watchdog:** marks a device disconnected after `WATCHDOG_STALE_SECS` (60s) with no data.
- **Front-end ping:** every 30s, TCP-connects to each device `ip:56000` to set `fe_alive`.
- **Persistence:** every 30s, `state.json` is written atomically (temp + rename). Holds
  selected sensitivities, buffer size, device order. Corrupt files are backed up; defaults used.
- **Audit:** `audit.rs` append-only JSON-lines log of connections/commands, rotates at 100 MB.

### Two distinct hardware write paths (important)

Device control does **not** go through the epicars client. Depending on the target:

1. **`hardware.rs`** — serializes `FrontEndSettings` to JSON and sends it over a raw TCP
   socket to the device front-end box at **`ip:56000`**. This is how sensitivity/gain and
   clear-calibration are applied (`settings_for_sensitivity`, `send_settings`).
2. **`epics::caput`** — shells out to the external **`caput` binary** (EPICS base) to write
   scalar PVs: `corrA`/`corrB` (zero-WCM), `DQcal`, sweep-timing windows, restore-defaults.
   `caput` must be on `PATH` at runtime or these writes fail (logged, non-fatal).

   The `Dockerfile` builds **EPICS base from source** (`epics-builder` stage: git clone at
   `ARG EPICS_VERSION`, default `R7.0.8.1`, then `make`) and copies `bin/linux-x86_64` and
   `lib/linux-x86_64` into the runtime image under the **same `/epics-base` prefix** — the
   binaries carry an rpath of `/epics-base/lib/linux-x86_64`, so relocating them would make
   library resolution depend on `LD_LIBRARY_PATH` alone. Runtime needs `libreadline8` +
   `libncurses6` (libCom links them). The image build asserts `caput -h` runs, so a broken
   EPICS copy fails the build rather than silently degrading PV writes. Locally you need
   `caput` on `PATH` yourself for writes to work.

So EPICS is read via the native client but written via the `caput` CLI.

> Note: commit `97e06fd` replaced these shell-outs with the epicars native write API
> (`client.write_pv`), but the merge `f74d46e` reverted that change in `epics.rs` while
> keeping the rest of the commit. `main` therefore still shells out to `caput`. The
> startup check and the EPICS-in-image build were restored in `de315be`; re-applying the
> *native write path* remains an open decision.

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
