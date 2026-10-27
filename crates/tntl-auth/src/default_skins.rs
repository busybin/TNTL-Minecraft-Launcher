//! Skins the launcher ships with, so an offline account has something to wear
//! without an online skin service. All of them are original artwork.

pub const DEFAULT_SKINS: [&str; 6] = ["classic", "ember", "ocean", "moss", "sun", "violet"];

/// A pick for a fresh offline account, so the avatar and the in-game player
/// are not blank. Wall-clock nanoseconds are enough here: the value only
/// decides which of six built-in skins a new account wears.
pub fn random_skin_key() -> &'static str {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos() as u64 ^ elapsed.as_secs())
        .unwrap_or_default();
    let seed = nanos ^ (u64::from(std::process::id()) << 17);
    // One splitmix64 step spreads names created in the same millisecond.
    let mixed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    DEFAULT_SKINS[((mixed ^ (mixed >> 31)) % DEFAULT_SKINS.len() as u64) as usize]
}

/// PNG bytes of one built-in skin.
pub fn default_skin_image(key: &str) -> Option<&'static [u8]> {
    let bytes: &'static [u8] = match key {
        "classic" => include_bytes!("../assets/skins/classic.png"),
        "ember" => include_bytes!("../assets/skins/ember.png"),
        "ocean" => include_bytes!("../assets/skins/ocean.png"),
        "moss" => include_bytes!("../assets/skins/moss.png"),
        "sun" => include_bytes!("../assets/skins/sun.png"),
        "violet" => include_bytes!("../assets/skins/violet.png"),
        _ => return None,
    };
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_skins_are_valid_pngs() {
        for key in DEFAULT_SKINS {
            let bytes = default_skin_image(key).expect("listed skin has bytes");
            let (width, height) = crate::png_dimensions(bytes).expect("valid png");
            assert_eq!((width, height), (64, 64), "{key}");
        }
        assert!(default_skin_image("nope").is_none());
    }

    #[test]
    fn random_picks_only_built_in_skins() {
        for _ in 0..32 {
            let key = random_skin_key();
            assert!(DEFAULT_SKINS.contains(&key), "{key}");
        }
    }
}
