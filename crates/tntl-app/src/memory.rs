//! Memory-advice policy for the launch settings slider.
//!
//! Band edges are fixed policy, not per-game data: a JVM under 1.5 GB cannot
//! host a modern Minecraft build, 2–6 GB is the normal green window, and an
//! allocation that would leave the OS less than ~4 GB drowns the machine.
//!
//! Reading the installed size stays outside this crate: Linux is safe to read
//! from `/proc` here, Windows queries `GetPhysicallyInstalledSystemMemory` in
//! the desktop shell (its crate allows the FFI call) and hands the number to
//! [`advice_from_total`].

/// Smallest allocation the slider offers, in MiB.
pub const FLOOR_MB: u32 = 256;

/// Memory kept away from the game. The slider never lets an instance claim the
/// whole machine: the OS and the launcher still have to live there.
pub const OS_RESERVE_MB: u32 = 2 * 1024;

/// Band edges for the slider, in MiB. Drawn as red before [`Self::green_min_mb`],
/// green up to [`Self::green_max_mb`], yellow up to [`Self::warn_max_mb`], red again.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct MemoryAdvice {
    /// Installed RAM, `None` when the machine refuses to say.
    pub total_mb: Option<u64>,
    /// Green band start: the smallest sane allocation.
    pub green_min_mb: u32,
    /// Green band end: the biggest allocation we call recommended.
    pub green_max_mb: u32,
    /// Yellow band end: works, but squeezes the system. Above it is red.
    pub warn_max_mb: u32,
    /// Top of the slider: installed RAM minus the OS reserve.
    pub limit_mb: u32,
}

/// Installed RAM in MiB on Linux, `None` elsewhere (the desktop shell asks
/// the OS directly and takes `advice_from_total` instead).
pub fn system_memory_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let info = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = info.lines().find(|line| line.starts_with("MemTotal:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb / 1024)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Compute the bands from a detected total. An unknown total pretends 16 GiB
/// so the slider still has sensible green and yellow zones.
pub fn advice_from_total(total_mb: Option<u64>) -> MemoryAdvice {
    let total = total_mb.unwrap_or(16 * 1024).max(4 * 1024) as u32;
    // Two GiB stay with the OS, so the top of the slider is never the whole
    // machine. Tiny (4 GiB) hosts still get a usable 2 GiB window.
    let limit = total.saturating_sub(OS_RESERVE_MB).max(2 * 1024);
    let green_min = (total / 4).clamp(1024, 2 * 1024).min(limit);
    let green_max = (total / 2).clamp(green_min, 6 * 1024).min(limit);
    // Red only starts once four GiB are gone from the OS, so the last stretch
    // of the track warns instead of pretending the whole machine is free.
    let warn = total.saturating_sub(2 * OS_RESERVE_MB).max(green_max).min(limit);
    MemoryAdvice {
        total_mb,
        green_min_mb: green_min,
        green_max_mb: green_max,
        warn_max_mb: warn,
        limit_mb: limit,
    }
}

/// Bands for this machine, however the caller read the size.
pub fn advice() -> MemoryAdvice {
    advice_from_total(system_memory_mb())
}

/// Clamp a launch memory value into the slider range.
pub fn clamp_mb(mb: u32) -> u32 {
    let limit = advice_from_total(system_memory_mb()).limit_mb;
    mb.clamp(FLOOR_MB, limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_never_exceeds_installed_memory() {
        // Keep a couple of GiB for the system: a 17 GiB host suggests 15 GiB.
        let advice = advice_from_total(Some(17 * 1024));
        assert_eq!(advice.limit_mb, 15 * 1024);
        assert!(advice.limit_mb < 17 * 1024);
        assert!(advice.warn_max_mb <= advice.limit_mb);
        assert!(advice.green_max_mb <= advice.warn_max_mb);
    }

    #[test]
    fn bands_stay_ordered_on_small_and_large_hosts() {
        for total in [4, 8, 16, 32, 64] {
            let advice = advice_from_total(Some(total * 1024));
            assert!(advice.green_min_mb < advice.green_max_mb);
            assert!(advice.green_max_mb <= advice.warn_max_mb);
            assert!(advice.warn_max_mb <= advice.limit_mb);
            assert!(advice.limit_mb < (total as u32) * 1024);
        }
    }

    #[test]
    fn unknown_total_falls_back_to_sixteen_gigabytes() {
        let advice = advice_from_total(None);
        assert_eq!(advice.total_mb, None);
        assert_eq!(advice.limit_mb, 14 * 1024);
    }
}
