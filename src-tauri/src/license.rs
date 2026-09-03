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
use tauri::{AppHandle, Manager, State};

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

/// What this machine already is, as far as the licence server is concerned.
///
/// Kept in a plain file rather than in the keychain, and that separation is the whole
/// point. The keychain holds the receipt, which is a secret. These two are not: an
/// instance id is an opaque handle Lemon Squeezy issued and already knows.
///
/// A keychain item is bound to the application's code signature, so changing the
/// signing certificate loses it — and so does a reinstall, a new user account, or a
/// keychain reset. When that happened the app had no way to recognise a machine it had
/// already activated, so re-entering the key called `/activate` and Lemon Squeezy
/// issued a *new* instance, consuming one of the three seats. Every time. Three such
/// events and someone who paid is locked out of their own licence, on one computer.
///
/// Keeping the identity outside the keychain means a lost receipt costs the user one
/// retyped key and nothing else.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct DeviceIdentity {
    #[serde(default)]
    device_id: String,
    /// The instance Lemon Squeezy issued for this machine, if it ever issued one.
    #[serde(default)]
    instance_id: String,
}

fn identity_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("device.json"))
}

fn load_identity(app: &AppHandle) -> DeviceIdentity {
    identity_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_identity(app: &AppHandle, identity: &DeviceIdentity) {
    if let Some(path) = identity_path(app) {
        if let Ok(raw) = serde_json::to_string(identity) {
            let _ = std::fs::write(path, raw);
        }
    }
}

fn clear_identity(app: &AppHandle) {
    if let Some(path) = identity_path(app) {
        let _ = std::fs::remove_file(path);
    }
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
    let stored = state.0.lock().unwrap_or_else(|e| e.into_inner());
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
    app: AppHandle,
    state: State<'_, LicenceState>,
    key: String,
) -> Result<LicenceInfo, String> {
    let key = key.trim().to_string();
    if key.is_empty() {
        return Ok(LicenceInfo::inactive(Some("empty_key".into())));
    }

    /* If this machine already holds a seat, take it back rather than a second one.
       `/validate` confirms an existing instance without consuming an activation;
       `/activate` always issues a new one. Anything that lost the receipt — a new
       signing certificate, a reinstall, a keychain reset — used to land here and quietly
       spend a seat the user had already paid for. */
    let known = load_identity(&app);
    if !known.instance_id.is_empty() {
        if let WorkerReply::Receipt(token) = call_worker(
            "/validate",
            serde_json::json!({
                "license_key": key,
                "instance_id": known.instance_id,
            }),
        )
        .await
        {
            if let Ok(receipt) = verify(&token) {
                let licence = StoredLicence {
                    token,
                    license_key: key,
                    device_id: known.device_id.clone(),
                };
                save_to_keychain(&licence)?;
                *state.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(licence);
                return Ok(info_from(&receipt));
            }
        }
        /* Falling through is correct and expected: a different key, or an instance the
           server no longer recognises, cannot be revalidated and must be activated. */
    }

    // Random, and deliberately not derived from the hardware — it says nothing about
    // the machine. Persisted below so it is generated once and not once per activation.
    let device_id = if known.device_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        known.device_id
    };

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
            // Remembered outside the keychain, so the next activation can reclaim this
            // seat instead of buying another.
            save_identity(
                &app,
                &DeviceIdentity {
                    device_id: licence.device_id.clone(),
                    instance_id: receipt.instance_id.clone(),
                },
            );
            *state.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(licence);
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
pub async fn deactivate_licence(
    app: AppHandle,
    state: State<'_, LicenceState>,
) -> Result<LicenceInfo, String> {
    let stored = { state.0.lock().unwrap_or_else(|e| e.into_inner()).clone() };
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
    // The seat has been given back, so the instance behind it is gone: keeping the id
    // would make the next activation try to revalidate something that no longer exists.
    clear_identity(&app);
    *state.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
    Ok(LicenceInfo::inactive(None))
}

/// Background refresh, run once at startup. Silent by design: it never interrupts,
/// and it only ever gives up the licence on an explicit refusal.
pub async fn revalidate_if_due(app: &AppHandle, state: &LicenceState) {
    let stored = { state.0.lock().unwrap_or_else(|e| e.into_inner()).clone() };
    let Some(licence) = stored else { return };
    let Ok(receipt) = verify(&licence.token) else {
        return;
    };

    /* Anyone who activated before the identity file existed has a valid seat and no
       record of it, so the first keychain loss would still cost them one. Writing it
       here, from a receipt that has just been verified, repairs those installations
       silently on the next launch. Before the refresh check on purpose: the repair is
       needed whether or not the receipt is due. */
    if load_identity(app).instance_id.is_empty() {
        save_identity(
            app,
            &DeviceIdentity {
                device_id: licence.device_id.clone(),
                instance_id: receipt.instance_id.clone(),
            },
        );
    }

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
                *state.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(refreshed);
            }
        }
        // Refunded, revoked, chargeback: the only case that takes the licence away.
        WorkerReply::Refused { .. } => {
            clear_keychain();
            *state.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
