//! The host identity pi presents to JavaScript extensions.
//!
//! Packages written for Pi detect how Pi was installed from the JS runtime. Released Pi binaries
//! are Bun single-file executables, and packages recognise them by `process.versions.bun` plus a
//! virtual entry script in `process.argv[1]` (pi-subagents' `resolveBunPiExecutable`). pi-rs is a
//! standalone executable in the same sense, so the extension runtime presents that identity:
//!
//! - `process.versions.bun` is [`HostIdentity::bun_version`];
//! - `process.argv` is `[<exe>, <entry>, ...real arguments]` with [`HostIdentity::entry`];
//! - `process.execPath` stays the real executable;
//! - `globalThis.Bun` stays undefined: pi-rs implements Node's APIs, not Bun's (context-mode keys
//!   `bun:sqlite` off it);
//! - [`HostIdentity::pi_binary_env`] is set to the executable in `process.env` when unset, so
//!   packages that only trust an executable named `pi` (pi-subagents' `getPiSpawnCommand`) spawn
//!   pi-rs rather than whatever `pi` is on `PATH`.
//!
//! Node-mode children (`child_process.fork()`, scripts run with the executable) keep a plain
//! Node identity: they start their own runtime without this overlay.

use std::collections::{BTreeMap, HashMap};

/// The identity [`super::JsExtensionHost`] presents; see the [module docs](self).
pub const HOST_IDENTITY: HostIdentity = HostIdentity::COMPILED_PI;

/// What the extension runtime reports about the program hosting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostIdentity {
    /// `process.versions.bun`.
    pub bun_version: &'static str,
    /// `process.argv[1]`: the entry script of the executable.
    pub entry: &'static str,
    /// Environment variable set to the executable's path when unset.
    pub pi_binary_env: &'static str,
}

impl HostIdentity {
    /// Pi 1.4.0's release binaries: `bun build --compile` with Bun 1.3.14
    /// (`.github/workflows/build-binaries.yml`) of an entry named `pi`, whose virtual path on
    /// Linux and macOS is `/$bunfs/root/pi`.
    pub const COMPILED_PI: HostIdentity =
        HostIdentity { bun_version: "1.3.14", entry: "/$bunfs/root/pi", pi_binary_env: "PI_SUBAGENT_PI_BINARY" };

    /// Entries added to `process.versions`.
    pub fn versions(&self) -> BTreeMap<String, String> {
        BTreeMap::from([("bun".to_string(), self.bun_version.to_string())])
    }

    /// `process.argv` for the executable `exe` started with `args` (without the program name).
    pub fn argv(&self, exe: &str, args: impl IntoIterator<Item = String>) -> Vec<String> {
        [exe.to_string(), self.entry.to_string()].into_iter().chain(args).collect()
    }

    /// `env` with [`HostIdentity::pi_binary_env`] defaulted to `exe` (an empty or blank value counts
    /// as unset, as pi-subagents trims it).
    pub fn env(&self, mut env: HashMap<String, String>, exe: &str) -> HashMap<String, String> {
        let set = env.get(self.pi_binary_env).is_some_and(|value| !value.trim().is_empty());
        if !set {
            env.insert(self.pi_binary_env.to_string(), exe.to_string());
        }
        env
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_puts_the_virtual_entry_after_the_executable() {
        let argv = HOST_IDENTITY.argv("/opt/pi-rs", ["-p".to_string(), "hi".to_string()]);
        assert_eq!(argv, ["/opt/pi-rs", "/$bunfs/root/pi", "-p", "hi"]);
    }

    #[test]
    fn env_keeps_an_explicit_pi_binary() {
        let explicit = HashMap::from([("PI_SUBAGENT_PI_BINARY".to_string(), "/usr/bin/other".to_string())]);
        assert_eq!(HOST_IDENTITY.env(explicit.clone(), "/opt/pi-rs"), explicit);
        for unset in [HashMap::new(), HashMap::from([("PI_SUBAGENT_PI_BINARY".to_string(), " ".to_string())])] {
            let env = HOST_IDENTITY.env(unset, "/opt/pi-rs");
            assert_eq!(env["PI_SUBAGENT_PI_BINARY"], "/opt/pi-rs");
        }
    }
}
