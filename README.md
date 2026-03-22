# CLARA Charge Overview v3

A Rust rewrite of the CLARA charge device monitoring application. Provides
real-time strip charts and controls for WCM, DQ, and Faraday Cup charge
diagnostics via EPICS Channel Access.

## Architecture

```
┌──────────────┐  WebSocket (JSON)  ┌─────────────────┐  EPICS CA  ┌──────┐
│  egui WASM   │◄──────────────────►│   axum server    │◄──────────►│ IOCs │
│  frontend    │                    │                  │            └──────┘
└──────────────┘                    │  - subscriptions │  TCP/JSON  ┌──────────┐
                                    │  - commands      │◄──────────►│ Hardware │
                                    │  - state mgmt    │            │ FE boxes │
                                    └─────────────────┘            └──────────┘
```

- **Server** (`crates/server`): Rust/axum backend. Subscribes to EPICS PVs via
  [epicars](https://github.com/ndevenish/epicars), manages device state with
  rolling buffers, broadcasts chart data at 10 Hz over WebSocket, and serves the
  WASM frontend as static files.
- **Frontend** (`crates/frontend`): egui/eframe compiled to WebAssembly. Renders
  strip charts with statistics (mean, min, max, RMSD) in pC, device controls
  (sensitivity, zero WCM, sweep timing, restore defaults), device filtering and
  reordering, freeze-able statistics, and notifications. Chart X-axes display
  human-readable timestamps (HH:MM:SS).
- **Shared** (`crates/shared`): Common types for the WebSocket protocol and YAML
  config parsing, used by both server and frontend.

State (selected sensitivities, buffer size) is persisted to `state.json` every
30 seconds using atomic writes (temp file + rename) and restored on startup.
Corrupt state files are backed up and defaults are used.

EPICS monitors are persistent — if a connection drops, each PV subscription
automatically reconnects with exponential backoff. A watchdog detects silent
connection drops (no data for 60s) and marks devices as disconnected.

All client connections and commands are logged to an append-only audit log file
in JSON-lines format, with automatic rotation at 100 MB.

WebSocket connections are rate-limited (10 commands/sec) with a 64 KB message
size cap.

## Prerequisites

- Rust toolchain (stable)
- `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`
- [Trunk](https://trunkrs.dev) for WASM builds: `cargo install trunk`
- EPICS base (for `caput` command, used as write fallback)

## Build

### Server

```bash
cargo build --release -p server
```

### Frontend (WASM)

```bash
cd crates/frontend
trunk build --release
```

This outputs the WASM bundle to `frontend_dist/` at the workspace root.

### Both together

```bash
cargo build --release -p server
cd crates/frontend && trunk build --release && cd ../..
```

## Run

```bash
# From the workspace root (after building both):
./target/release/server
```

Open `http://localhost:49195` in a browser.

## Configuration

### YAML files

| File | Description |
|---|---|
| `config/charge_devices.yaml` | Device definitions: PV names, IPs, sensitivities, defaults |
| `config/network.yaml` | EPICS CA address lists (physical & virtual networks) |

### Environment variables

| Variable | Default | Description |
|---|---|---|
| `PORT` | `49195` | HTTP server port |
| `CHARGE_CONFIG` | `config/charge_devices.yaml` | Path to device config |
| `NETWORK_CONFIG` | `config/network.yaml` | Path to network config |
| `FRONTEND_DIR` | `frontend_dist` | Directory containing WASM build output |
| `VIRTUAL` | `0` | Set to `1` to use virtual EPICS network |
| `AUDIT_LOG` | `audit.log` | Path to the audit log file |
| `RUST_LOG` | `server=info,tower_http=info` | Log level filter |

## Docker

```bash
docker build -t clara-chg-overview .
docker run -p 49195:49195 --network host clara-chg-overview
```

Use `--network host` so the container can reach the EPICS CA broadcast network.
Alternatively, pass `EPICS_CA_ADDR_LIST` explicitly:

```bash
docker run -p 49195:49195 \
  -e EPICS_CA_ADDR_LIST="192.168.83.255" \
  -e EPICS_CA_AUTO_ADDR_LIST=NO \
  clara-chg-overview
```

## Devices

| Name | Type | IP |
|---|---|---|
| CLA-S01-DIA-WCM-01 | WCM | 192.168.114.14 |
| CLA-S01-DIA-WCM-01:DQ | DQ | 192.168.114.14 |
| CLA-SP1-DIA-FCUP-01 | FCUP | 192.168.114.10 |
| CLA-SP2-DIA-FCUP-01 | FCUP | 192.168.114.11 |
| CLA-SP3-DIA-FCUP-01 | FCUP | 192.168.114.12 |
| CLA-S07-DIA-FCUP-01 | FCUP | — |
| CLA-FED-DIA-FCUP-01 | FCUP | 192.168.114.9 |

## UI Features

- **Strip charts** with live-updating charge data in pC, timestamped HH:MM:SS
- **Statistics** (mean, min, max, RMSD) per device with pC units
- **Freeze stats** button to snapshot current statistics for recording
- **Device filtering** by type (WCM / DQ / FCUP checkboxes) and individual device toggles
- **Device reordering** via up/down buttons in the controls panel
- **Last seen** timestamp per device in the controls panel
- **Sensitivity selection** dynamically from config (supports any FB level)
- **Buffer size** control to adjust rolling window length
- **Notifications** for command results and errors (auto-dismiss after 10s, errors persist)
- **Audit logging** of all client connections and commands (JSON-lines format)

## Testing

```bash
cargo test --workspace
```

Tests cover: rolling buffer operations, statistics correctness, state
persistence round-trips, corrupt file recovery, config validation, and message
serialization compatibility.

## License

Internal — STFC/CLARA.
