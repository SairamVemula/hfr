//! Finding, downloading and installing a newer version of the app.
//!
//! HTTPS goes through the system `curl` (the client has no TLS library of its own). A release is only trusted when
//! its `SHA256SUMS` file carries a valid ed25519 signature from one of the public keys built into this program and
//! the installer's SHA-256 is listed there. Only `api.github.com` / `github.com` are contacted and the only thing
//! sent is a `User-Agent: hfr/<version>` header.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{command, data_dir, Cancelled, Phase, Transfer};

pub const REPO: &str = "SairamVemula/hfr";
pub const RELEASES_PAGE: &str = "https://github.com/SairamVemula/hfr/releases";
/// Version of this build.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Public keys (hex, 32 bytes) whose signatures are accepted. Several are allowed so a key can be rotated: ship the
/// new one in release N, sign release N+1 with it.
pub const PUBLIC_KEYS: &[&str] = &["f62de5fd8a8a7c9f426671608fa03375e86b4a49665720cd2ff09201d310a207"];

/// Do not ask GitHub more often than this, however many times the app or the CLI starts.
const MIN_INTERVAL_SECS: u64 = 3600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// Without the leading `v`.
    pub version: String,
    pub notes: String,
    pub page: String,
    pub assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

/// Reads the GitHub "latest release" answer. Drafts and pre-releases are not offered.
pub fn parse_release(json: &str) -> Result<Option<Release>> {
    let r: RawRelease = serde_json::from_str(json).context("unexpected answer from the release server")?;
    if r.draft || r.prerelease {
        return Ok(None);
    }
    let version = r.tag_name.trim_start_matches('v').to_string();
    semver::Version::parse(&version).with_context(|| format!("release tag `{}` is not a version", r.tag_name))?;
    Ok(Some(Release {
        version,
        notes: r.body.unwrap_or_default(),
        page: if r.html_url.is_empty() { RELEASES_PAGE.to_string() } else { r.html_url },
        assets: r.assets.into_iter().map(|a| Asset { name: a.name, url: a.browser_download_url, size: a.size }).collect(),
    }))
}

/// True when `latest` is a higher version than `current`.
pub fn is_newer(current: &str, latest: &str) -> bool {
    match (semver::Version::parse(current.trim_start_matches('v')), semver::Version::parse(latest.trim_start_matches('v'))) {
        (Ok(c), Ok(l)) => l > c,
        _ => false,
    }
}

/// The installer for this platform, by file name.
pub fn pick_installer<'a>(r: &'a Release, os: &str, arch: &str) -> Option<&'a Asset> {
    let lower = |a: &Asset| a.name.to_ascii_lowercase();
    let arm = arch == "aarch64" || arch == "arm64";
    match os {
        "windows" => r.assets.iter().find(|a| lower(a).ends_with("-setup.exe") || lower(a).ends_with("_setup.exe")),
        "macos" => {
            let want = |a: &&Asset| {
                let n = lower(a);
                n.ends_with(".dmg") && (n.contains("aarch64") || n.contains("arm64")) == arm
            };
            r.assets.iter().find(want).or_else(|| r.assets.iter().find(|a| lower(a).ends_with(".dmg")))
        }
        "linux" => {
            r.assets.iter().find(|a| lower(a).ends_with(".appimage")).or_else(|| r.assets.iter().find(|a| lower(a).ends_with(".deb")))
        }
        _ => None,
    }
}

/// `sha256sum` format: `<hex>  <name>` (a `*` before the name marks binary mode).
pub fn parse_sums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (hash, name) = l.trim().split_once(char::is_whitespace)?;
            let name = name.trim().trim_start_matches('*');
            (hash.len() == 64 && !name.is_empty()).then(|| (name.to_string(), hash.to_ascii_lowercase()))
        })
        .collect()
}

fn from_hex(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        bail!("not hexadecimal");
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| anyhow!("not hexadecimal"))).collect()
}

pub fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Checks the detached signature (hex) over `sums` against any of `keys` (hex public keys).
pub fn verify_signature(sums: &[u8], sig_hex: &str, keys: &[&str]) -> Result<()> {
    if keys.is_empty() {
        bail!("this build has no update signing key, so updates cannot be verified");
    }
    let sig_bytes: [u8; 64] = from_hex(sig_hex)?.try_into().map_err(|_| anyhow!("the signature has the wrong length"))?;
    let sig = Signature::from_bytes(&sig_bytes);
    for k in keys {
        let Ok(kb) = from_hex(k) else { continue };
        let Ok(kb): Result<[u8; 32], _> = kb.try_into() else { continue };
        let Ok(vk) = VerifyingKey::from_bytes(&kb) else { continue };
        if vk.verify(sums, &sig).is_ok() {
            return Ok(());
        }
    }
    bail!("the signature of the release does not match a trusted key")
}

/// Keys to trust. Debug builds may be pointed at a test key with `HFR_UPDATE_PUBKEY`.
fn trusted_keys() -> Vec<String> {
    let mut keys: Vec<String> = PUBLIC_KEYS.iter().map(|k| k.to_string()).collect();
    if cfg!(debug_assertions) {
        if let Ok(k) = std::env::var("HFR_UPDATE_PUBKEY") {
            keys.push(k);
        }
    }
    keys
}

fn api_url() -> String {
    if cfg!(debug_assertions) {
        if let Ok(u) = std::env::var("HFR_UPDATE_URL") {
            return u;
        }
    }
    format!("https://api.github.com/repos/{REPO}/releases/latest")
}

fn curl_missing(e: std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        anyhow!("`curl` was not found, so updates cannot be checked. Download the new version from {RELEASES_PAGE}")
    } else {
        anyhow!(e)
    }
}

/// GET `url` with the system curl.
pub async fn fetch(url: &str) -> Result<Vec<u8>> {
    let out = command("curl")
        .args(["-fsSL", "-m", "30", "-A", &format!("hfr/{CURRENT_VERSION}"), "-H", "Accept: application/vnd.github+json", url])
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(curl_missing)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("could not reach the release server: {}", err.trim().lines().last().unwrap_or("request failed"));
    }
    Ok(out.stdout)
}

/// Asks the release server for the latest release; `Some` only when it is newer than `current`.
pub async fn check(current: &str) -> Result<Option<Release>> {
    let body = fetch(&api_url()).await?;
    let rel = parse_release(&String::from_utf8_lossy(&body))?;
    Ok(rel.filter(|r| is_newer(current, &r.version)))
}

// ---- remembered state ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    /// Automatic checks (the user can turn them off; "Check now" still works).
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub last_check_unix: u64,
    /// The newer release found by the last check, if any.
    #[serde(default)]
    pub latest: Option<Release>,
    /// The user chose "Skip this version".
    #[serde(default)]
    pub skipped: Option<String>,
}

fn yes() -> bool {
    true
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl State {
    fn path() -> PathBuf {
        data_dir().join("update.json")
    }

    pub fn load() -> State {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(State { enabled: true, ..Default::default() })
    }

    pub fn save(&self) {
        let p = Self::path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, s);
        }
    }

    /// Time for another automatic check?
    pub fn due(&self, now: u64) -> bool {
        self.enabled && now.saturating_sub(self.last_check_unix) >= MIN_INTERVAL_SECS
    }

    /// The release to tell the user about: newer than `current` and not skipped.
    pub fn offer(&self, current: &str) -> Option<&Release> {
        self.latest.as_ref().filter(|r| is_newer(current, &r.version) && self.skipped.as_deref() != Some(r.version.as_str()))
    }

    /// Runs a check now and remembers the answer.
    pub async fn refresh(&mut self, current: &str) -> Result<()> {
        let res = check(current).await;
        self.last_check_unix = now_unix();
        if let Ok(r) = &res {
            self.latest = r.clone();
        }
        self.save();
        res.map(|_| ())
    }
}

// ---- download, verify, install ------------------------------------------------------------------------------------

fn update_dir() -> PathBuf {
    std::env::temp_dir().join("hfr-update")
}

fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(to_hex(&h.finalize()))
}

fn asset<'a>(r: &'a Release, name: &str) -> Result<&'a Asset> {
    r.assets.iter().find(|a| a.name == name).with_context(|| format!("the release has no `{name}` (it is not fully published yet)"))
}

/// Downloads the installer for this platform and checks it. Returns the verified file. `t` shows the progress.
pub async fn download_verified(r: &Release, t: &Arc<Transfer>) -> Result<PathBuf> {
    let keys = trusted_keys();
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    let installer = pick_installer(r, std::env::consts::OS, std::env::consts::ARCH)
        .with_context(|| format!("version {} has no installer for this computer; see {}", r.version, r.page))?;
    // The signed list of checksums first: nothing big is downloaded for a release that cannot be verified.
    t.begin(Phase::Scanning, 0);
    let sums = fetch(&asset(r, "SHA256SUMS")?.url).await?;
    let sig = fetch(&asset(r, "SHA256SUMS.sig")?.url).await?;
    verify_signature(&sums, &String::from_utf8_lossy(&sig), &keys)?;
    let expected = parse_sums(&String::from_utf8_lossy(&sums))
        .remove(&installer.name)
        .with_context(|| format!("`{}` is not listed in the signed checksums", installer.name))?;

    let dir = update_dir();
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(&installer.name);
    t.begin_at(Phase::Downloading, installer.size, std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0));
    let mut child = command("curl")
        .args(["-fsSL", "--retry", "3", "-C", "-", "-A", &format!("hfr/{CURRENT_VERSION}"), "-o"])
        .arg(&file)
        .arg(&installer.url)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(curl_missing)?;
    let status = loop {
        tokio::select! {
            s = child.wait() => break s?,
            _ = t.cancelled() => {
                let _ = child.kill().await;
                bail!(Cancelled)
            }
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                let have = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
                t.begin_at(Phase::Downloading, installer.size, have);
            }
        }
    };
    if !status.success() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            use tokio::io::AsyncReadExt;
            let _ = e.read_to_string(&mut err).await;
        }
        bail!("the download failed: {}", err.trim().lines().last().unwrap_or("curl reported an error"));
    }

    t.begin(Phase::Verifying, std::fs::metadata(&file)?.len());
    let path = file.clone();
    let actual = tokio::task::spawn_blocking(move || sha256_file(&path)).await??;
    if actual != expected {
        let _ = std::fs::remove_file(&file);
        bail!("the downloaded installer does not match the signed checksum, so it was deleted");
    }
    t.set_phase(Phase::Done);
    Ok(file)
}

/// Starts the verified installer. On Windows it runs silently and starts the app again; the caller must exit
/// right after. Elsewhere the file is opened or revealed and the user finishes the install.
pub fn install(file: &Path) -> Result<String> {
    if cfg!(debug_assertions) && std::env::var_os("HFR_UPDATE_NO_INSTALL").is_some() {
        return Ok(format!("(test mode) would install {}", file.display()));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let app = std::env::current_exe().context("cannot find this program's path")?;
        // Wait for the installer, then start the app again. The old app must be closed by the caller.
        let line = format!("/C \"\"{}\" /S && start \"\" \"{}\"\"", file.display(), app.display());
        std::process::Command::new("cmd")
            .raw_arg(line)
            .creation_flags(0x0800_0000 | 0x0000_0008)
            .spawn()
            .context("could not start the installer")?;
        Ok("Installing the update; the app closes and opens again when it is done.".into())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(file).spawn().context("could not open the disk image")?;
        Ok("Opened the disk image. Drag HFR Remote to Applications, replacing the old one.".into())
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        use std::os::unix::fs::PermissionsExt;
        if file.extension().is_some_and(|e| e.eq_ignore_ascii_case("appimage")) {
            let _ = std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755));
        }
        let dir = file.parent().unwrap_or(Path::new("."));
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
        Ok(format!("The verified installer is at {}. Run it (or install the .deb) to finish.", file.display()))
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};

    use super::*;

    const JSON: &str = r#"{
      "tag_name": "v0.2.0", "draft": false, "prerelease": false, "html_url": "https://github.com/x/y/releases/tag/v0.2.0",
      "body": "- faster downloads",
      "assets": [
        {"name":"hfr-gui_0.2.0_x64-setup.exe","browser_download_url":"https://e/x.exe","size":100},
        {"name":"HFR.Remote_0.2.0_aarch64.dmg","browser_download_url":"https://e/a.dmg","size":90},
        {"name":"HFR.Remote_0.2.0_x64.dmg","browser_download_url":"https://e/x.dmg","size":91},
        {"name":"hfr-remote_0.2.0_x86_64.AppImage","browser_download_url":"https://e/x.AppImage","size":92},
        {"name":"SHA256SUMS","browser_download_url":"https://e/SHA256SUMS","size":10}
      ]}"#;

    #[test]
    fn parses_a_release_and_strips_the_v() {
        let r = parse_release(JSON).unwrap().unwrap();
        assert_eq!(r.version, "0.2.0");
        assert_eq!(r.notes, "- faster downloads");
        assert_eq!(r.assets.len(), 5);
    }

    #[test]
    fn drafts_prereleases_and_junk_are_not_offered() {
        assert!(parse_release(r#"{"tag_name":"v9.0.0","draft":true}"#).unwrap().is_none());
        assert!(parse_release(r#"{"tag_name":"v9.0.0","prerelease":true}"#).unwrap().is_none());
        assert!(parse_release("not json").is_err());
        assert!(parse_release(r#"{"tag_name":"nightly"}"#).is_err());
    }

    #[test]
    fn version_comparison() {
        assert!(is_newer("0.1.0", "0.2.0"));
        assert!(is_newer("0.1.9", "0.1.10"));
        assert!(is_newer("0.1.0", "v1.0.0"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.3.0", "0.2.0"));
        assert!(!is_newer("0.1.0", "garbage"));
        assert!(is_newer("0.2.0-rc.1", "0.2.0"));
    }

    #[test]
    fn picks_the_installer_for_each_platform() {
        let r = parse_release(JSON).unwrap().unwrap();
        assert_eq!(pick_installer(&r, "windows", "x86_64").unwrap().name, "hfr-gui_0.2.0_x64-setup.exe");
        assert_eq!(pick_installer(&r, "macos", "aarch64").unwrap().name, "HFR.Remote_0.2.0_aarch64.dmg");
        assert_eq!(pick_installer(&r, "macos", "x86_64").unwrap().name, "HFR.Remote_0.2.0_x64.dmg");
        assert_eq!(pick_installer(&r, "linux", "x86_64").unwrap().name, "hfr-remote_0.2.0_x86_64.AppImage");
        assert!(pick_installer(&r, "freebsd", "x86_64").is_none());
    }

    #[test]
    fn parses_sha256sum_output() {
        let h = "a".repeat(64);
        let m = parse_sums(&format!("{h}  one.exe\n{}  *two.dmg\nbroken line\n", "B".repeat(64)));
        assert_eq!(m["one.exe"], h);
        assert_eq!(m["two.dmg"], "b".repeat(64));
        assert_eq!(m.len(), 2);
    }

    fn key() -> (SigningKey, String) {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let pk = to_hex(sk.verifying_key().as_bytes());
        (sk, pk)
    }

    #[test]
    fn accepts_a_good_signature_and_rejects_everything_else() {
        let (sk, pk) = key();
        let sums = b"abc  file.exe\n";
        let sig = to_hex(&sk.sign(sums).to_bytes());
        verify_signature(sums, &sig, &[&pk]).unwrap();
        // tampered content
        assert!(verify_signature(b"abd  file.exe\n", &sig, &[&pk]).is_err());
        // another key
        let other = to_hex(SigningKey::from_bytes(&[9u8; 32]).verifying_key().as_bytes());
        assert!(verify_signature(sums, &sig, &[&other]).is_err());
        // rotation: any trusted key may match
        verify_signature(sums, &sig, &[&other, &pk]).unwrap();
        // garbage and wrong length
        assert!(verify_signature(sums, "zz", &[&pk]).is_err());
        assert!(verify_signature(sums, "abcd", &[&pk]).is_err());
        // no keys built in: never trusted
        assert!(verify_signature(sums, &sig, &[]).is_err());
    }

    #[test]
    fn state_offers_only_newer_unskipped_releases() {
        let r = parse_release(JSON).unwrap().unwrap();
        let mut s = State { enabled: true, latest: Some(r), ..Default::default() };
        assert_eq!(s.offer("0.1.0").unwrap().version, "0.2.0");
        assert!(s.offer("0.2.0").is_none());
        s.skipped = Some("0.2.0".into());
        assert!(s.offer("0.1.0").is_none());
    }

    #[test]
    fn checks_are_throttled_and_can_be_turned_off() {
        let mut s = State { enabled: true, last_check_unix: 1000, ..Default::default() };
        assert!(!s.due(1000 + 60));
        assert!(s.due(1000 + MIN_INTERVAL_SECS));
        s.enabled = false;
        assert!(!s.due(10_000_000));
    }

    #[test]
    fn hashes_a_file() {
        let p = std::env::temp_dir().join("hfr-update-test-hash.bin");
        std::fs::write(&p, b"hello").unwrap();
        assert_eq!(sha256_file(&p).unwrap(), "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
        let _ = std::fs::remove_file(p);
    }
}
