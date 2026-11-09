//! A machine for an integration test: scratch home and data directory, no
//! environment, and an agent CLI that records what it was asked.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::Context;

pub const BINARY: &str = "/opt/thirdeye bin/thirdeye";

/// A fresh directory for one test.
pub fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("thirdeye-int-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn no_vars(_: &str) -> Option<OsString> {
    None
}

/// An agent CLI that is not there.
pub fn no_run(program: &str, _: &[&str], _: Option<&Path>) -> Result<String> {
    anyhow::bail!("{program} is not run in this test")
}

/// The calls an agent CLI received, as `program arg arg…`.
#[derive(Default)]
pub struct Calls(pub RefCell<Vec<String>>);

impl Calls {
    pub fn record(&self, program: &str, args: &[&str]) {
        self.0
            .borrow_mut()
            .push(format!("{program} {}", args.join(" ")));
    }

    pub fn take(&self) -> Vec<String> {
        self.0.take()
    }
}

/// A context under `base`: the binary is [`BINARY`], `base/home` the home,
/// `base/data` thirdeye's directory.
pub fn context<'a>(
    base: &Path,
    var: &'a dyn Fn(&str) -> Option<OsString>,
    run: &'a super::Runner<'a>,
) -> Context<'a> {
    Context {
        binary: PathBuf::from(BINARY),
        data: base.join("data"),
        home: Some(base.join("home")),
        var,
        run,
    }
}

/// Every file under `dir`, with its bytes, for asserting a tree unchanged.
pub fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                walk(root, &path, out);
            } else if meta.is_symlink() {
                let to = std::fs::read_link(&path).unwrap();
                out.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    to.display().to_string().into_bytes(),
                ));
            } else {
                out.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}
