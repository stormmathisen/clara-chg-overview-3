//! Control of the device front-end boxes over their HTTP API (`clara-chg-fe-2`).
//!
//! The box exposes a REST API on `ip:56000` (see that repo's `API_REFERENCE.md`). We only
//! ever need two operations, so instead of PUTing the whole `Settings` object we use the
//! per-field endpoints:
//!
//!   * set sensitivity  → `POST /settings/integrator "FB{level}"`
//!   * clear calibration → `POST /settings/io/input "EXT"` then set the integrator
//!
//! Per-field writes matter because of the device's **CAL-mode gate**: a full-object POST
//! that carried any `calibration.*` field while `io.input == "EXT"` would be rejected. The
//! two things we control (integrator + input) are never gated, so they always land.
//!
//! Each endpoint returns the full updated `Settings` on 200 and a plain-text message on a
//! 4xx validation error; we only care about success vs. failure, so the body is surfaced
//! only in the error path.
//!
//! **Legacy boxes:** older front-ends (pre-`clara-chg-fe-2`) don't speak HTTP at all — they
//! take a raw-TCP JSON `Settings` blob on the same port 56000. We [`detect_api`] once per
//! control action and fall back to the [`legacy`] TCP path for those. This is transitional;
//! drop the fallback once every box is reflashed.

use serde::Serialize;
use tracing::info;

/// Which control protocol a front-end box speaks. Both live on port 56000, so we probe.
enum FrontEndApi {
    /// New `clara-chg-fe-2` firmware: HTTP REST API.
    Http,
    /// Older firmware: raw-TCP JSON `Settings` blob (see [`legacy`]).
    Legacy,
}

/// Probe a box to decide which protocol it speaks. The new firmware answers
/// `GET /settings` with 200 JSON; the legacy raw-TCP server never produces a valid HTTP
/// response, so any error or non-2xx means legacy. Bounded by the short connect timeout so
/// an old box (which never replies to our HTTP request) costs at most that.
async fn detect_api(ip: &str) -> FrontEndApi {
    detect_api_at(&base_url(ip)).await
}

async fn detect_api_at(base_url: &str) -> FrontEndApi {
    let probe = reqwest::Client::builder()
        .timeout(crate::consts::FRONT_END_CONNECT_TIMEOUT)
        .build();
    match probe {
        Ok(c) => match c.get(format!("{base_url}/settings")).send().await {
            Ok(r) if r.status().is_success() => FrontEndApi::Http,
            _ => FrontEndApi::Legacy,
        },
        Err(_) => FrontEndApi::Legacy,
    }
}

fn base_url(ip: &str) -> String {
    format!("http://{ip}:{}", crate::consts::FRONT_END_PORT)
}

fn client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(crate::consts::FRONT_END_HTTP_TIMEOUT)
        .build()?)
}

/// POST a bare JSON value (string/number/bool) to a per-field settings endpoint under
/// `{base_url}/settings/{path}`, returning an error on any non-success status. On a 4xx the
/// device's plain-text body is included in the error.
async fn post_field<T: Serialize>(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    value: &T,
) -> anyhow::Result<()> {
    let url = format!("{base_url}/settings/{path}");
    let resp = client.post(&url).json(value).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("{url} returned {status}: {body}");
    }
    Ok(())
}

/// Set a front-end's integrator (sensitivity) to `FB{level}`.
pub async fn set_sensitivity(ip: &str, level: u8) -> anyhow::Result<()> {
    if ip.is_empty() {
        anyhow::bail!("No IP address configured for device");
    }
    match detect_api(ip).await {
        FrontEndApi::Http => {
            info!("Setting integrator FB{level} on front-end {ip} (HTTP)");
            post_field(
                &client()?,
                &base_url(ip),
                "integrator",
                &format!("FB{level}"),
            )
            .await
        }
        FrontEndApi::Legacy => {
            info!("Setting integrator FB{level} on front-end {ip} (legacy TCP)");
            legacy::send_settings(ip, &legacy::settings_for_sensitivity(level)).await
        }
    }
}

/// Put a front-end back into normal operation at `FB{level}`: external input (calibration
/// mode off) plus the selected integrator. Used by clear-calibration and the post-reset
/// resend — both mean "normal operation at the selected sensitivity".
pub async fn clear_calibration(ip: &str, level: u8) -> anyhow::Result<()> {
    if ip.is_empty() {
        anyhow::bail!("No IP address configured for device");
    }
    match detect_api(ip).await {
        FrontEndApi::Http => {
            info!("Clearing calibration (EXT, FB{level}) on front-end {ip} (HTTP)");
            let client = client()?;
            let base = base_url(ip);
            post_field(&client, &base, "io/input", &"EXT").await?;
            post_field(&client, &base, "integrator", &format!("FB{level}")).await
        }
        FrontEndApi::Legacy => {
            info!("Clearing calibration (EXT, FB{level}) on front-end {ip} (legacy TCP)");
            legacy::send_settings(ip, &legacy::settings_for_clear_calibration(level)).await
        }
    }
}

/// Set the box's `meta.device_name` to `name`, but only if it has none set yet.
///
/// There is no per-field endpoint for `meta` (unlike integrator/io), so this reads the
/// whole `Settings`, and if `device_name` is empty, writes it back via full `POST /settings`
/// (which — unlike the per-field cal writes — is not CAL-gated). A name already set on the
/// box is left untouched. Modeled as an opaque `serde_json::Value` so we don't have to mirror
/// the firmware's full schema just to touch one string.
pub async fn ensure_device_name(ip: &str, name: &str) -> anyhow::Result<()> {
    if ip.is_empty() {
        anyhow::bail!("No IP address configured for device");
    }
    // Naming is HTTP-only (needs GET /settings). Legacy boxes can't do it — skip quietly.
    match detect_api(ip).await {
        FrontEndApi::Http => ensure_device_name_at(&client()?, &base_url(ip), name).await,
        FrontEndApi::Legacy => {
            info!("Legacy front-end {ip}: skipping device_name write");
            Ok(())
        }
    }
}

async fn ensure_device_name_at(
    client: &reqwest::Client,
    base_url: &str,
    name: &str,
) -> anyhow::Result<()> {
    let mut settings: serde_json::Value = client
        .get(format!("{base_url}/settings"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let current = settings
        .get("meta")
        .and_then(|m| m.get("device_name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !current.is_empty() {
        return Ok(());
    }
    settings["meta"]["device_name"] = serde_json::Value::String(name.to_string());
    info!("Setting device_name '{name}' on front-end {base_url}");
    let resp = client
        .post(format!("{base_url}/settings"))
        .json(&settings)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("{base_url}/settings returned {status}: {body}");
    }
    Ok(())
}

/// Legacy control path: older front-end boxes take the whole `Settings` object as a JSON
/// line over a raw TCP socket on port 56000 (no HTTP). Transitional — remove once every
/// box runs `clara-chg-fe-2`.
mod legacy {
    use serde::Serialize;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;
    use tracing::info;

    #[derive(Clone, Debug, Serialize)]
    pub struct FrontEndSettings {
        pub calibration: Calibration,
        pub io: InputOutput,
        pub integrator: String,
        pub power: Power,
        pub meta: Meta,
    }

    #[derive(Clone, Debug, Serialize)]
    pub struct Calibration {
        pub reference: String,
        pub level: u16,
        pub trigger: u16,
        pub offset: u16,
    }

    #[derive(Clone, Debug, Serialize)]
    pub struct InputOutput {
        pub input: String,
        pub output: String,
        pub reference: String,
    }

    #[derive(Clone, Debug, Serialize)]
    pub struct Power {
        pub positive: bool,
        pub negative: bool,
        pub integrator: bool,
    }

    #[derive(Clone, Debug, Serialize)]
    pub struct Meta {
        pub last_changed: [u64; 2],
        pub device_name: String,
        pub device_location: String,
    }

    impl Default for FrontEndSettings {
        fn default() -> Self {
            Self {
                calibration: Calibration {
                    reference: "REF2048mV".to_string(),
                    level: 128,
                    trigger: 1,
                    offset: 1,
                },
                io: InputOutput {
                    input: "EXT".to_string(),
                    output: "TERM".to_string(),
                    reference: "REF500mV".to_string(),
                },
                integrator: "FB0".to_string(),
                power: Power {
                    positive: true,
                    negative: true,
                    integrator: true,
                },
                meta: Meta {
                    last_changed: [0, 0],
                    device_name: String::new(),
                    device_location: String::new(),
                },
            }
        }
    }

    /// Settings for a given sensitivity level (0–5 → FB0–FB5); only the integrator differs
    /// from the normal-operation default (which already sets `io.input = "EXT"`).
    pub fn settings_for_sensitivity(level: u8) -> FrontEndSettings {
        FrontEndSettings {
            integrator: format!("FB{level}"),
            ..Default::default()
        }
    }

    /// Same as [`settings_for_sensitivity`] but forcing `io.input = "EXT"` (calibration off).
    pub fn settings_for_clear_calibration(level: u8) -> FrontEndSettings {
        let mut settings = settings_for_sensitivity(level);
        settings.io.input = "EXT".to_string();
        settings
    }

    /// Send a settings blob to a legacy front-end as one newline-terminated JSON line.
    pub async fn send_settings(ip: &str, settings: &FrontEndSettings) -> anyhow::Result<()> {
        send_settings_to(&format!("{ip}:{}", crate::consts::FRONT_END_PORT), settings).await
    }

    /// As [`send_settings`] but to a full `host:port` address (so tests can use a random port).
    pub async fn send_settings_to(addr: &str, settings: &FrontEndSettings) -> anyhow::Result<()> {
        let mut stream = tokio::time::timeout(
            crate::consts::FRONT_END_CONNECT_TIMEOUT,
            TcpStream::connect(addr),
        )
        .await??;
        let json = serde_json::to_string(settings)?;
        stream.write_all(json.as_bytes()).await?;
        stream.write_all(b"\n").await?;
        stream.flush().await?;
        info!("Legacy settings sent to {addr}");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        extract::State,
        http::{Request, StatusCode},
        routing::post,
        Json, Router,
    };
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    // Records (path, raw-json-body) of each POST the fake front-end receives.
    type Log = Arc<Mutex<Vec<(String, String)>>>;

    async fn record(State(log): State<Log>, req: Request<Body>) -> Json<serde_json::Value> {
        let path = req.uri().path().to_string();
        let bytes = to_bytes(req.into_body(), 64 * 1024).await.unwrap();
        let body = String::from_utf8_lossy(&bytes).to_string();
        log.lock().unwrap().push((path, body));
        Json(serde_json::json!({})) // device returns full Settings; the client ignores it
    }

    async fn reject() -> (StatusCode, &'static str) {
        (StatusCode::BAD_REQUEST, "Offset must be at least 1")
    }

    /// Stand up a throwaway HTTP server standing in for a front-end box; returns its base URL.
    async fn spawn_fake_fe() -> (String, Log) {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/settings/integrator", post(record))
            .route("/settings/io/input", post(record))
            .route("/settings/bad", post(reject))
            .with_state(log.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), log)
    }

    #[tokio::test]
    async fn per_field_posts_land_with_enum_string_bodies() {
        let (base, log) = spawn_fake_fe().await;
        let client = client().unwrap();

        // Mirrors clear_calibration's two writes.
        post_field(&client, &base, "io/input", &"EXT")
            .await
            .unwrap();
        post_field(&client, &base, "integrator", &format!("FB{}", 3))
            .await
            .unwrap();

        let entries = log.lock().unwrap().clone();
        assert_eq!(
            entries,
            vec![
                ("/settings/io/input".to_string(), "\"EXT\"".to_string()),
                ("/settings/integrator".to_string(), "\"FB3\"".to_string()),
            ]
        );
    }

    /// Fake front-end exposing GET/POST /settings backed by a shared Settings JSON, so a
    /// POST is visible to a following GET. Returns base URL + the shared settings handle.
    async fn spawn_fake_settings(device_name: &str) -> (String, Arc<Mutex<serde_json::Value>>) {
        let settings = Arc::new(Mutex::new(serde_json::json!({
            "integrator": "FB5",
            "meta": { "device_name": device_name }
        })));
        let get = {
            let s = settings.clone();
            move || {
                let s = s.clone();
                async move { Json(s.lock().unwrap().clone()) }
            }
        };
        let set = {
            let s = settings.clone();
            move |Json(body): Json<serde_json::Value>| {
                let s = s.clone();
                async move {
                    *s.lock().unwrap() = body.clone();
                    Json(body)
                }
            }
        };
        let app = Router::new().route("/settings", axum::routing::get(get).post(set));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), settings)
    }

    #[tokio::test]
    async fn sets_name_only_when_box_has_none() {
        // Empty name on the box → we write the configured name.
        let (base, settings) = spawn_fake_settings("").await;
        ensure_device_name_at(&client().unwrap(), &base, "CLA-DEV-01")
            .await
            .unwrap();
        assert_eq!(
            settings.lock().unwrap()["meta"]["device_name"],
            "CLA-DEV-01"
        );

        // Name already present → left untouched, no overwrite.
        let (base, settings) = spawn_fake_settings("EXISTING").await;
        ensure_device_name_at(&client().unwrap(), &base, "CLA-DEV-01")
            .await
            .unwrap();
        assert_eq!(settings.lock().unwrap()["meta"]["device_name"], "EXISTING");
    }

    #[tokio::test]
    async fn non_success_status_surfaces_body_as_error() {
        let (base, _log) = spawn_fake_fe().await;
        let err = post_field(&client().unwrap(), &base, "bad", &0u16)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("400"), "{err}");
        assert!(err.contains("Offset must be at least 1"), "{err}");
    }

    #[tokio::test]
    async fn detects_http_rest_box() {
        let (base, _settings) = spawn_fake_settings("X").await;
        assert!(matches!(detect_api_at(&base).await, FrontEndApi::Http));
    }

    /// Raw-TCP fake standing in for a legacy box: records the first line of each connection
    /// then closes. Not HTTP, so the probe classifies it as legacy.
    async fn spawn_raw_tcp() -> (String, Arc<Mutex<Vec<String>>>) {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let store = lines.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let store = store.clone();
                tokio::spawn(async move {
                    let mut line = String::new();
                    if BufReader::new(sock).read_line(&mut line).await.is_ok()
                        && !line.trim().is_empty()
                    {
                        store.lock().unwrap().push(line.trim_end().to_string());
                    }
                    // socket drops → connection closes; the HTTP probe sees no valid
                    // response and falls back to legacy.
                });
            }
        });
        (addr, lines)
    }

    #[tokio::test]
    async fn detects_legacy_and_sends_json_blob() {
        let (addr, lines) = spawn_raw_tcp().await;

        assert!(matches!(
            detect_api_at(&format!("http://{addr}")).await,
            FrontEndApi::Legacy
        ));

        legacy::send_settings_to(&addr, &legacy::settings_for_sensitivity(3))
            .await
            .unwrap();

        // send_settings_to returns once flushed; the server records asynchronously.
        let blob = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let Some(l) = lines
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|l| l.starts_with('{'))
                    .cloned()
                {
                    return l;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("no JSON line received from legacy box");

        let v: serde_json::Value = serde_json::from_str(&blob).unwrap();
        assert_eq!(v["integrator"], "FB3");
        assert_eq!(v["io"]["input"], "EXT");
    }
}
