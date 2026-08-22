//! Licence checking.
//!
//! Skimrr never contacts Lemon Squeezy. It asks a Worker we control, which holds the
//! API key and returns a receipt signed with Ed25519. Only the public half is
//! compiled in here, so a receipt cannot be forged without the Worker's private key.
//!
//! The rule that matters for customers: **only an explicit refusal deactivates**.
//! A missing network, a Worker outage, a DNS failure: none of them ever take the
//! app away from someone who paid.

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::State;

/// Replace with your own deployment. This is the only address the app ever calls.
const LICENSE_ENDPOINT: &str = "https://license.skimrr.com";

/// Printed by `npm run keys` in skimrr-license-worker. Public: safe to ship.
const SIGNING_PUBLIC_KEY: &str = "NFhVb89lXFiZlpldLQ0xBKj660k1LQqMKEPjOfq3/I0=";

const KEYCHAIN_SERVICE: &str = "com.skimrr.app";
const KEYCHAIN_ENTRY: &str = "licence";

/// What the Worker signed. Mirrors the shape produced in `src/index.ts`.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct Receipt {
    key_hash: String,
    instance_id: String,
    status: String,
    #[serde(default)]
    product_id: u64,
    refresh_after: i64,
    issued_at: i64,
    #[serde(default)]
    activation_limit: u32,
    #[serde(default)]
    activation_usage: u32,
    /// Latest published version, carried by the Worker. Absent on older receipts.
    #[serde(default)]
    latest_version: String,
}

/// Everything kept in the OS keychain, as one blob.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct StoredLicence {
    /// `payload.signature`, exactly as the Worker returned it.
    token: String,
    /// Needed to revalidate and to release the device later.
    license_key: String,
    /// Random, generated once. Not derived from the hardware: it says nothing about
    /// the machine, which is the point.
    device_id: String,
}

/// What the interface is allowed to know.
#[derive(Debug, Clone, Serialize)]
pub struct LicenceInfo {
    pub activated: bool,
    pub status: String,
    pub activation_usage: u32,
    pub activation_limit: u32,
    /// Set when the last activation attempt failed, so the screen can explain itself.
    pub message: Option<String>,
    /// Newer version published, when the receipt announces one. The check rides along
    /// with the licence refresh, so no extra request is ever made.
    pub update_available: Option<String>,
}

impl LicenceInfo {
    fn inactive(message: Option<String>) -> Self {
        LicenceInfo {
            activated: false,
            status: "none".into(),
            activation_usage: 0,
            activation_limit: 0,
            message,
            update_available: None,
        }
    }
}

pub struct LicenceState(Mutex<Option<StoredLicence>>);

impl LicenceState {
    pub fn new() -> Self {
        LicenceState(Mutex::new(load_from_keychain()))
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ENTRY).map_err(|e| e.to_string())
}

fn load_from_keychain() -> Option<StoredLicence> {
    let raw = entry().ok()?.get_password().ok()?;
    serde_json::from_str(&raw).ok()
}

fn save_to_keychain(licence: &StoredLicence) -> Result<(), String> {
    let raw = serde_json::to_string(licence).map_err(|e| e.to_string())?;
    entry()?.set_password(&raw).map_err(|e| e.to_string())
}

fn clear_keychain() {
    if let Ok(e) = entry() {
        let _ = e.delete_credential();
    }
}

/// Verify the Worker's signature and decode the receipt. A tampered or home-made
/// token fails here, which is the whole point of signing.
fn verify(token: &str) -> Result<Receipt, String> {
    verify_with(token, SIGNING_PUBLIC_KEY)
}

fn verify_with(token: &str, public_key: &str) -> Result<Receipt, String> {
    let (payload, signature) = token
        .split_once('.')
        .ok_or_else(|| "malformed receipt".to_string())?;

    let key_bytes: [u8; 32] = STANDARD
        .decode(public_key)
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "bad public key".to_string())?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|e| e.to_string())?;

    let sig_bytes: [u8; 64] = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "bad signature length".to_string())?;

    key.verify(payload.as_bytes(), &Signature::from_bytes(&sig_bytes))
        .map_err(|_| "signature does not match".to_string())?;

    let json = URL_SAFE_NO_PAD.decode(payload).map_err(|e| e.to_string())?;
    serde_json::from_slice(&json).map_err(|e| e.to_string())
}

/// Compare dotted versions numerically. String ordering would rank "0.10.0" below
/// "0.9.0" and silently stop announcing updates after the tenth release.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u32> {
        v.split(['.', '-'])
            .map(|p| {
                p.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
            })
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    let (a, b) = (parts(candidate), parts(current));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    false
}

fn info_from(receipt: &Receipt) -> LicenceInfo {
    let update = (!receipt.latest_version.is_empty()
        && is_newer(&receipt.latest_version, env!("CARGO_PKG_VERSION")))
    .then(|| receipt.latest_version.clone());

    LicenceInfo {
        activated: receipt.status == "active",
        status: receipt.status.clone(),
        activation_usage: receipt.activation_usage,
        activation_limit: receipt.activation_limit,
        message: None,
        update_available: update,
    }
}

#[derive(Deserialize)]
struct WorkerOk {
    receipt: String,
}

#[derive(Deserialize)]
struct WorkerErr {
    error: String,
    message: String,
}

/// Outcome of talking to the Worker. `Unreachable` is deliberately distinct from a
/// refusal: it must never cost anyone their licence.
enum WorkerReply {
    Receipt(String),
    Refused { code: String, message: String },
    Unreachable,
}

async fn call_worker(path: &str, body: serde_json::Value) -> WorkerReply {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()
    {
        Ok(c) => c,
        Err(_) => return WorkerReply::Unreachable,
    };

    let response = match client
        .post(format!("{LICENSE_ENDPOINT}{path}"))
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return WorkerReply::Unreachable,
    };

    let status = response.status();
    // 5xx means our side is broken, not the customer's licence.
    if status.is_server_error() {
        return WorkerReply::Unreachable;
    }
    let text = response.text().await.unwrap_or_default();

    if status.is_success() {
        return match serde_json::from_str::<WorkerOk>(&text) {
            Ok(ok) => WorkerReply::Receipt(ok.receipt),
            Err(_) => WorkerReply::Unreachable,
        };
    }
    match serde_json::from_str::<WorkerErr>(&text) {
        Ok(err) => WorkerReply::Refused {
            code: err.error,
            message: err.message,
        },
        Err(_) => WorkerReply::Unreachable,
    }
}

fn device_name() -> String {
    // Shown in the customer's Lemon Squeezy portal so they can tell devices apart.
    hostname().unwrap_or_else(|| "Skimrr".into())
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
}

#[tauri::command]
pub fn licence_status(state: State<LicenceState>) -> LicenceInfo {
    let stored = state.0.lock().unwrap();
    match stored.as_ref() {
        Some(licence) => match verify(&licence.token) {
            Ok(receipt) => info_from(&receipt),
            // A receipt that no longer verifies is worthless; treat it as absent.
            Err(_) => LicenceInfo::inactive(None),
        },
        None => LicenceInfo::inactive(None),
    }
}

#[tauri::command]
pub async fn activate_licence(
    state: State<'_, LicenceState>,
    key: String,
) -> Result<LicenceInfo, String> {
    let key = key.trim().to_string();
    if key.is_empty() {
        return Ok(LicenceInfo::inactive(Some("empty_key".into())));
    }
    let device_id = uuid::Uuid::new_v4().to_string();

    let reply = call_worker(
        "/activate",
        serde_json::json!({
            "license_key": key,
            "instance_name": format!("{} ({})", device_name(), &device_id[..8]),
        }),
    )
    .await;

    match reply {
        WorkerReply::Receipt(token) => {
            let receipt = verify(&token)?;
            let licence = StoredLicence {
                token,
                license_key: key,
                device_id,
            };
            save_to_keychain(&licence)?;
            *state.0.lock().unwrap() = Some(licence);
            Ok(info_from(&receipt))
        }
        WorkerReply::Refused { code, message } => {
            Ok(LicenceInfo::inactive(Some(if message.is_empty() {
                code
            } else {
                message
            })))
        }
        WorkerReply::Unreachable => Ok(LicenceInfo::inactive(Some("offline".into()))),
    }
}

#[tauri::command]
pub async fn deactivate_licence(state: State<'_, LicenceState>) -> Result<LicenceInfo, String> {
    let stored = { state.0.lock().unwrap().clone() };
    if let Some(licence) = stored {
        if let Ok(receipt) = verify(&licence.token) {
            let _ = call_worker(
                "/deactivate",
                serde_json::json!({
                    "license_key": licence.license_key,
                    "instance_id": receipt.instance_id,
                }),
            )
            .await;
        }
    }
    clear_keychain();
    *state.0.lock().unwrap() = None;
    Ok(LicenceInfo::inactive(None))
}

/// Background refresh, run once at startup. Silent by design: it never interrupts,
/// and it only ever gives up the licence on an explicit refusal.
pub async fn revalidate_if_due(state: &LicenceState) {
    let stored = { state.0.lock().unwrap().clone() };
    let Some(licence) = stored else { return };
    let Ok(receipt) = verify(&licence.token) else {
        return;
    };
    if now() < receipt.refresh_after {
        return;
    }

    match call_worker(
        "/validate",
        serde_json::json!({
            "license_key": licence.license_key,
            "instance_id": receipt.instance_id,
        }),
    )
    .await
    {
        WorkerReply::Receipt(token) => {
            if verify(&token).is_ok() {
                let refreshed = StoredLicence { token, ..licence };
                let _ = save_to_keychain(&refreshed);
                *state.0.lock().unwrap() = Some(refreshed);
            }
        }
        // Refunded, revoked, chargeback: the only case that takes the licence away.
        WorkerReply::Refused { .. } => {
            clear_keychain();
            *state.0.lock().unwrap() = None;
        }
        // Offline, DNS down, Worker asleep: change nothing, try again next launch.
        WorkerReply::Unreachable => {}
    }
}

/// Gate used by the actions that move files. Scanning stays free.
pub fn is_active(state: &LicenceState) -> bool {
    state
        .0
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|l| verify(&l.token).ok())
        .map(|r| r.status == "active")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixture signed by the Worker's own code path (Web Crypto Ed25519), proving the
    /// two halves agree on the format.
    /// Public half of the throwaway pair used to sign the fixture below. Deliberately
    /// not the shipped key: rotating that one must not invalidate these tests.
    const TEST_PUBLIC_KEY: &str = "8nCPoKYtXxL4uYqVcJNDZywNwwPkKcKJ4a5mxLHoV2Y=";

    const WORKER_TOKEN: &str = "eyJrZXlfaGFzaCI6ImFiYzEyMyIsImluc3RhbmNlX2lkIjoiaW5zdC0xIiwic3RhdHVzIjoiYWN0aXZlIiwicHJvZHVjdF9pZCI6NDIsInJlZnJlc2hfYWZ0ZXIiOjE4MDAwMDAwMDAsImlzc3VlZF9hdCI6MTcwMDAwMDAwMCwiYWN0aXZhdGlvbl9saW1pdCI6MywiYWN0aXZhdGlvbl91c2FnZSI6MX0.An7cmj6QTkkOzSg1RylYwdVide-cWuXirdM1eAjGJ28mntOL7_zm2I8zqctTbz67IxFZ5WMDY4FLTpKz5bUeBQ";

    #[test]
    fn accepts_a_receipt_the_worker_signed() {
        let receipt =
            verify_with(WORKER_TOKEN, TEST_PUBLIC_KEY).expect("worker receipt must verify");
        assert_eq!(receipt.status, "active");
        assert_eq!(receipt.instance_id, "inst-1");
        assert_eq!(receipt.activation_limit, 3);
    }

    #[test]
    fn rejects_a_receipt_someone_edited() {
        let (payload, signature) = WORKER_TOKEN.split_once('.').unwrap();
        let mut json: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
        json["activation_limit"] = serde_json::json!(999);
        let forged = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json).unwrap()),
            signature
        );
        assert!(
            verify_with(&forged, TEST_PUBLIC_KEY).is_err(),
            "edited payload must not verify"
        );
    }

    #[test]
    fn rejects_a_home_made_receipt() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"key_hash":"x","instance_id":"x","status":"active","refresh_after":9999999999,"issued_at":0}"#);
        let fake = format!("{}.{}", payload, URL_SAFE_NO_PAD.encode([0u8; 64]));
        assert!(
            verify_with(&fake, TEST_PUBLIC_KEY).is_err(),
            "unsigned receipt must not verify"
        );
    }

    /// Guards the wiring itself: if the shipped key were ever left as the test one,
    /// anybody holding that leaked private half could mint licences.
    #[test]
    fn the_shipped_key_is_not_the_test_key() {
        assert_ne!(SIGNING_PUBLIC_KEY, TEST_PUBLIC_KEY);
        assert!(
            verify(WORKER_TOKEN).is_err(),
            "a receipt signed by the test pair must not verify in production"
        );
    }

    #[test]
    fn version_comparison_survives_double_digits() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(
            is_newer("0.10.0", "0.9.0"),
            "string ordering would fail here"
        );
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.0"), "same version is not an update");
        assert!(!is_newer("0.1.0", "0.2.0"), "older must never be announced");
        assert!(!is_newer("", "0.1.0"));
    }

    #[test]
    fn a_receipt_without_a_signature_is_not_a_receipt() {
        assert!(verify_with("no-dot-here", TEST_PUBLIC_KEY).is_err());
    }
}
