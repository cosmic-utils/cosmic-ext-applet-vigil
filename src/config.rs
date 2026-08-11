// SPDX-License-Identifier: MPL-2.0

use cosmic::cosmic_config::{self, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry};

#[derive(Debug, Clone, CosmicConfigEntry, Eq, PartialEq)]
#[version = 1]
pub struct Config {
    /// Last-used duration in minutes. 0 means indefinite.
    pub duration_mins: u32,
    /// Whether Vigil is currently inhibiting.
    ///
    /// `cosmic-panel` spawns one applet process per output, so a multi-monitor
    /// setup runs several independent instances. Keeping this in config rather
    /// than in `AppModel` lets every instance observe the same state through
    /// the existing `watch_config` subscription.
    pub active: bool,
    /// Unix timestamp, in seconds, at which the inhibit expires.
    ///
    /// Zero when indefinite or inactive. Stored as an absolute deadline rather
    /// than a remaining count so instances stay in agreement without writing
    /// config once per second.
    pub expiry_ts: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            duration_mins: 30,
            active: false,
            expiry_ts: 0,
        }
    }
}
