//! Where a tool's config lives, and whether it was actually updated. Returned
//! (rather than logged in place) so `login` can tell the developer exactly
//! which files it touched -- silently editing someone's dotfiles is worse
//! than not editing them.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Written(PathBuf),
    /// The tool isn't installed here (its config directory doesn't exist).
    /// Not an error: most developers have one of the two, not both.
    Skipped(PathBuf),
    /// The developer passed this client's `--no-…` flag. Distinct from
    /// `Skipped` because the two are different facts about their machine, and
    /// one line of output that conflates them is a line nobody can act on:
    /// "not present" is a tool to install, "left alone" is a choice they made.
    Declined {
        path: PathBuf,
        flag: &'static str,
    },
}

impl Outcome {
    /// Prints one line per outcome, and reports whether Codex's `config.toml`
    /// was among the files written.
    ///
    /// Every outcome gets a line: silently editing someone's dotfiles is worse
    /// than not editing them. The three read differently on purpose --
    /// `Configured:` is a file that changed, `Skipped:` is a tool they could
    /// install, `Left alone:` is a choice they made and nothing to act on.
    ///
    /// The return value is that narrow on purpose. Codex is the ONLY client
    /// without a dynamic-headers hook: Claude Code refreshes through
    /// `otelHeadersHelper`, and VS Code Copilot no longer exports for itself at
    /// all -- it writes a file that `copilot push` ships with a bearer it
    /// refreshes. So the missing-credential warning its caller prints is about
    /// exactly one file, and naming the others would be crying wolf.
    pub fn report(outcomes: &[Self]) -> bool {
        let mut wrote_codex_config = false;
        for outcome in outcomes {
            match outcome {
                Self::Written(path) => {
                    eprintln!("Configured: {}", path.display());
                    wrote_codex_config |=
                        path.file_name().is_some_and(|name| name == "config.toml");
                }
                Self::Skipped(dir) => eprintln!("Skipped: {} not present.", dir.display()),
                Self::Declined { path, flag } => {
                    eprintln!("Left alone ({flag}): {}", path.display());
                }
            }
        }
        wrote_codex_config
    }
}
