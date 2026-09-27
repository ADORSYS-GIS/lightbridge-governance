//! Split out of `schedule/tests.rs` (issue #364) purely to keep that file
//! under its grandfathered LoC ceiling after adding the callback-port/
//! callback-bind fields to `OauthConfig::config()`'s test fixture -- this
//! test's own subject (XML escaping in the rendered plist) is unrelated to
//! that change.

use std::path::Path;

use super::config;
use crate::schedule::{Invocation, launchd};

#[test]
fn the_plist_escapes_xml_rather_than_emitting_a_broken_agent() {
    // launchd refuses to bootstrap a plist it cannot parse, and a bare `&` in
    // a query string is exactly that -- the job would then never run, silently.
    let mut config = config();
    config.otel_endpoint = Some("https://otel.example.com/?a=1&b=2".to_owned());
    let invocation = Invocation::resolve(&config)
        .expect("resolve")
        .expect("some");
    let (path, plist) = launchd::plist(Path::new("/Users/dev"), &invocation).expect("render");

    assert!(path.ends_with("digital.camer.ai.governance-auth.copilot-push.plist"));
    assert!(
        plist.contains("<string>https://otel.example.com/?a=1&amp;b=2</string>"),
        "got:\n{plist}"
    );
    assert!(
        plist.contains("<string>--copilot-spool-path</string>"),
        "each argv word is its own <string>, not one shell line"
    );
    assert!(plist.contains("<integer>300</integer>"));
    assert!(
        plist.contains("/Users/dev/Library/Logs/governance-auth/governance-auth.log"),
        "launchd has no journal, so stderr must land where Console.app looks \
         -- and in the SAME rotated file `crate::logging` writes, not a \
         second, unbounded one beside it"
    );
}
