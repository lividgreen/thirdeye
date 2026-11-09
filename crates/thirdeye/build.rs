//! Stamps the build with the commit it came from.
//!
//! Without this, "is the installed binary this code?" has no answer, and every
//! consistency check `thirdeye doctor` makes degrades to a guess. The `-dirty`
//! suffix is the load-bearing part for a contributor: it distinguishes running
//! your edit from running your last commit.

use std::process::Command;

fn main() {
    // Re-stamp when HEAD moves. Uncommitted edits are caught by `--dirty`
    // being computed at build time, and cargo rebuilds on source changes
    // anyway, so the stamp cannot lag the code it describes.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");

    let described = Command::new("git")
        .args(["describe", "--always", "--dirty", "--abbrev=8"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        // A tarball with no .git is a legitimate way to build this.
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=THIRDEYE_BUILD={described}");
}
