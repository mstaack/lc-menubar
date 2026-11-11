//! Persisted user settings and start-at-login (LaunchAgent) handling.

use crate::model::Period;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

pub type Shared = Arc<RwLock<Settings>>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub start_at_login: bool,
    /// Poll interval while nothing is deploying (seconds).
    pub refresh_secs: u64,
    pub notify_started: bool,
    pub notify_succeeded: bool,
    pub notify_failed: bool,
    pub notify_cancelled: bool,
    pub metrics_enabled: bool,
    pub metrics_period: Period,
    pub show_cpu_in_menu: bool,
    pub show_replicas_in_menu: bool,
    /// Environment ids that are hidden from the menu, light and notifications.
    pub hidden_envs: HashSet<String>,
    /// Application (project) ids hidden with all their environments, also ones
    /// added later.
    pub hidden_apps: HashSet<String>,
    pub show_spend: bool,
    pub show_spend_in_menu_bar: bool,
    /// Connected organizations; tokens live in the Keychain under each id.
    pub accounts: Vec<Account>,
    /// Saved commands offered in the command runner.
    pub favorite_commands: Vec<String>,
    /// Organization shown in the menu (account id); `None` shows all.
    pub active_account: Option<String>,
    /// Look for new releases on GitHub every few hours.
    pub check_updates: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub name: String,
}

impl Settings {
    pub fn is_hidden(
        &self,
        app: &crate::model::Application,
        env: &crate::model::Environment,
    ) -> bool {
        self.hidden_apps.contains(&app.id) || self.hidden_envs.contains(&env.id)
    }

    /// Ids of every hidden environment in `apps`: hidden by itself or by its project.
    pub fn hidden_in(&self, apps: &[crate::model::Application]) -> HashSet<String> {
        apps.iter()
            .flat_map(|a| a.environments.iter().map(move |e| (a, e)))
            .filter(|(a, e)| self.is_hidden(a, e))
            .map(|(_, e)| e.id.clone())
            .collect()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            start_at_login: false,
            refresh_secs: 60,
            notify_started: true,
            notify_succeeded: true,
            notify_failed: true,
            notify_cancelled: false,
            metrics_enabled: true,
            metrics_period: Period::H1,
            show_cpu_in_menu: true,
            show_replicas_in_menu: true,
            hidden_envs: HashSet::new(),
            hidden_apps: HashSet::new(),
            show_spend: true,
            show_spend_in_menu_bar: false,
            accounts: vec![],
            favorite_commands: vec![
                "php artisan about".into(),
                "php artisan migrate --force".into(),
                "php artisan optimize:clear".into(),
                "php artisan queue:restart".into(),
            ],
            active_account: None,
            check_updates: true,
        }
    }
}

impl Settings {
    pub fn sanitized(mut self) -> Self {
        self.refresh_secs = self.refresh_secs.clamp(15, 3600);
        self
    }
}

fn dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lc-menubar")
}

/// Settings folders of earlier names, newest first.
fn old_dirs() -> impl Iterator<Item = PathBuf> {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    ["cirrus", "cloud-bar"]
        .into_iter()
        .map(move |d| base.join(d))
}

fn path() -> PathBuf {
    dir().join("settings.json")
}

pub fn load() -> Settings {
    if !path().exists()
        && let Some(old) = old_dirs()
            .map(|d| d.join("settings.json"))
            .find(|p| p.exists())
        && std::fs::create_dir_all(dir()).is_ok()
    {
        let _ = std::fs::copy(&old, path());
    }
    std::fs::read(path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
        .unwrap_or_default()
        .sanitized()
}

pub fn save(s: &Settings) -> Result<()> {
    std::fs::create_dir_all(dir())?;
    let tmp = path().with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)?;
    std::fs::rename(&tmp, path())?;
    Ok(())
}

// ------------------------------------------------------------ start at login

const LABEL: &str = "io.github.mstaack.lc-menubar";
/// LaunchAgent labels of earlier names.
const OLD_LABELS: [&str; 2] = ["dev.cirrus.menubar", "dev.cloud-bar.agent"];

fn agent_path() -> Result<PathBuf> {
    agent_path_for(LABEL)
}

fn agent_path_for(label: &str) -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("no home directory")?
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist")))
}

/// Replace a login item from before the rename with one for this binary.
pub fn migrate_login_item() {
    let mut found = false;
    for label in OLD_LABELS {
        if let Ok(old) = agent_path_for(label)
            && old.exists()
        {
            let _ = std::fs::remove_file(&old);
            found = true;
        }
    }
    if found {
        let _ = apply_login_item(true);
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn plist(exe: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{}</string></array>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#,
        xml_escape(exe)
    )
}

/// Install or remove a per-user LaunchAgent that starts this binary at login.
/// It only takes effect from the next login, so the running instance is untouched.
pub fn apply_login_item(enabled: bool) -> Result<()> {
    let path = agent_path()?;
    if enabled {
        let exe = std::env::current_exe()?.canonicalize()?;
        if exe.to_string_lossy().contains("/AppTranslocation/") {
            anyhow::bail!(
                "Move LC Menubar to the Applications folder first: macOS runs it from a temporary location"
            );
        }
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, plist(&exe.to_string_lossy()))?;
    } else if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

pub fn login_item_installed() -> bool {
    agent_path().is_ok_and(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hiding_a_project_hides_all_its_environments() {
        use crate::model::{Application, Environment};
        let env = |id: &str| Environment {
            id: id.into(),
            name: id.into(),
            status: "running".into(),
            domain: None,
            latest: None,
            metrics: None,
        };
        let apps = vec![
            Application {
                id: "a1".into(),
                account: "x".into(),
                org: "Org".into(),
                name: "one".into(),
                environments: vec![env("e1"), env("e2")],
            },
            Application {
                id: "a2".into(),
                account: "x".into(),
                org: "Org".into(),
                name: "two".into(),
                environments: vec![env("e3"), env("e4")],
            },
        ];
        let s = Settings {
            hidden_apps: ["a1".to_string()].into(),
            hidden_envs: ["e4".to_string()].into(),
            ..Settings::default()
        };
        let mut hidden: Vec<_> = s.hidden_in(&apps).into_iter().collect();
        hidden.sort();
        assert_eq!(hidden, ["e1", "e2", "e4"]);
    }

    #[test]
    fn partial_json_falls_back_to_defaults() {
        let s: Settings =
            serde_json::from_str(r#"{"refresh_secs": 5, "metrics_period": "24h"}"#).unwrap();
        let s = s.sanitized();
        assert_eq!(s.refresh_secs, 15);
        assert_eq!(s.metrics_period, Period::H24);
        assert!(s.notify_failed);
    }

    #[test]
    fn plist_escapes_path() {
        let p = plist("/Users/a&b/lc-menubar");
        assert!(p.contains("<string>/Users/a&amp;b/lc-menubar</string>"));
        assert!(p.contains(LABEL));
    }
}
