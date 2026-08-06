//! `ozzel update`: checks the `main` branch's `Cargo.toml` version on GitHub
//! and, if it's newer (or `--force`), reinstalls via `cargo install --git`.
//! Runs entirely before any terminal setup (see `main.rs`'s `main`), so it's
//! plain stdout/stderr the whole way — never touches the TUI machinery at
//! all.

use std::process::Command;

use anyhow::bail;

/// Repository URL used by both `cargo install --git` (the actual update
/// mechanism) and the raw-content fetch that checks the remote version
/// first — see `self_update`.
const REPO_URL: &str = "https://github.com/m-tkg/Ozzel";

/// How long `fetch_remote_version`'s `curl` invocation is allowed to hang
/// before it's treated the same as any other failure (see
/// `fetch_remote_version`'s doc comment) — passed straight through as
/// curl's own `--max-time` rather than wrapped in, say, a `wait_timeout`
/// loop, since curl already does exactly this job itself.
const FETCH_TIMEOUT_SECS: u64 = 10;

/// Fetches `Cargo.toml`'s `package.version` from the `main` branch on
/// GitHub, `None` on any failure (curl missing, network error, non-200,
/// unparsable TOML, missing field) — deliberately not `Result`: every
/// caller (`self_update`) treats "couldn't check" as just another reason to
/// proceed with the install rather than a distinct error to report, most
/// notably before the GitHub repository has even been made public (a 404,
/// in practice).
///
/// Shells out to `curl` (`-sf --max-time 10 <url>`) rather than linking an
/// HTTP client crate: this is the *only* place in ozzel that ever makes a
/// network request, and it's a one-shot, best-effort GET with no response
/// body processing beyond a TOML parse — not enough to justify pulling in
/// an HTTP stack (and, transitively, a TLS stack) for the whole binary. `-s`
/// (silent, no progress meter) plus `-f` (fail — exit nonzero on a non-2xx
/// HTTP status instead of printing the error body to stdout) together are
/// what make a 404 (pre-launch) or any other HTTP-level failure collapse
/// into the same `None` as a DNS failure or `curl` not being installed at
/// all.
pub fn fetch_remote_version() -> Option<String> {
    let url = "https://raw.githubusercontent.com/m-tkg/Ozzel/main/Cargo.toml";
    let output = Command::new("curl")
        .args(["-sf", "--max-time", &FETCH_TIMEOUT_SECS.to_string(), url])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let body = String::from_utf8(output.stdout).ok()?;
    let parsed: toml::Value = toml::from_str(&body).ok()?;
    Some(parsed.get("package")?.get("version")?.as_str()?.to_string())
}

/// What `self_update` should do given the remote version check's outcome —
/// pulled out as a pure enum/fn pair (rather than left inline as match arms
/// returning early) so the equal/newer/unknown × `--force` decision matrix
/// is directly unit-testable without a network call.
#[derive(Debug, PartialEq, Eq)]
pub enum UpdateDecision {
    /// Remote version matches the running binary's and `--force` wasn't
    /// given — nothing to do.
    AlreadyLatest,
    /// Proceed with `cargo install --git`: either the remote is a
    /// different version, `--force` was given, or the remote version
    /// couldn't be determined at all (missing repo, network error, ...).
    Install,
}

pub fn decide_update(current: &str, remote: Option<&str>, force: bool) -> UpdateDecision {
    match remote {
        Some(remote) if remote == current && !force => UpdateDecision::AlreadyLatest,
        _ => UpdateDecision::Install,
    }
}

/// Updates ozzel in place via `cargo install --git`, mirroring the sibling
/// `llmeter` project's `self_update`.
pub fn self_update(force: bool) -> anyhow::Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    println!("current version: {current}");

    let remote = fetch_remote_version();
    match decide_update(current, remote.as_deref(), force) {
        UpdateDecision::AlreadyLatest => {
            let remote = remote.expect("AlreadyLatest implies a known remote version");
            println!("already up to date ({remote}). Use --force to reinstall anyway.");
            return Ok(());
        }
        UpdateDecision::Install => match &remote {
            Some(remote) => println!("remote version: {remote}. Updating."),
            None => println!("could not determine the remote version. Reinstalling anyway."),
        },
    }

    let status = Command::new("cargo")
        .args(["install", "--git", REPO_URL, "--force"])
        .status();
    match status {
        Ok(s) if s.success() => println!("update complete."),
        Ok(s) => bail!("cargo install failed (exit: {s})"),
        Err(e) => bail!("could not run cargo: {e} (cargo must be installed)"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- `ozzel update` version-compare decision -----------------------

    #[test]
    fn decide_update_same_version_without_force_is_already_latest() {
        assert_eq!(
            decide_update("1.2.3", Some("1.2.3"), false),
            UpdateDecision::AlreadyLatest
        );
    }

    #[test]
    fn decide_update_same_version_with_force_installs_anyway() {
        assert_eq!(
            decide_update("1.2.3", Some("1.2.3"), true),
            UpdateDecision::Install
        );
    }

    #[test]
    fn decide_update_newer_remote_version_installs() {
        assert_eq!(
            decide_update("1.2.3", Some("1.3.0"), false),
            UpdateDecision::Install
        );
    }

    #[test]
    fn decide_update_newer_remote_version_installs_with_force_too() {
        assert_eq!(
            decide_update("1.2.3", Some("1.3.0"), true),
            UpdateDecision::Install
        );
    }

    #[test]
    fn decide_update_unknown_remote_version_always_installs() {
        // Most notably: the GitHub repo not existing/being public yet, per
        // `fetch_remote_version`'s doc comment — a failed version check is
        // never a reason to refuse to proceed.
        assert_eq!(decide_update("1.2.3", None, false), UpdateDecision::Install);
        assert_eq!(decide_update("1.2.3", None, true), UpdateDecision::Install);
    }
}
