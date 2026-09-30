//! Anonymous, read-only update checks. Downloads are opened only on a user click.
use anyhow::{Context, Result, bail};
use semver::Version;
use serde::Deserialize;
use std::{sync::mpsc, thread, time::Duration};

pub const LOCAL_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Deserialize)]
struct Feed {
    version: String,
    repository: String,
    branch: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub version: String,
    pub download_url: String,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
    state: String,
}

pub fn asset_name(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("slicer-macos-aarch64.dmg"),
        ("macos", "x86_64") => Some("slicer-macos-x86_64.dmg"),
        ("linux", "x86_64") => Some("slicer-linux-x86_64.tar.gz"),
        ("linux", "aarch64") => Some("slicer-linux-aarch64.tar.gz"),
        _ => None,
    }
}

fn newer_version(local: &str, remote: &str) -> Result<Option<Version>> {
    let local = Version::parse(local)?;
    let remote = Version::parse(remote)?;
    Ok((remote.pre.is_empty() && remote.cmp_precedence(&local).is_gt()).then_some(remote))
}

fn select_update(
    local: &str,
    feed: &Feed,
    release: Release,
    asset_name: &str,
) -> Result<Option<AvailableUpdate>> {
    let Some(version) = newer_version(local, &feed.version)? else {
        return Ok(None);
    };
    let tag_version = Version::parse(
        release
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&release.tag_name),
    )?;
    // Main may already contain the next version, or the release may still be building.
    if release.draft || release.prerelease || tag_version != version {
        return Ok(None);
    }
    let Some(asset) = release
        .assets
        .into_iter()
        .find(|a| a.name == asset_name && a.size > 0 && a.state == "uploaded")
    else {
        return Ok(None);
    };
    let expected = format!(
        "https://github.com/{}/releases/download/{}/{}",
        feed.repository, release.tag_name, asset_name
    );
    if asset.browser_download_url != expected {
        bail!("release asset URL does not match this project's release");
    }
    Ok(Some(AvailableUpdate {
        version: version.to_string(),
        download_url: expected,
    }))
}

fn get(agent: &ureq::Agent, url: &str, limit: u64) -> Result<String> {
    let mut response = agent
        .get(url)
        .header("User-Agent", &format!("Slicer/{LOCAL_VERSION}"))
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("update request failed")?;
    Ok(response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_string()?)
}

pub fn check() -> Result<Option<AvailableUpdate>> {
    let Some(asset) = asset_name(std::env::consts::OS, std::env::consts::ARCH) else {
        return Ok(None);
    };
    let config: Feed = serde_json::from_str(include_str!("../version.json"))?;
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .https_only(true)
        .build()
        .new_agent();
    let raw = format!(
        "https://raw.githubusercontent.com/{}/{}/version.json",
        config.repository, config.branch
    );
    let feed: Feed = serde_json::from_str(&get(&agent, &raw, 16_384)?)?;
    if feed.repository != config.repository || feed.branch != config.branch {
        bail!("update feed changed repositories");
    }
    if newer_version(LOCAL_VERSION, &feed.version)?.is_none() {
        return Ok(None);
    }
    let url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        config.repository
    );
    let release = serde_json::from_str(&get(&agent, &url, 2_000_000)?)?;
    select_update(LOCAL_VERSION, &feed, release, asset)
}

/// Once per app launch. Offline/404/rate-limit failures never interrupt editing.
pub fn spawn_check() -> mpsc::Receiver<Option<AvailableUpdate>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(check().ok().flatten());
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    fn feed() -> Feed {
        Feed {
            version: "0.10.0".into(),
            repository: "xptea/slicer-rs".into(),
            branch: "main".into(),
        }
    }
    fn release() -> Release {
        Release { tag_name: "v0.10.0".into(), draft: false, prerelease: false, assets: vec![Asset { name: "slicer-macos-aarch64.dmg".into(), size: 123, state: "uploaded".into(), browser_download_url: "https://github.com/xptea/slicer-rs/releases/download/v0.10.0/slicer-macos-aarch64.dmg".into() }] }
    }
    #[test]
    fn compares_semantic_versions() {
        assert!(newer_version("0.9.0", "0.10.0").unwrap().is_some());
        for remote in ["0.9.0", "0.8.0", "1.0.0-beta.1", "0.9.0+build"] {
            assert!(newer_version("0.9.0", remote).unwrap().is_none());
        }
        assert!(newer_version("0.9.0", "bad").is_err());
    }
    #[test]
    fn selects_exact_platform_asset() {
        assert!(
            select_update("0.9.0", &feed(), release(), "slicer-macos-aarch64.dmg")
                .unwrap()
                .is_some()
        );
        assert!(
            select_update("0.9.0", &feed(), release(), "slicer-macos-x86_64.dmg")
                .unwrap()
                .is_none()
        );
        assert!(
            select_update("0.10.0", &feed(), release(), "slicer-macos-aarch64.dmg")
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn waits_for_published_uploaded_matching_release() {
        for case in 0..5 {
            let mut r = release();
            match case {
                0 => r.tag_name = "v0.9.0".into(),
                1 => r.draft = true,
                2 => r.prerelease = true,
                3 => r.assets[0].size = 0,
                _ => r.assets[0].state = "new".into(),
            }
            assert!(
                select_update("0.9.0", &feed(), r, "slicer-macos-aarch64.dmg")
                    .unwrap()
                    .is_none()
            );
        }
    }
    #[test]
    fn rejects_unrelated_download_urls() {
        let mut r = release();
        r.assets[0].browser_download_url = "https://example.com/app.dmg".into();
        assert!(select_update("0.9.0", &feed(), r, "slicer-macos-aarch64.dmg").is_err());
    }
    #[test]
    fn supported_platforms_match_release_names() {
        assert_eq!(
            asset_name("macos", "aarch64"),
            Some("slicer-macos-aarch64.dmg")
        );
        assert_eq!(
            asset_name("macos", "x86_64"),
            Some("slicer-macos-x86_64.dmg")
        );
        assert_eq!(
            asset_name("linux", "aarch64"),
            Some("slicer-linux-aarch64.tar.gz")
        );
        assert_eq!(
            asset_name("linux", "x86_64"),
            Some("slicer-linux-x86_64.tar.gz")
        );
        assert_eq!(asset_name("windows", "x86_64"), None);
    }
    #[test]
    fn checked_in_version_matches_binary() {
        let f: Feed = serde_json::from_str(include_str!("../version.json")).unwrap();
        assert_eq!(f.version, LOCAL_VERSION);
    }
}
