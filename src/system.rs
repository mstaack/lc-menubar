//! macOS integration: Keychain token storage, notifications, dialogs, opening URLs.
//! Strings reach AppleScript only via `argv`, never by interpolation.

use anyhow::{Result, bail};
use std::process::Command;

const SERVICE: &str = "LC Menubar";
/// Keychain services of earlier names, newest first.
const OLD_SERVICES: [&str; 2] = ["Cirrus", "cloud-bar"];
/// Keychain account used by versions with a single token.
const LEGACY_ACCOUNT: &str = "laravel-cloud-api-token";

fn entry(account: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(SERVICE, account)?)
}

fn old_entry(service: &str, account: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(service, account)?)
}

fn key(account_id: &str) -> String {
    format!("org:{account_id}")
}

/// `LARAVEL_CLOUD_TOKEN`, if set: an extra, unsaved account.
pub fn env_token() -> Option<String> {
    std::env::var("LARAVEL_CLOUD_TOKEN")
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

pub fn load_token(account_id: &str) -> Option<String> {
    if let Some(t) = entry(&key(account_id))
        .ok()?
        .get_password()
        .ok()
        .filter(|t| !t.is_empty())
    {
        return Some(t);
    }
    // Move a token saved under an earlier name.
    for service in OLD_SERVICES {
        let Ok(old) = old_entry(service, &key(account_id)) else {
            continue;
        };
        if let Some(t) = old.get_password().ok().filter(|t| !t.is_empty()) {
            if save_token(account_id, &t).is_ok() {
                let _ = old.delete_credential();
            }
            return Some(t);
        }
    }
    None
}

pub fn save_token(account_id: &str, token: &str) -> Result<()> {
    entry(&key(account_id))?.set_password(token)?;
    Ok(())
}

pub fn delete_token(account_id: &str) -> Result<()> {
    match entry(&key(account_id))?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Move a single-token setup to an account entry; returns the token if migrated.
pub fn migrate_legacy_token(account_id: &str) -> Option<String> {
    let legacy = old_entry("cloud-bar", LEGACY_ACCOUNT).ok()?;
    let token = legacy.get_password().ok().filter(|t| !t.is_empty())?;
    save_token(account_id, &token).ok()?;
    let _ = legacy.delete_credential();
    Some(token)
}

fn osascript(script: &str, args: &[&str]) -> Result<Option<String>> {
    let out = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .args(args)
        .output()?;
    if out.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim_end().to_string(),
        ))
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        // -128 = user pressed Cancel
        if err.contains("-128") {
            Ok(None)
        } else {
            bail!("osascript: {}", err.trim())
        }
    }
}

pub fn notify(title: &str, body: &str) {
    let script = r#"on run argv
display notification (item 2 of argv) with title (item 1 of argv) sound name "default"
end run"#;
    if let Err(e) = osascript(script, &[title, body]) {
        eprintln!("notification failed: {e}");
    }
}

pub fn open_url(url: &str) {
    let _ = Command::new("open").arg(url).spawn();
}
