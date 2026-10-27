use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Operating system names as used by Minecraft metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OsName {
    Windows,
    Linux,
    Osx,
}

impl OsName {
    pub fn as_str(self) -> &'static str {
        match self {
            OsName::Windows => "windows",
            OsName::Linux => "linux",
            OsName::Osx => "osx",
        }
    }

    pub fn current() -> Option<Self> {
        match std::env::consts::OS {
            "windows" => Some(OsName::Windows),
            "linux" => Some(OsName::Linux),
            "macos" => Some(OsName::Osx),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arch {
    X86,
    X86_64,
    Arm64,
    Other,
}

impl Arch {
    pub fn current() -> Self {
        match std::env::consts::ARCH {
            "x86" => Arch::X86,
            "x86_64" => Arch::X86_64,
            "aarch64" => Arch::Arm64,
            _ => Arch::Other,
        }
    }

    /// The value found in `os.arch` rules and `${arch}` natives templates.
    pub fn as_str(self) -> &'static str {
        match self {
            Arch::X86 => "x86",
            Arch::X86_64 => "x86_64",
            Arch::Arm64 => "arm64",
            Arch::Other => "other",
        }
    }

    /// Natives classifier suffix: 32-bit x86 gets `32`, everything else `64`.
    pub fn native_bits(self) -> &'static str {
        match self {
            Arch::X86 => "32",
            _ => "64",
        }
    }
}

/// Everything rule evaluation depends on.
#[derive(Debug, Clone)]
pub struct Environment {
    pub os: OsName,
    pub arch: Arch,
    pub os_version: Option<String>,
    pub features: BTreeMap<String, bool>,
}

impl Environment {
    pub fn current() -> Self {
        Self {
            os: OsName::current().unwrap_or(OsName::Linux),
            arch: Arch::current(),
            os_version: current_os_version(),
            features: BTreeMap::new(),
        }
    }

    /// Environment used by tests and CI fixtures.
    pub fn fixed(os: OsName, arch: Arch) -> Self {
        Self {
            os,
            arch,
            os_version: None,
            features: BTreeMap::new(),
        }
    }

    pub fn with_feature(mut self, name: &str, value: bool) -> Self {
        self.features.insert(name.to_string(), value);
        self
    }

    pub fn with_os_version(mut self, version: impl Into<String>) -> Self {
        self.os_version = Some(version.into());
        self
    }

    pub fn os_name(&self) -> &'static str {
        self.os.as_str()
    }
}

/// What `System.getProperty("os.version")` reports on this platform; version
/// JSON rules such as `"os":{"version":"^10\\."}` match against it.
fn current_os_version() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok()?;
        let release = release.trim();
        (!release.is_empty()).then(|| release.to_string())
    }
    #[cfg(target_os = "windows")]
    {
        // Java reports major.minor, not the build number. `cmd /C ver` prints
        // e.g. `Microsoft Windows [Version 10.0.22631.4317]`. Hide the console:
        // the launcher is a GUI process and must not flash a window.
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("cmd")
            .args(["/C", "ver"])
            .creation_flags(0x0800_0000)
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let token = text.split_whitespace().next_back()?.trim_matches(['[', ']']);
        let mut parts = token.split('.');
        let major = parts.next()?.parse::<u32>().ok()?;
        let minor = parts.next().and_then(|part| part.parse::<u32>().ok()).unwrap_or(0);
        Some(format!("{major}.{minor}"))
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()?;
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!version.is_empty()).then_some(version)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        None
    }
}
