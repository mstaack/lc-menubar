//! Self-update from GitHub Releases.
//!
//! The latest release is checked through the GitHub API. Its zip asset holds
//! `LC Menubar.app`; a `.sha256` asset next to it must match the download. When the
//! running app is code signed, the new bundle must pass `codesign --verify` and
//! carry the same Team ID. The bundle is then swapped in place, so the login
//! item (which points into the bundle) keeps working.

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

/// `owner/name` of the GitHub repository releases come from.
pub const REPO: &str = "mstaack/lc-menubar";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub page: String,
    pub zip_url: Option<String>,
    pub sha256_url: Option<String>,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

fn repo() -> String {
    std::env::var("LCM_UPDATE_REPO").unwrap_or_else(|_| REPO.into())
}

fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(crate::api::user_agent())
        .timeout(std::time::Duration::from_secs(120))
        .build()?)
}

/// The newest published release, if it is newer than this build.
pub async fn check() -> Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", repo());
    let resp = http()?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None); // no release published yet
    }
    let release: GhRelease = resp.error_for_status()?.json().await?;
    Ok(parse_release(release).filter(|r| is_newer(&r.version, CURRENT)))
}

fn parse_release(r: GhRelease) -> Option<Release> {
    if r.draft || r.prerelease {
        return None;
    }
    let asset = |pred: &dyn Fn(&str) -> bool| {
        r.assets
            .iter()
            .find(|a| pred(&a.name))
            .map(|a| a.browser_download_url.clone())
    };
    Some(Release {
        version: r.tag_name.trim_start_matches('v').to_string(),
        notes: r.body.unwrap_or_default().trim().to_string(),
        page: r.html_url,
        zip_url: asset(&|n| n.ends_with(".zip") && n.contains("macos")),
        sha256_url: asset(&|n| n.ends_with(".zip.sha256") && n.contains("macos")),
    })
}

/// `1.2.3` style comparison; a pre-release (`1.2.3-beta.1`) sorts before its release.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<(Vec<u64>, bool)> {
        let v = v.trim().trim_start_matches('v');
        let (core, pre) = match v.split_once('-') {
            Some((c, _)) => (c, true),
            None => (v, false),
        };
        let nums = core
            .split('.')
            .map(|p| p.parse().ok())
            .collect::<Option<Vec<u64>>>()?;
        Some((nums, pre))
    }
    let (Some((a, a_pre)), Some((b, b_pre))) = (parse(candidate), parse(current)) else {
        return false;
    };
    let len = a.len().max(b.len());
    let pad = |v: &[u64]| {
        (0..len)
            .map(|i| v.get(i).copied().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    match pad(&a).cmp(&pad(&b)) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => b_pre && !a_pre,
    }
}

/// The `.app` bundle this binary runs from, if any.
pub fn bundle_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    // …/LC Menubar.app/Contents/MacOS/lc-menubar
    let bundle = exe.parent()?.parent()?.parent()?;
    (bundle.extension()? == "app").then(|| bundle.to_path_buf())
}

/// Whether this copy can replace itself (runs from a bundle in a writable folder).
pub fn can_install() -> bool {
    bundle_path()
        .and_then(|b| b.parent().map(Path::to_path_buf))
        .is_some_and(|dir| writable(&dir))
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".lc-menubar-write-test-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Downloads, verifies and swaps in the release. Returns the installed bundle.
pub async fn install(release: &Release) -> Result<PathBuf> {
    let bundle = bundle_path().context("LC Menubar is not running from an app bundle")?;
    let zip_url = release
        .zip_url
        .as_deref()
        .context("The release has no macOS download")?;
    let sha_url = release
        .sha256_url
        .as_deref()
        .context("The release has no checksum")?;
    let client = http()?;
    let zip = client
        .get(zip_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let sha_text = client
        .get(sha_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    verify_sha256(&zip, &sha_text)?;

    let work = std::env::temp_dir().join(format!("lc-menubar-update-{}", release.version));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let zip_path = work.join("update.zip");
    std::fs::write(&zip_path, &zip)?;
    run(
        "ditto",
        &["-x", "-k", path_str(&zip_path)?, path_str(&work)?],
    )?;
    let new = std::fs::read_dir(&work)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .context("The download holds no app")?;
    verify_signature(&bundle, &new)?;
    swap(&bundle, &new)?;
    let _ = std::fs::remove_dir_all(&work);
    Ok(bundle)
}

fn verify_sha256(data: &[u8], sha_file: &str) -> Result<()> {
    let expected = sha_file
        .split_whitespace()
        .next()
        .context("Empty checksum file")?
        .to_ascii_lowercase();
    let actual: String = Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if expected != actual {
        bail!("Checksum mismatch, the download was not installed");
    }
    Ok(())
}

/// Team ID of a signed bundle, `None` if unsigned or ad-hoc signed.
fn team_id(bundle: &Path) -> Option<String> {
    let out = Command::new("codesign")
        .args(["-dv", "--verbose=2"])
        .arg(bundle)
        .output()
        .ok()?;
    // codesign prints details on stderr.
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .find_map(|l| l.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|t| !t.is_empty() && *t != "not set")
        .map(String::from)
}

fn verify_signature(current: &Path, new: &Path) -> Result<()> {
    let Some(team) = team_id(current) else {
        // Unsigned development build: the checksum is the only check.
        return Ok(());
    };
    run(
        "codesign",
        &["--verify", "--deep", "--strict", path_str(new)?],
    )
    .map_err(|_| anyhow!("The update's signature is invalid"))?;
    if team_id(new).as_deref() != Some(team.as_str()) {
        bail!("The update is signed by a different developer");
    }
    Ok(())
}

/// Replaces `bundle` with `new`, restoring the old bundle if the move fails.
fn swap(bundle: &Path, new: &Path) -> Result<()> {
    let backup = bundle.with_extension("app.old");
    let _ = std::fs::remove_dir_all(&backup);
    std::fs::rename(bundle, &backup).context("Could not move the current app aside")?;
    // `ditto` copies across volumes (the temp dir may be on another one).
    if let Err(e) = run("ditto", &[path_str(new)?, path_str(bundle)?]) {
        let _ = std::fs::remove_dir_all(bundle);
        let _ = std::fs::rename(&backup, bundle);
        return Err(e.context("Could not install the update"));
    }
    let _ = std::fs::remove_dir_all(&backup);
    // Downloads through this app carry no quarantine flag, but be explicit.
    let _ = Command::new("xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(bundle)
        .status();
    Ok(())
}

/// Starts the installed bundle once this process has exited.
pub fn relaunch(bundle: &Path) {
    let script = format!(
        "while kill -0 {} 2>/dev/null; do sleep 0.2; done; open \"$0\"",
        std::process::id()
    );
    let _ = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg(bundle)
        .spawn();
}

fn run(cmd: &str, args: &[&str]) -> Result<()> {
    let out = Command::new(cmd).args(args).output()?;
    if !out.status.success() {
        bail!(
            "{cmd} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn path_str(p: &Path) -> Result<&str> {
    p.to_str().context("Non-UTF-8 path")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v0.1.1", "0.1.0"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(is_newer("0.2.0", "0.2.0-beta.1"));
        assert!(!is_newer("0.2.0-beta.1", "0.2.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }

    fn gh(tag: &str, draft: bool, pre: bool) -> GhRelease {
        GhRelease {
            tag_name: tag.into(),
            body: Some("  Notes\n".into()),
            html_url: "https://example.test/r".into(),
            draft,
            prerelease: pre,
            assets: vec![
                GhAsset {
                    name: "lc-menubar-0.2.0-macos-universal.zip".into(),
                    browser_download_url: "https://example.test/z".into(),
                },
                GhAsset {
                    name: "lc-menubar-0.2.0-macos-universal.zip.sha256".into(),
                    browser_download_url: "https://example.test/s".into(),
                },
            ],
        }
    }

    #[test]
    fn parses_release_assets() {
        let r = parse_release(gh("v0.2.0", false, false)).unwrap();
        assert_eq!(r.version, "0.2.0");
        assert_eq!(r.notes, "Notes");
        assert_eq!(r.zip_url.as_deref(), Some("https://example.test/z"));
        assert_eq!(r.sha256_url.as_deref(), Some("https://example.test/s"));
        assert!(parse_release(gh("v0.2.0", true, false)).is_none());
        assert!(parse_release(gh("v0.2.0", false, true)).is_none());
    }

    #[test]
    fn checks_sha256() {
        // sha256("abc")
        let sum = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  x.zip\n";
        assert!(verify_sha256(b"abc", sum).is_ok());
        assert!(verify_sha256(b"abd", sum).is_err());
        assert!(verify_sha256(b"abc", "").is_err());
    }
}
