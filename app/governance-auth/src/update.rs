//! Self-update from GitHub Releases, so a developer laptop doesn't drift
//! behind on a binary that holds credentials and writes their dotfiles.
//!
//! ## Trust model, stated plainly
//!
//! The SHA-256 is fetched from the same release as the binary, so it proves
//! the download wasn't **corrupted or truncated** in transit. It does NOT
//! prove the release is authentic: anyone who could replace the asset could
//! replace the checksum beside it. TLS to `api.github.com` plus GitHub's own
//! account controls are what actually establish authenticity today.
//!
//! That is weaker than this repo's container images, which are cosign-signed
//! (`.github/workflows/docker.yml`). Signing these binaries the same way is
//! the right next step; until then this is deliberately not described as
//! "verified", only as "checksummed".
//!
//! Assets are RAW BINARIES, not tarballs -- unpacking an archive would mean
//! adding `tar`+`flate2` to a security-adjacent binary for no benefit, since
//! there is exactly one file to ship per platform.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Where releases are published. Not configurable: pointing a self-updater
/// at an arbitrary host via a flag is a remote-code-execution primitive, and
/// a developer who wants a different build can install it by hand.
const RELEASES_API: &str =
    "https://api.github.com/repos/ADORSYS-GIS/lightbridge-governance/releases/latest";

/// GitHub rejects API requests without one.
const USER_AGENT: &str = concat!("governance-auth/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// The asset basename for the platform this binary was built for. Kept in
/// lockstep with the release workflow's build matrix -- a mismatch here shows
/// up as "no asset for your platform", not as a wrong binary being installed.
fn asset_name() -> &'static str {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "governance-auth-x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "governance-auth-aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "governance-auth-x86_64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "governance-auth-aarch64-apple-darwin"
    } else {
        ""
    }
}

/// Strips a leading `v`/`governance-auth-v` so a tag compares against
/// `CARGO_PKG_VERSION`. Tag shapes drift over a repo's life (release-please
/// uses component prefixes for a workspace), so this normalises rather than
/// assuming one.
fn normalise_version(tag: &str) -> &str {
    tag.trim_start_matches("governance-auth-")
        .trim_start_matches('v')
}

/// Dotted numeric ordering, longest-common-prefix then length. Deliberately
/// NOT a semver crate: this only has to answer "is the release newer than
/// me", and adding a dependency to a security-adjacent binary for one
/// comparison is a poor trade.
///
/// Anything non-numeric (a `-rc.1` suffix, a hash) compares as 0 for that
/// component, which makes a pre-release sort as equal-or-lower rather than
/// higher. That's the right bias here: the failure mode is "declines to
/// update", not "installs something unexpected".
fn is_newer(candidate: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.split(['.', '-', '+'])
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    }
    let (a, b) = (parts(candidate), parts(current));
    let len = a.len().max(b.len());
    for i in 0..len {
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

/// Refuses to replace a binary this process doesn't own.
///
/// ⚠️ FAIL CLOSED, same shape as `/internal/v1/resolve`: "I can't tell who
/// installed this" is UNKNOWN, and unknown takes the strict branch. A
/// self-update that fights a package manager is worse than no self-update --
/// it leaves the package database describing a file that no longer matches,
/// and on the next `brew upgrade` / `apt upgrade` the change is silently
/// reverted, so the developer is running a version neither tool agrees on.
///
/// This is the conservative half of the rule only: it detects the managed
/// prefixes we can name. The positive half -- an install receipt written by
/// the standalone installer, which would let this refuse by DEFAULT rather
/// than by pattern -- lands with that installer; see the packaging ADR.
fn ensure_replaceable(target: &Path) -> Result<()> {
    // Resolve symlinks first: Homebrew puts a symlink on PATH pointing into
    // the Cellar, and on Linux `current_exe()` already returns the resolved
    // target while macOS may return the invocation path. Canonicalising
    // makes both platforms agree before any prefix is matched.
    let resolved = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    let shown = resolved.display().to_string();

    let managed: &[(&str, &str)] = &[
        ("/Cellar/", "brew upgrade governance-auth"),
        ("/homebrew/", "brew upgrade governance-auth"),
        ("/linuxbrew/", "brew upgrade governance-auth"),
        ("/nix/store/", "nix profile upgrade governance-auth"),
        ("/.asdf/", "asdf install governance-auth latest"),
        ("/mise/", "mise up governance-auth"),
    ];
    for (marker, command) in managed {
        if shown.contains(marker) {
            bail!(
                "{shown} is managed by a package manager, so self-update refuses to overwrite \
                 it (doing so would leave the package database describing a file that no longer \
                 matches, and the next upgrade would silently revert it).\n\nUpdate it with:\n  \
                 {command}"
            );
        }
    }

    // Distro package territory. `/usr/local` is explicitly NOT here: that is
    // the documented location for a manual system-wide install, which this
    // binary may legitimately replace.
    if (shown.starts_with("/usr/") && !shown.starts_with("/usr/local/"))
        || shown.starts_with("/bin/")
        || shown.starts_with("/sbin/")
    {
        bail!(
            "{shown} looks like a distro-packaged path, so self-update refuses to overwrite it.\
             \n\nUpdate it with your package manager, e.g.:\n  \
             sudo apt-get install --only-upgrade governance-auth\n  \
             sudo dnf upgrade governance-auth"
        );
    }

    // Cheapest real check, and the one that catches every case the prefix
    // list doesn't name: can this process actually write the directory the
    // rename lands in?
    let dir = resolved
        .parent()
        .context("the running executable has no parent directory")?;
    let probe = dir.join(".governance-auth.write-probe");
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => bail!(
            "{} is not writable by this user ({error}), so self-update cannot replace {shown}.\
             \n\nEither re-run with the privileges that own it, or update it the way you \
             installed it.",
            dir.display()
        ),
    }
}

pub async fn run(http: &reqwest::Client, check_only: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");

    let response = http
        .get(RELEASES_API)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .context("querying the GitHub releases API")?;

    // `releases/latest` 404s when a repo has published none. That's an
    // ordinary state, not a failure -- reporting it as an HTTP error would
    // have a developer chasing a broken updater when the answer is simply
    // that nothing has been released yet.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        eprintln!(
            "No published release found for this repository, so there is nothing to update to \
             (running {current})."
        );
        return Ok(());
    }

    let release: Release = response
        .error_for_status()
        .context("the GitHub releases API returned an error status")?
        .json()
        .await
        .context("parsing the releases API response")?;

    let latest = normalise_version(&release.tag_name);
    // `>`, not `!=`. String inequality treats an OLDER release as an update
    // and installs it -- a real downgrade path, not a theoretical one:
    // `releases/latest` is the newest release of the whole REPO, and this
    // binary's version is the workspace version, so a release cut from a
    // branch, a re-tag, or any tag-shape drift can present a lower version
    // here. Comparing order means the worst case is "no update", never
    // "silently rolled back".
    if !is_newer(latest, current) {
        eprintln!("governance-auth {current} is already the latest release.");
        return Ok(());
    }
    eprintln!(
        "Update available: {current} -> {latest} ({}).",
        release.tag_name
    );
    if check_only {
        return Ok(());
    }

    // Refuse BEFORE downloading ~10MB. The rename at the end would fail
    // anyway on a root-owned or read-only path, but only after spending the
    // user's time and this repo's release bandwidth on a decision that was
    // knowable up front.
    let target = std::env::current_exe().context("locating the running executable")?;
    ensure_replaceable(&target)?;

    let wanted = asset_name();
    if wanted.is_empty() {
        bail!(
            "no prebuilt asset is published for this platform ({} {}); install manually",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
    }

    let binary = release
        .assets
        .iter()
        .find(|asset| asset.name == wanted)
        .with_context(|| format!("release {} has no asset named {wanted}", release.tag_name))?;
    let checksum = release
        .assets
        .iter()
        .find(|asset| asset.name == format!("{wanted}.sha256"))
        .with_context(|| {
            format!(
                "release {} has no {wanted}.sha256; refusing to install an unchecked binary",
                release.tag_name
            )
        })?;

    let bytes = download(http, &binary.browser_download_url).await?;
    let expected = download(http, &checksum.browser_download_url).await?;
    verify_checksum(&bytes, &expected)?;

    install(&target, &bytes)?;

    eprintln!("Updated to {latest}. Re-run any long-lived shell to pick it up.");
    Ok(())
}

async fn download(http: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    Ok(http
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .with_context(|| format!("downloading {url}"))?
        .error_for_status()
        .with_context(|| format!("{url} returned an error status"))?
        .bytes()
        .await
        .with_context(|| format!("reading the body of {url}"))?
        .to_vec())
}

/// The `.sha256` asset is `<hex>  <filename>` (sha256sum's own format), so
/// only the first field is the digest.
fn verify_checksum(bytes: &[u8], checksum_file: &[u8]) -> Result<()> {
    let text = String::from_utf8_lossy(checksum_file);
    let expected = text
        .split_whitespace()
        .next()
        .context("the .sha256 asset was empty")?
        .to_ascii_lowercase();

    let actual = hex::encode(Sha256::digest(bytes));
    if actual != expected {
        bail!("checksum mismatch: expected {expected}, got {actual}; refusing to install");
    }
    Ok(())
}

/// Writes next to the target and renames over it. Same-directory rename is
/// atomic on POSIX and works even while the old binary is running (the
/// running process keeps its open inode), so there is no window where the
/// path is missing or half-written -- which for a credential helper invoked
/// on a timer by two other tools would mean spurious auth failures.
fn install(target: &Path, bytes: &[u8]) -> Result<()> {
    let dir = target
        .parent()
        .context("the running executable has no parent directory")?;
    let staged: PathBuf = dir.join(".governance-auth.update");

    write_executable(&staged, bytes)
        .with_context(|| format!("staging the new binary at {}", staged.display()))?;

    fs::rename(&staged, target).with_context(|| {
        format!(
            "replacing {} (is it on a read-only mount, or owned by another user?)",
            target.display()
        )
    })?;
    Ok(())
}

#[cfg(unix)]
fn write_executable(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};

    // 0755, not 0600: this replaces an executable on $PATH.
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o755)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_executable(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_tags_normalise_across_the_shapes_a_repo_drifts_through() {
        assert_eq!(normalise_version("v0.1.0"), "0.1.0");
        assert_eq!(normalise_version("governance-auth-v0.1.0"), "0.1.0");
        assert_eq!(normalise_version("0.1.0"), "0.1.0");
    }

    #[test]
    fn a_matching_checksum_passes() {
        let bytes = b"hello";
        let digest = hex::encode(Sha256::digest(bytes));
        let file = format!("{digest}  governance-auth-x86_64-unknown-linux-gnu\n");
        assert!(verify_checksum(bytes, file.as_bytes()).is_ok());
    }

    #[test]
    fn a_mismatched_checksum_refuses_to_install() {
        // The whole point: a corrupted or truncated download must not be
        // written over a binary that holds credentials.
        let file = format!("{}  x\n", "0".repeat(64));
        let error = verify_checksum(b"hello", file.as_bytes())
            .expect_err("a mismatched checksum must refuse");
        assert!(format!("{error:#}").contains("checksum mismatch"));
    }

    #[test]
    fn an_empty_checksum_file_refuses_rather_than_passing_vacuously() {
        // An empty or truncated .sha256 must not read as "nothing to check".
        assert!(verify_checksum(b"hello", b"").is_err());
        assert!(verify_checksum(b"hello", b"   \n").is_err());
    }
}
