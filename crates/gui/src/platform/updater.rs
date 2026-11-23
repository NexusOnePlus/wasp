//! Checking for and applying a new nightly build.
//!
//! The app is published from one rolling release, tagged `nightly`, whose
//! title names the commit it was built from. So "is there an update?" is
//! not a version comparison: it is "is the
//! published commit a different one from mine?".
//!
//! Two things are deliberate about how an update is applied. The installer is
//! downloaded beside its checksum file and refused unless the SHA-256 matches
//! what that file says, because the nightly is unsigned and a truncated or
//! rewritten download would otherwise be run without question. And the
//! installer is started with `--restart`, so the window that began the
//! update is replaced by the one it installed rather than leaving the
//! person to find the shortcut again.
//!
//! Only a desktop build can apply an update: a page has nowhere to put an
//! installer. `check` is shared because reading a release is the same work on
//! both, and `install` is the one answer that differs.

use std::sync::Arc;

use gpui::http_client::{AsyncBody, HttpClient, Request};
use sha2::{Digest as _, Sha256};

/// The release the app follows. A tagged release would need a version
/// comparison; the nightly already carries the exact commit.
pub const CHANNEL: &str = "nightly";

/// The installer asset the release publishes for this platform.
const INSTALLER_ASSET: &str = "Wasp-Setup-nightly-x64.exe";
const CHECKSUMS_ASSET: &str = "checksums.sha256";

/// A release answer is a few kilobytes; anything larger is not one.
const MAX_RELEASE_BYTES: usize = 256 * 1024;
/// The checksum file is one line per asset.
const MAX_CHECKSUMS_BYTES: usize = 16 * 1024;
/// The installer, with room to grow. A nightly is tens of megabytes.
const MAX_INSTALLER_BYTES: usize = 200 * 1024 * 1024;

/// A nightly that is a different build from this one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    /// The commit the release was built from, abbreviated.
    pub revision: String,
    pub installer_url: String,
    pub checksums_url: String,
}

/// The commit this window was built from, when the build knew one.
pub fn current_revision() -> Option<&'static str> {
    option_env!("WASP_REV").filter(|rev| !rev.is_empty())
}

/// Whether this build can install a release at all.
///
/// Only the installed desktop copy can: the binaries in a build directory are
/// the tree's, and replacing them would break the tree while claiming to have
/// updated it.
pub fn supported() -> bool {
    imp::installed()
}

/// Ask the release page whether it holds a different build.
///
/// `Ok(None)` is "this is the published build": not an error, and
/// not a reason to draw anything alarming.
pub async fn check(client: &Arc<dyn HttpClient>) -> Result<Option<Update>, String> {
    let bytes = get(
        client,
        &release_url(),
        "application/vnd.github+json",
        MAX_RELEASE_BYTES,
    )
    .await?;
    let release: Release = serde_json::from_slice(&bytes)
        .map_err(|error| format!("the release page did not read as JSON: {error}"))?;
    let revision = release
        .revision()
        .ok_or_else(|| "the nightly release did not name a commit".to_string())?;
    if !is_newer(current_revision(), &revision) {
        return Ok(None);
    }
    let installer_url = release
        .asset(INSTALLER_ASSET)
        .ok_or_else(|| format!("the nightly release has no {INSTALLER_ASSET}"))?;
    let checksums_url = release
        .asset(CHECKSUMS_ASSET)
        .ok_or_else(|| format!("the nightly release has no {CHECKSUMS_ASSET}"))?;
    Ok(Some(Update {
        revision,
        installer_url,
        checksums_url,
    }))
}

/// Download the installer and refuse it unless its checksum is the published
/// one, then start it.
pub async fn install(client: &Arc<dyn HttpClient>, update: &Update) -> Result<(), String> {
    let sums = get(
        client,
        &update.checksums_url,
        "text/plain",
        MAX_CHECKSUMS_BYTES,
    )
    .await?;
    let sums = String::from_utf8_lossy(&sums);
    let expected = checksum_for(&sums, INSTALLER_ASSET).ok_or_else(|| {
        format!("{CHECKSUMS_ASSET} does not name {INSTALLER_ASSET}; refusing to run it")
    })?;
    let bytes = get(
        client,
        &update.installer_url,
        "application/octet-stream",
        MAX_INSTALLER_BYTES,
    )
    .await?;
    let actual = sha256_hex(&bytes);
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(format!(
            "the installer did not match its checksum (expected {expected}, got {actual})"
        ));
    }
    imp::launch(&bytes, &update.revision)
}

/// `name (revision)` is how the nightly workflow titles a release.
fn revision_from_title(title: &str) -> Option<String> {
    let open = title.rfind('(')?;
    let rest = &title[open + 1..];
    let close = rest.find(')')?;
    let revision = rest[..close].trim();
    (revision.len() >= 7
        && revision.len() <= 40
        && revision.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then(|| revision.to_ascii_lowercase())
}

/// Whether `release` names a commit other than `current`.
///
/// Git abbreviates to a prefix, and the release title is abbreviated too, so
/// equality is decided by one being a prefix of the other rather than by
/// comparing the strings.
pub fn is_newer(current: Option<&str>, release: &str) -> bool {
    let Some(current) = current.filter(|current| !current.is_empty()) else {
        return false;
    };
    let current = current.to_ascii_lowercase();
    let release = release.to_ascii_lowercase();
    !(current.starts_with(&release) || release.starts_with(&current))
}

/// The checksum a `sha256sum`-style file records for `name`.
fn checksum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, listed) = line.split_once(char::is_whitespace)?;
        let listed = listed.trim().trim_start_matches('*');
        (listed == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest.as_slice() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[derive(serde::Deserialize)]
struct Release {
    name: Option<String>,
    #[serde(default)]
    assets: Vec<Asset>,
}

impl Release {
    fn revision(&self) -> Option<String> {
        self.name.as_deref().and_then(revision_from_title)
    }

    fn asset(&self, name: &str) -> Option<String> {
        self.assets
            .iter()
            .find(|asset| asset.name == name)
            .map(|asset| asset.browser_download_url.clone())
    }
}

#[derive(serde::Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// The API address for this repository's nightly release.
fn release_url() -> String {
    let repository = env!("CARGO_PKG_REPOSITORY");
    format!("{repository}/releases/tags/{CHANNEL}")
        .replace("https://github.com/", "https://api.github.com/repos/")
}

async fn get(
    client: &Arc<dyn HttpClient>,
    url: &str,
    accept: &str,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let request = Request::get(url)
        .header("User-Agent", "Wasp")
        .header("Accept", accept)
        .body(AsyncBody::empty())
        .map_err(|error| format!("could not build the request: {error}"))?;
    let response = client
        .send(request)
        .await
        .map_err(|error| format!("could not reach {url}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} answered {}", response.status()));
    }
    let body = response.into_body();
    let mut bytes = Vec::new();
    use futures_lite::AsyncReadExt as _;
    body.take(limit as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| format!("could not read {url}: {error}"))?;
    Ok(bytes)
}

// The desktop half: write the verified installer somewhere and start it.
#[cfg(not(target_family = "wasm"))]
mod imp {
    use std::path::{Path, PathBuf};

    /// Whether this executable is the installed copy, which is the one an
    /// update may replace.
    ///
    /// The window started from a build directory says no: its binaries are
    /// the tree's, and replacing them would break the tree while claiming to
    /// have updated it.
    pub(super) fn installed() -> bool {
        let Some(exe) = std::env::current_exe().ok() else {
            return false;
        };
        #[cfg(windows)]
        {
            let Some(dir) = exe.parent() else {
                return false;
            };
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .map(|root| root.join("Programs").join("Wasp"))
                .is_some_and(|expected| same_path(dir, &expected))
        }
        #[cfg(not(windows))]
        {
            let _ = exe;
            false
        }
    }

    fn same_path(left: &Path, right: &Path) -> bool {
        let (Ok(left), Ok(right)) = (left.canonicalize(), right.canonicalize()) else {
            return false;
        };
        left == right
    }

    /// Write the verified bytes and start the installer.
    pub(super) fn launch(bytes: &[u8], revision: &str) -> Result<(), String> {
        #[cfg(windows)]
        {
            use std::io::Write as _;
            use std::os::windows::process::CommandExt as _;
            use std::process::Command;

            let directory = std::env::temp_dir().join("wasp-update");
            std::fs::create_dir_all(&directory)
                .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
            let installer = directory.join(format!("Wasp-Setup-{revision}.exe"));
            let mut file = std::fs::File::create(&installer)
                .map_err(|error| format!("could not write {}: {error}", installer.display()))?;
            file.write_all(bytes)
                .map_err(|error| format!("could not write {}: {error}", installer.display()))?;
            drop(file);
            // CREATE_NO_WINDOW: the updater has no console to give it.
            Command::new(&installer)
                .arg("--quiet")
                .arg("--restart")
                .creation_flags(0x0800_0000)
                .spawn()
                .map_err(|error| format!("could not start the installer: {error}"))?;
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = (bytes, revision);
            Err("nightly builds are published for Windows only".to_string())
        }
    }
}

// The page half: there is no installer to run and nowhere to put one.
#[cfg(target_family = "wasm")]
mod imp {
    pub(super) fn installed() -> bool {
        false
    }

    pub(super) fn launch(_bytes: &[u8], _revision: &str) -> Result<(), String> {
        Err("an update can only be installed by the desktop app".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CHECKSUMS_ASSET, INSTALLER_ASSET, checksum_for, is_newer, release_url, revision_from_title,
    };

    #[test]
    fn the_nightly_title_names_its_commit() {
        assert_eq!(
            revision_from_title("Nightly (abc1234)").as_deref(),
            Some("abc1234")
        );
        assert_eq!(
            revision_from_title("Nightly (ABC1234)").as_deref(),
            Some("abc1234")
        );
        assert_eq!(revision_from_title("Nightly"), None);
        assert_eq!(revision_from_title("Nightly (main)"), None);
        assert_eq!(revision_from_title("Nightly (abc)"), None);
    }

    #[test]
    fn a_different_commit_is_an_update() {
        assert!(is_newer(Some("abcdef1"), "1234567"));
        assert!(!is_newer(Some("abcdef1"), "abcdef1"));
        // One is an abbreviation of the other, so they are one build.
        assert!(!is_newer(Some("abcdef1"), "abcdef"));
        assert!(!is_newer(Some("abcdef"), "abcdef1"));
        // A build with no commit of its own is never offered one.
        assert!(!is_newer(None, "abcdef1"));
    }

    #[test]
    fn a_checksum_file_names_each_asset() {
        let sums = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  wasp-nightly-windows-x86_64.zip
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  *Wasp-Setup-nightly-x64.exe
";
        assert_eq!(
            checksum_for(sums, INSTALLER_ASSET).as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
        assert_eq!(checksum_for(sums, "missing.exe"), None);
        assert_eq!(
            checksum_for(
                "not a checksum  Wasp-Setup-nightly-x64.exe\n",
                INSTALLER_ASSET
            ),
            None
        );
        assert!(
            checksum_for(sums, CHECKSUMS_ASSET).is_none(),
            "the list does not name itself"
        );
    }

    #[test]
    fn the_release_address_is_the_repositorys_api() {
        assert_eq!(
            release_url(),
            "https://api.github.com/repos/NexusOnePlus/wasp/releases/tags/nightly"
        );
    }
}
