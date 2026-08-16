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

/// The version this binary claims to be, and the ONLY thing `self-update`
/// compares a release tag against.
///
/// ⚠️ Why this is not simply `CARGO_PKG_VERSION`. release-please governs this
/// repo's releases with `release-type: simple`, which bumps
/// `.release-please-manifest.json` and `Chart.yaml` but **not** `Cargo.toml`
/// (its `extra-files` never lists one, and the Rust strategy cannot be used
/// here -- its updater rejects a virtual workspace manifest outright). So the
/// tag moves to `v0.2.0` while `[workspace.package] version` stays `0.1.0`.
///
/// That shipped: release `v0.2.0` contains binaries whose `CARGO_PKG_VERSION`
/// is `0.1.0`. `self-update` then sees `0.2.0 > 0.1.0`, reinstalls, and on the
/// next invocation sees the identical mismatch -- an unbounded reinstall loop
/// that no amount of retrying escapes, because the newly-installed binary is
/// just as stale as the one it replaced.
///
/// The release workflow therefore injects the tag it is building at
/// `.github/workflows/release-governance-auth.yml`, and a released binary
/// reports that. A locally-built one falls back to `CARGO_PKG_VERSION` and so
/// reads as older than any release, which is the correct bias: a developer
/// build offering to update is harmless, whereas a released build that cannot
/// recognise itself loops forever.
///
/// No build script is needed to make this correct across cached CI builds, and
/// one was written and then deleted rather than kept "just in case". rustc
/// records every variable an `env!`/`option_env!` touches in the unit's
/// dep-info, and cargo fingerprints it -- measured here rather than assumed:
///
/// ```text
/// # env-dep:GOVERNANCE_AUTH_RELEASE_VERSION=v2.2.2
/// ```
///
/// With no `build.rs` present at all, changing the variable rebuilds and the
/// reported version follows, so a warm `Swatinem/rust-cache` restore cannot
/// bake a previous release's tag into a new one.
pub const VERSION: &str = match option_env!("GOVERNANCE_AUTH_RELEASE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// GitHub rejects API requests without one.
///
/// Built at runtime rather than via `concat!`, which only accepts literals and
/// so would pin this to `CARGO_PKG_VERSION` -- reintroducing, in the one string
/// GitHub actually logs, exactly the stale-version claim [`VERSION`] exists to
/// eliminate. Three short-lived allocations per `self-update` run, on a path
/// that is already making network requests.
fn user_agent() -> String {
    format!("governance-auth/{VERSION}")
}

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
    // ⚠️ `target_env` is load-bearing on Linux, not cosmetic. The release
    // publishes BOTH `-musl` and `-gnu` assets per arch; without this branch
    // a musl-built binary would ask for the `-gnu` asset and self-update
    // itself onto a build that cannot start on the very distro it is running
    // on (that glibc floor is the whole reason the musl assets exist).
    if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "musl"
    )) {
        "governance-auth-x86_64-unknown-linux-musl"
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "aarch64",
        target_env = "musl"
    )) {
        "governance-auth-aarch64-unknown-linux-musl"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
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
    // Normalised on BOTH sides. The injected value is whatever the workflow
    // was triggered with, which is a tag (`v0.2.0`) rather than a bare version,
    // and `is_newer` parses digits -- so an un-normalised `v0.2.0` would parse
    // its first component as 0 and make a released binary read as older than
    // itself, restoring the loop through a different door.
    let current = normalise_version(VERSION);

    let response = http
        .get(RELEASES_API)
        .header(reqwest::header::USER_AGENT, user_agent())
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
        .header(reqwest::header::USER_AGENT, user_agent())
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

    /// The release workflow's own header says `asset_name` "must stay in
    /// lockstep with the matrix", and until this test nothing enforced it --
    /// a drift shows up only as "no asset for your platform" on a developer's
    /// machine, long after the release.
    #[test]
    fn every_asset_name_exists_in_the_release_workflow_matrix() {
        let workflow = include_str!("../../../.github/workflows/release-governance-auth.yml");
        for target in [
            "x86_64-unknown-linux-musl",
            "aarch64-unknown-linux-musl",
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "x86_64-apple-darwin",
            "aarch64-apple-darwin",
        ] {
            assert!(
                workflow.contains(&format!("target: {target}")),
                "asset_name() can return governance-auth-{target}, but the release \
                 workflow builds no such target -- self-update would report \"no asset \
                 for your platform\""
            );
        }
        // The reverse direction: a target built but never requested is dead
        // weight in the release, and usually means asset_name() was missed.
        assert!(
            !workflow.contains("target: x86_64-unknown-linux-musleabi"),
            "sanity check that the matcher above is not vacuous"
        );
    }

    #[test]
    fn musl_and_gnu_asset_names_are_distinct() {
        // Guards the branch order in `asset_name`: `target_env = "musl"` must
        // be tested BEFORE the bare linux arms, or musl falls through to the
        // gnu name and self-update installs a binary that cannot start.
        let name = asset_name();
        if cfg!(all(target_os = "linux", target_env = "musl")) {
            assert!(
                name.ends_with("-musl"),
                "musl build must want a musl asset, got {name}"
            );
        } else if cfg!(target_os = "linux") {
            assert!(
                name.ends_with("-gnu"),
                "glibc build must want a gnu asset, got {name}"
            );
        }
    }

    /// The bug that shipped in v0.2.0, pinned as an executable statement so it
    /// cannot quietly return: a binary that reports a version older than the
    /// latest tag asks to update, and -- because reinstalling does not change
    /// what it reports -- asks again, forever.
    #[test]
    fn a_binary_that_misreports_its_version_never_stops_updating() {
        let tag = normalise_version("v0.2.0");

        // What v0.2.0 actually shipped: CARGO_PKG_VERSION frozen at 0.1.0.
        assert!(
            is_newer(tag, "0.1.0"),
            "the stale-version binary asks to update ..."
        );
        // ... and installing it changes nothing, because the replacement
        // reports 0.1.0 too. Same inputs, same answer, no termination.
        assert!(
            is_newer(tag, "0.1.0"),
            "... and asks again after installing, which is the loop"
        );

        // The fix terminates it: a binary reporting its own release tag.
        assert!(
            !is_newer(tag, normalise_version("v0.2.0")),
            "a binary that knows its own version must stop"
        );
    }

    /// Guards the `v`-stripping on the INJECTED side specifically. The workflow
    /// injects a tag, not a bare version, and `is_newer` parses digits -- so
    /// skipping normalisation here parses `v0` as 0 and makes a released binary
    /// read as older than itself, which is the loop again by another route.
    #[test]
    fn an_injected_tag_is_normalised_before_comparison() {
        assert!(
            !is_newer(normalise_version("v0.2.0"), normalise_version("v0.2.0")),
            "tag-shaped VERSION must compare equal to the same tag"
        );
        // Sanity check that the assertion above is not vacuous -- and note the
        // version deliberately starts at 1, not 0. `is_newer` parses `"v1"` as
        // 0, so on a 0.x line the un-normalised bug is INVISIBLE (0 == 0) and
        // only starts biting the day this repo cuts 1.0.0. A regression here
        // would therefore lie dormant across every 0.x release and surface at
        // the worst possible moment, which is exactly why it is pinned.
        assert!(
            is_newer(normalise_version("v1.2.0"), "v1.2.0"),
            "sanity: skipping normalisation on the current side parses `v1` as \
             0, so a released binary reads as older than itself"
        );
    }

    /// `option_env!` resolves at compile time, and this test binary is built
    /// without the variable set, so `VERSION` must be the crate version here.
    /// Also pins the fallback direction: unset means "developer build", never
    /// empty.
    #[test]
    fn version_falls_back_to_the_crate_version_when_nothing_is_injected() {
        assert!(!VERSION.is_empty(), "VERSION must never be empty");
        if option_env!("GOVERNANCE_AUTH_RELEASE_VERSION").is_none() {
            assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
        }
    }

    /// The injection is split across three files that cannot see each other:
    /// `option_env!` here, the `env:` block in the release workflow, and the
    /// `rerun-if-env-changed` in `build.rs`. Remove any one and the binary goes
    /// back to misreporting its version -- silently, and only on a real
    /// release, which is the worst place to find out. These two tests fail if
    /// either of the other two files loses its half.
    #[test]
    fn the_release_workflow_injects_the_release_version() {
        let workflow = include_str!("../../../.github/workflows/release-governance-auth.yml");
        assert!(
            workflow.contains("GOVERNANCE_AUTH_RELEASE_VERSION:"),
            "the release workflow no longer sets GOVERNANCE_AUTH_RELEASE_VERSION, so released \
             binaries would report the stale workspace version and self-update would loop"
        );
        assert!(
            workflow.contains("tag_name"),
            "the injected value must come from the release tag, not a literal"
        );
    }

    /// The two consumer sites. `run` reading `CARGO_PKG_VERSION` directly, or
    /// clap's `version` going back to bare `version`, both restore the bug
    /// while every behavioural test above still passes -- because those test
    /// `is_newer` in isolation and never observe which value gets fed in.
    /// Asserted against the source because the alternative is a live HTTP
    /// round-trip through `run` for a one-line invariant.
    #[test]
    fn the_crate_version_is_read_in_exactly_one_place() {
        let this_module = include_str!("update.rs");
        // Only the shipping half. The tests below legitimately mention the
        // macro (the fallback assertion uses it, and this needle is spelled
        // out), and counting those would make the guard permanently wrong.
        let shipping = this_module
            .split_once("#[cfg(test)]")
            .map_or(this_module, |(before, _)| before);
        // Split so this needle does not match itself in the file it scans.
        let needle = concat!("env!(\"CARGO_PKG_", "VERSION\")");
        let direct_reads = shipping.matches(needle).count();
        assert_eq!(
            direct_reads, 1,
            "`CARGO_PKG_VERSION` must be read ONLY as VERSION's fallback; another read means some \
             path compares against the stale workspace version again (found {direct_reads})"
        );
    }

    #[test]
    fn the_cli_reports_the_same_version_self_update_acts_on() {
        let main_rs = include_str!("main.rs");
        assert!(
            main_rs.contains("version = update::VERSION"),
            "`--version` must come from update::VERSION; bare `version` wires clap to \
             CARGO_PKG_VERSION, so a released binary would print a version that disagrees with \
             the one self-update compares -- and `--version` is what people run to check"
        );
    }

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
