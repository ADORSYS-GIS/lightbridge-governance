use super::*;

/// The release workflow's own header says `asset_name` "must stay in
/// lockstep with the matrix", and until this test nothing enforced it --
/// a drift shows up only as "no asset for your platform" on a developer's
/// machine, long after the release.
#[test]
fn every_asset_name_exists_in_the_release_workflow_matrix() {
    let workflow = include_str!("../../../../.github/workflows/release-governance-auth.yml");
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
    let workflow = include_str!("../../../../.github/workflows/release-governance-auth.yml");
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
    let this_module = include_str!("mod.rs");
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
    let cli_rs = include_str!("../cli/mod.rs");
    assert!(
        cli_rs.contains("version = update::VERSION"),
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
    let error =
        verify_checksum(b"hello", file.as_bytes()).expect_err("a mismatched checksum must refuse");
    assert!(format!("{error:#}").contains("checksum mismatch"));
}

#[test]
fn an_empty_checksum_file_refuses_rather_than_passing_vacuously() {
    // An empty or truncated .sha256 must not read as "nothing to check".
    assert!(verify_checksum(b"hello", b"").is_err());
    assert!(verify_checksum(b"hello", b"   \n").is_err());
}
