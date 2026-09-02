//! Help text is the product, not a side effect of documenting the source.
//!
//! clap derive takes a flag's `--help` from its Rust doc comment, so a note
//! written for the next maintainer is printed verbatim to whoever runs
//! `governance-auth copilot push --help`. That is how `--help` came to open
//! with a rustdoc link to a private module, a fully-qualified Rust path, and
//! three paragraphs on why a field is `Option` and not a clap default.
//! Nothing caught it, because everything compiled and every sentence was true.
//!
//! These two tests are the missing check. The rationale still exists -- it is
//! in `docs/governance-auth/configuration.md` and in plain `//` comments,
//! which clap cannot reach.

use clap::CommandFactory;

use crate::cli::Cli;

#[test]
fn no_help_text_is_addressed_to_a_maintainer() {
    // Substrings that have no business in front of a user. `crate::` and the
    // rustdoc link opener catch Rust paths; the rest are the phrasings the
    // old field docs actually used.
    const BANNED: [&str; 6] = [
        "crate::",
        "[`",
        "module doc",
        "default_value",
        "this module",
        "clap",
    ];

    for (path, text) in every_help_string(&mut Cli::command(), &[]) {
        for banned in BANNED {
            assert!(
                !text.contains(banned),
                "`governance-auth {path} --help` prints {banned:?}, which is a note to a \
                 maintainer, not help. Move it to a `//` comment or to \
                 docs/governance-auth/configuration.md.\n\nfull text: {text}"
            );
        }
    }
}

/// A developer scanning `-h` gets one line per flag, so the short help has to
/// BE one line. The long form is where a paragraph is allowed to live.
#[test]
fn every_short_help_is_one_scannable_line() {
    // Wider than a sentence, narrower than the paragraph that used to be
    // here: the worst offender before this rule was 494 characters.
    const MAX: usize = 140;

    for (path, help) in every_short_help(&mut Cli::command(), &[]) {
        assert!(
            !help.contains('\n'),
            "`{path}`'s short help spans lines; it must be one: {help}"
        );
        assert!(
            help.len() <= MAX,
            "`{path}`'s short help is {} characters, over the {MAX} a person will scan: {help}",
            help.len()
        );
    }
}

/// Every help string clap would render anywhere in the tree, paired with the
/// command path it belongs to: each command's about and long about, and each
/// argument's short and long help.
fn every_help_string(command: &mut clap::Command, path: &[&str]) -> Vec<(String, String)> {
    let here = path.join(" ");
    let mut found: Vec<(String, String)> = command
        .get_about()
        .into_iter()
        .chain(command.get_long_about())
        .map(|text| (here.clone(), text.to_string()))
        .collect();
    found.extend(
        command
            .get_arguments()
            .flat_map(|arg| arg.get_help().into_iter().chain(arg.get_long_help()))
            .map(|text| (here.clone(), text.to_string())),
    );
    for sub in command.get_subcommands_mut() {
        let name = sub.get_name().to_owned();
        let mut deeper: Vec<&str> = path.to_vec();
        deeper.push(&name);
        found.extend(every_help_string(sub, &deeper));
    }
    found
}

/// Every argument's SHORT help in the tree, keyed by `<command> <flag>`.
/// `help` (`hide_short_help` or not) is what `-h` renders; `long_help` is
/// deliberately excluded, since that one is allowed to be a paragraph.
fn every_short_help(command: &mut clap::Command, path: &[&str]) -> Vec<(String, String)> {
    let here = path.join(" ");
    let mut found: Vec<(String, String)> = command
        .get_arguments()
        .filter_map(|arg| {
            arg.get_help()
                .map(|help| (format!("{here} --{}", arg.get_id()), help.to_string()))
        })
        .collect();
    for sub in command.get_subcommands_mut() {
        let name = sub.get_name().to_owned();
        let mut deeper: Vec<&str> = path.to_vec();
        deeper.push(&name);
        found.extend(every_short_help(sub, &deeper));
    }
    found
}
