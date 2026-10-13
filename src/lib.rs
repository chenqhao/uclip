//! uclip — a cross-platform universal clipboard.
//!
//! This crate contains the domain types, sync logic, and platform
//! abstractions for the uclip daemon and CLI.

// Declare the clipboard module. This tells Rust: "there is a file
// called clipboard.rs — compile it as part of this crate."
// In Java, this is like having a package declaration.
pub mod clipboard;
pub mod engine;
pub mod proto;
pub mod watcher;

// Re-export the key types so callers can write `uclip::ClipContent`
// instead of the longer `uclip::clipboard::ClipContent`.
pub use clipboard::{
    ClipContent, ClipboardBackend, ClipboardError, FileClipboard, MemoryClipboard, SystemClipboard,
    create_backend, create_backend_from_spec,
};
pub use engine::{
    Action, ClockDriftError, DEFAULT_MAX_DRIFT_MS, Engine, HybridLogicalClock, HybridTimestamp,
    LamportClock, UpdateId,
};
pub use proto::{Capability, GoodbyeReason, Message, PROTOCOL_VERSION};
pub use watcher::{ClipEvent, PollingWatcher, WatcherHandle};

use std::fmt;
use std::str::FromStr;

use uuid::Uuid;

/// A unique identifier for a device running uclip.
///
/// This is a newtype wrapper around [`Uuid`] — it prevents accidentally
/// mixing up device IDs with other UUIDs in the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DeviceId(Uuid);

impl DeviceId {
    /// Generate a new random device ID.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for DeviceId {
    fn default() -> Self {
        Self::new()
    }
}

/// Display a DeviceId as its underlying UUID string.
///
/// This is Rust's equivalent of Java's `toString()`. Any type that
/// implements `Display` can be used in `println!("{}", value)` and
/// also gets `.to_string()` for free.
impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Parse a string into a DeviceId.
///
/// This is the counterpart of `Display`: if `Display` converts
/// DeviceId → String, then `FromStr` converts String → DeviceId.
/// Together they form a round-trip.
impl FromStr for DeviceId {
    // The error type returned when parsing fails.
    // We reuse uuid's own error — no need to invent our own yet.
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Parse the string as a UUID, then wrap it in DeviceId.
        // The `?` operator: if Uuid::parse_str returns Err, return
        // that error immediately. If it returns Ok, unwrap the value
        // and continue. It's like a try-catch that re-throws.
        let uuid = Uuid::parse_str(s)?;
        Ok(Self(uuid))
    }
}

use directories::ProjectDirs;
use std::env;
use std::path::{Path, PathBuf};

/// Resolves the directories where uclip stores its config and data.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The root directory for configuration files (e.g., settings.toml).
    config_dir: PathBuf,
    /// The root directory for data files (e.g., identity keys, database).
    data_dir: PathBuf,
}

impl Paths {
    /// Discover the paths based on the OS conventions, or use `UCLIP_HOME` if set.
    ///
    /// Returns `None` if the OS home directory cannot be determined (very rare).
    pub fn new() -> Option<Self> {
        // If the user (or a test) sets UCLIP_HOME, use that for both config and data.
        if let Ok(home) = env::var("UCLIP_HOME") {
            let base = PathBuf::from(home);
            return Some(Self {
                config_dir: base.clone(),
                data_dir: base,
            });
        }

        // Otherwise, ask the OS where things should go.
        let dirs = ProjectDirs::from("com", "uclip", "uclip")?;

        Some(Self {
            // .to_path_buf() converts a borrowed &Path into an owned PathBuf.
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
        })
    }

    /// The root directory for configuration files.
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// The root directory for data files.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// The path to the main configuration file.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// The path to the device identity key.
    pub fn identity_file(&self) -> PathBuf {
        self.data_dir.join("identity.key")
    }
}

use serde::{Deserialize, Serialize};

/// User preferences saved in `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    /// The port the daemon listens on for incoming sync requests.
    pub port: u16,
    /// If false, the daemon pauses syncing completely.
    pub sync_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            port: 8443,
            sync_enabled: true,
        }
    }
}

/// Information about this specific device.
/// Saved in `identity.key` (or similar file in data_dir).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceInfo {
    /// The unique identifier of this device.
    pub id: DeviceId,
    /// A human-readable name, e.g., "Howie's MacBook".
    pub name: String,
}

// ── Tests ──────────────────────────────────────────────────────

// This module only exists when running `cargo test`.
// It is completely removed from the final binary.
#[cfg(test)]
mod tests {
    // Import everything from the parent module (lib.rs).
    // `super` means "one level up" — like `..` in a file path.
    use super::*;

    #[test]
    fn device_id_round_trips_through_string() {
        let original = DeviceId::new();

        // Display: DeviceId → String
        let text = original.to_string();

        // FromStr: String → DeviceId
        // .expect() is like .unwrap() but with a custom message.
        let parsed: DeviceId = text.parse().expect("should parse back");

        assert_eq!(original, parsed);
    }

    #[test]
    fn paths_respects_uclip_home_override() {
        // `env::set_var` is unsafe in recent Rust versions because it can cause data races
        // in multi-threaded programs (and `cargo test` runs tests in parallel on multiple threads).
        // Instead, we use the `temp-env` crate to safely set the environment variable
        // just for the duration of this closure.
        temp_env::with_var("UCLIP_HOME", Some("/tmp/fake_uclip_home"), || {
            let paths = Paths::new().expect("should create paths");

            // Both config and data dirs should point to our override
            assert_eq!(paths.config_dir, Path::new("/tmp/fake_uclip_home"));
            assert_eq!(paths.data_dir, Path::new("/tmp/fake_uclip_home"));

            // The files should be inside the override folder
            assert_eq!(
                paths.config_file(),
                Path::new("/tmp/fake_uclip_home/config.toml")
            );
            assert_eq!(
                paths.identity_file(),
                Path::new("/tmp/fake_uclip_home/identity.key")
            );
        });
    }

    #[test]
    fn settings_serialize_to_toml() {
        let settings = Settings::default();

        // toml::to_string takes a reference to our struct and returns a Result<String, Error>
        let toml_str = toml::to_string(&settings).expect("should serialize");

        // Let's verify what it looks like!
        assert!(toml_str.contains("port = 8443"));
        assert!(toml_str.contains("sync_enabled = true"));

        // Now parse it back
        let parsed: Settings = toml::from_str(&toml_str).expect("should deserialize");
        assert_eq!(settings, parsed);
    }
}
