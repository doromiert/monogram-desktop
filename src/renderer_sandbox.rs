//! Read-only runtime files for renderer helpers on NixOS and FHS distributions.
use std::{path::Path, process::Command};
pub fn runtime(command: &mut Command) {
    for path in ["/nix/store", "/usr", "/lib", "/lib64", "/bin", "/etc/fonts"] {
        if Path::new(path).exists() {
            command.args(["--ro-bind", path, path]);
        }
    }
}
