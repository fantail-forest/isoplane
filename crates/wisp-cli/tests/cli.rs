//! The `wisp` binary, driven as a user would drive it.

mod common;

use common::run;



#[test]
fn every_documented_subcommand_exists() {
    let help = run(&["--help"]).stdout;
    for cmd in ["dev", "doctor"] {
        assert!(help.contains(cmd), "`wisp {cmd}` is missing from --help");
    }
}


