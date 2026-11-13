//! Pick the GPU a game process renders with.
//!
//! On Linux a laptop can run its X screen on the integrated GPU while a
//! discrete card sits idle. With PRIME render offload the game renders on the
//! discrete card and the result is copied to the screen; this needs three
//! environment variables, and nothing in the game or Java knows about them.
//! Without them the game silently renders on the integrated GPU.
//!
//! Detection is offline and read-only: the DRM sysfs tree names the cards and
//! their drivers, `xrandr` is only asked whether the X screen has more than one
//! provider. When detection is impossible the user's choice still wins.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use tntl_model::GpuPreference;

/// Environment `prime-run` (nvidia-prime) sets to render on the discrete card.
const OFFLOAD_ENV: [(&str, &str); 3] = [
    ("__NV_PRIME_RENDER_OFFLOAD", "1"),
    ("__VK_LAYER_NV_optimus", "NVIDIA_only"),
    ("__GLX_VENDOR_LIBRARY_NAME", "nvidia"),
];

/// One DRM card as sysfs describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    /// Device node name, e.g. `card0`.
    pub node: String,
    /// Kernel driver bound to it, e.g. `nvidia` or `amdgpu`.
    pub driver: Option<String>,
    /// Whether a connector under it reports a connected display.
    pub connected: bool,
    /// PCI address when the sysfs path exposes one, e.g. `0000:01:00.0`.
    pub pci: Option<String>,
}

/// Everything detection could learn about this machine's graphics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Graphics {
    pub cards: Vec<Card>,
    /// How many X providers `xrandr` reported, when it answered.
    pub x_providers: Option<usize>,
}

impl Graphics {
    /// The card driving the screen: the one with a connected connector, else
    /// the first.
    #[must_use]
    pub fn primary(&self) -> Option<&Card> {
        self.cards
            .iter()
            .find(|card| card.connected)
            .or_else(|| self.cards.first())
    }

    /// The discrete card a PRIME offload can render on, when there is one.
    ///
    /// The NVIDIA driver only exposes offload on the proprietary and open
    /// kernel modules, which both report `nvidia`. A machine whose only card is
    /// that one is already rendering on it.
    #[must_use]
    pub fn discrete(&self) -> Option<&Card> {
        let discrete = self
            .cards
            .iter()
            .find(|card| card.driver.as_deref() == Some("nvidia"))?;
        let primary = self.primary()?;
        (primary.node != discrete.node).then_some(discrete)
    }

    /// Whether rendering off the discrete card is possible at all.
    ///
    /// Needs a discrete card that is not driving the screen, and an X screen
    /// that can take offloaded output. A missing `xrandr` is not a refusal: one
    /// provider is what a machine with a single GPU reports, so only a count of
    /// one rules it out.
    #[must_use]
    pub fn offload_available(&self) -> bool {
        self.discrete().is_some() && self.x_providers != Some(1)
    }

    /// A short human-readable summary, for logs and the instance page.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = self
            .cards
            .iter()
            .map(|card| {
                let driver = card.driver.as_deref().unwrap_or("unknown driver");
                match (card.connected, &card.pci) {
                    (true, Some(pci)) => format!("{driver} ({pci}, drives the screen)"),
                    (true, None) => format!("{driver} (drives the screen)"),
                    (false, Some(pci)) => format!("{driver} ({pci})"),
                    (false, None) => driver.to_string(),
                }
            })
            .collect();
        if parts.is_empty() {
            parts.push("no DRM card found".to_string());
        }
        parts.join("; ")
    }
}

/// Read the DRM tree under `root` (normally `/sys/class/drm`).
#[must_use]
pub fn read_cards(root: &Path) -> Vec<Card> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut cards: Vec<Card> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // `card0`, not `card0-DP-1`, `renderD128` or `version`.
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let device = entry.path().join("device");
        if !device.is_dir() {
            continue;
        }
        let driver = std::fs::read_link(device.join("driver"))
            .ok()
            .and_then(|target| target.file_name().map(|name| name.to_string_lossy().into_owned()));
        // Connectors of this card are `<node>-<output>`, e.g. `card1-DP-4`.
        let prefix = format!("{name}-");
        let connected = std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|connector| {
                connector
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(&prefix))
            })
            .any(|connector| {
                std::fs::read_to_string(connector.path().join("status"))
                    .is_ok_and(|status| status.trim() == "connected")
            });
        cards.push(Card {
            node: name,
            driver,
            connected,
            pci: pci_address(&device),
        });
    }
    cards.sort_by(|left, right| left.node.cmp(&right.node));
    cards
}

/// The PCI address a sysfs device path ends with, when it has one.
fn pci_address(device: &Path) -> Option<String> {
    std::fs::canonicalize(device).ok().and_then(|path| {
        path.components().rev().find_map(|component| {
            let text = component.as_os_str().to_string_lossy();
            let looks_like_pci = text.len() == 12
                && text.as_bytes()[4] == b':'
                && text.as_bytes()[7] == b':'
                && text.as_bytes()[10] == b'.';
            looks_like_pci.then(|| text.into_owned())
        })
    })
}

/// Count the X providers `xrandr --listproviders` prints, when it can run.
///
/// The `DISPLAY` the game will inherit is the one that matters; a failure to
/// reach it is reported as unknown rather than as "one provider".
fn x_provider_count() -> Option<usize> {
    let output = std::process::Command::new("xrandr")
        .arg("--listproviders")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .find_map(|line| line.trim().strip_prefix("Providers:"))
        .and_then(parse_provider_count)
}

/// The count in a `Providers:` line.
///
/// The real line is `Providers: number : 2`, not `Providers: 2`, so the count
/// is the first token that is a number rather than the first token.
fn parse_provider_count(rest: &str) -> Option<usize> {
    rest.split_whitespace().find_map(|token| token.parse().ok())
}

/// Detect this machine's graphics.
#[must_use]
pub fn detect() -> Graphics {
    Graphics {
        cards: read_cards(Path::new("/sys/class/drm")),
        x_providers: x_provider_count(),
    }
}

/// The variables to add for `preference`, given what detection found.
///
/// Variables the user already set are never overridden: an explicit
/// `__GLX_VENDOR_LIBRARY_NAME` means they are driving this themselves.
#[must_use]
pub fn offload_env(
    preference: GpuPreference,
    graphics: &Graphics,
    existing: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let wanted = match preference {
        GpuPreference::Screen => false,
        GpuPreference::Discrete => true,
        GpuPreference::Auto => graphics.offload_available(),
    };
    if !wanted {
        return BTreeMap::new();
    }
    OFFLOAD_ENV
        .iter()
        .filter(|(key, _)| !existing.contains_key(*key))
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// A path to the NVIDIA device node, when one exists. Used to tell "the driver
/// is loaded" from "the card is present".
#[must_use]
pub fn nvidia_device_present() -> bool {
    PathBuf::from("/dev/nvidia0").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(node: &str, driver: &str, connected: bool, pci: &str) -> Card {
        Card {
            node: node.to_string(),
            driver: Some(driver.to_string()),
            connected,
            pci: Some(pci.to_string()),
        }
    }

    /// The layout of the machine this was written for: X on the iGPU, the
    /// discrete card idle.
    fn igpu_primary() -> Graphics {
        Graphics {
            cards: vec![
                card("card0", "nvidia", false, "0000:01:00.0"),
                card("card1", "amdgpu", true, "0000:03:00.0"),
            ],
            x_providers: Some(2),
        }
    }

    #[test]
    fn primary_is_the_connected_card() {
        assert_eq!(igpu_primary().primary().unwrap().node, "card1");
    }

    #[test]
    fn discrete_offload_is_available_when_x_on_the_igpu() {
        let graphics = igpu_primary();
        assert_eq!(graphics.discrete().unwrap().node, "card0");
        assert!(graphics.offload_available());
    }

    #[test]
    fn auto_adds_the_offload_variables() {
        let env = offload_env(GpuPreference::Auto, &igpu_primary(), &BTreeMap::new());
        assert_eq!(env.get("__NV_PRIME_RENDER_OFFLOAD").map(String::as_str), Some("1"));
        assert_eq!(env.get("__GLX_VENDOR_LIBRARY_NAME").map(String::as_str), Some("nvidia"));
        assert_eq!(env.len(), 3);
    }

    #[test]
    fn default_adds_nothing() {
        assert!(offload_env(GpuPreference::Screen, &igpu_primary(), &BTreeMap::new()).is_empty());
    }

    #[test]
    fn discrete_forces_the_variables_without_detection() {
        let empty = Graphics::default();
        assert!(!empty.offload_available());
        assert_eq!(offload_env(GpuPreference::Discrete, &empty, &BTreeMap::new()).len(), 3);
    }

    #[test]
    fn auto_does_nothing_when_the_discrete_card_drives_the_screen() {
        let graphics = Graphics {
            cards: vec![card("card0", "nvidia", true, "0000:01:00.0")],
            x_providers: Some(1),
        };
        assert!(graphics.discrete().is_none());
        assert!(!graphics.offload_available());
        assert!(offload_env(GpuPreference::Auto, &graphics, &BTreeMap::new()).is_empty());
    }

    #[test]
    fn one_provider_rules_out_offload() {
        let graphics = Graphics {
            x_providers: Some(1),
            ..igpu_primary()
        };
        assert!(!graphics.offload_available());
    }

    #[test]
    fn an_unknown_provider_count_does_not_block_offload() {
        let graphics = Graphics {
            x_providers: None,
            ..igpu_primary()
        };
        assert!(graphics.offload_available());
    }

    #[test]
    fn variables_the_user_set_are_kept() {
        let existing = BTreeMap::from([("__GLX_VENDOR_LIBRARY_NAME".to_string(), "mesa".to_string())]);
        let env = offload_env(GpuPreference::Auto, &igpu_primary(), &existing);
        assert_eq!(env.len(), 2);
        assert!(!env.contains_key("__GLX_VENDOR_LIBRARY_NAME"));
    }

    #[test]
    fn parses_the_real_providers_line() {
        // Verbatim from `xrandr --listproviders` on a hybrid laptop. The count
        // is the third token, which a "first token" parser reads as `number`.
        assert_eq!(parse_provider_count(" number : 2"), Some(2));
        assert_eq!(parse_provider_count(" 1"), Some(1));
        assert_eq!(parse_provider_count(" number : 3 "), Some(3));
        assert_eq!(parse_provider_count(" number : unknown"), None);
        assert_eq!(parse_provider_count(""), None);
    }

    #[test]
    fn reading_a_tree_without_cards_is_not_an_error() {
        let dir = std::env::temp_dir().join(format!("tntl-gpu-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_cards(&dir).is_empty());
        let graphics = Graphics {
            cards: read_cards(&dir),
            x_providers: None,
        };
        assert!(graphics.primary().is_none());
        assert!(offload_env(GpuPreference::Auto, &graphics, &BTreeMap::new()).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
