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

use serde::Serialize;
use tracing::info;

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
    info!("Setting integrator FB{level} on front-end {ip}");
    post_field(
        &client()?,
        &base_url(ip),
        "integrator",
        &format!("FB{level}"),
    )
    .await
}

/// Put a front-end back into normal operation at `FB{level}`: external input (calibration
/// mode off) plus the selected integrator. Used by clear-calibration and the post-reset
/// resend — both mean "normal operation at the selected sensitivity".
pub async fn clear_calibration(ip: &str, level: u8) -> anyhow::Result<()> {
    if ip.is_empty() {
        anyhow::bail!("No IP address configured for device");
    }
    info!("Clearing calibration (EXT, FB{level}) on front-end {ip}");
    let client = client()?;
    let base = base_url(ip);
    post_field(&client, &base, "io/input", &"EXT").await?;
    post_field(&client, &base, "integrator", &format!("FB{level}")).await
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
    ensure_device_name_at(&client()?, &base_url(ip), name).await
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
}
