//! Which GPU the game renders on.

use serde::{Deserialize, Serialize};

/// The GPU the game should render on.
///
/// Linux is the only platform where this matters: a laptop can drive its screen
/// from the integrated GPU while a discrete card sits idle. Everything else
/// renders on whichever card owns the display, so the preference is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GpuPreference {
    /// Render on the discrete card when the machine has one the screen can take
    /// offloaded output from; otherwise change nothing.
    #[default]
    Auto,
    /// Never change the environment: the game renders on the card that drives
    /// the screen.
    Screen,
    /// Always ask for the discrete card, even when detection found none.
    Discrete,
}

impl GpuPreference {
    /// The value stored in `instance.json`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Screen => "screen",
            Self::Discrete => "discrete",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_auto() {
        assert_eq!(GpuPreference::default(), GpuPreference::Auto);
    }

    #[test]
    fn values_round_trip_through_json() {
        for preference in [GpuPreference::Auto, GpuPreference::Screen, GpuPreference::Discrete] {
            let json = serde_json::to_string(&preference).unwrap();
            assert_eq!(json, format!("\"{}\"", preference.as_str()));
            assert_eq!(serde_json::from_str::<GpuPreference>(&json).unwrap(), preference);
        }
    }

    #[test]
    fn a_file_without_the_field_loads_as_auto() {
        #[derive(Deserialize)]
        struct Holder {
            #[serde(default)]
            gpu: GpuPreference,
        }
        let holder: Holder = serde_json::from_str("{}").unwrap();
        assert_eq!(holder.gpu, GpuPreference::Auto);
    }
}
