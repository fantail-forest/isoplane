//! The `wisp` binary, driven as a user would drive it.

mod common;

use common::run;

#[test]
fn a_zero_watch_interval_is_refused_before_starting_local_development() {
    let out = run(&["dev", "--poll-ms", "0"]);
    assert_eq!(
        out.code,
        Some(1),
        "a zero watch interval must fail before opening project resources"
    );
    assert!(
        out.stderr.contains("--poll-ms must be greater than zero"),
        "the error must identify the invalid polling interval: {}",
        out.stderr
    );
}



#[test]
fn every_documented_subcommand_exists() {
    let help = run(&["--help"]).stdout;
    for cmd in ["dev", "doctor"] {
        assert!(help.contains(cmd), "`wisp {cmd}` is missing from --help");
    }
}


