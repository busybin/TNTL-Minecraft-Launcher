//! Hosts shared by several crates; crate-specific API hosts live next to the
//! crate that calls them.

pub const VERSION_MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
pub const LEGACY_VERSION_MANIFEST: &str = "https://launchermeta.mojang.com/mc/game/version_manifest.json";
pub const LIBRARIES: &str = "https://libraries.minecraft.net/";
pub const RESOURCES: &str = "https://resources.download.minecraft.net/";

/// Fabric and Quilt loader metadata (official project services).
pub const FABRIC_META: &str = "https://meta.fabricmc.net/v2";
pub const QUILT_META: &str = "https://meta.quiltmc.org/v3";
/// Legacy Fabric keeps 1.8–1.13.2 on its own metadata and maven.
pub const LEGACY_FABRIC_META: &str = "https://meta.legacyfabric.net/v2";
/// Fallback maven repositories advertised by the loader profiles.
pub const FABRIC_MAVEN: &str = "https://maven.fabricmc.net/";
pub const QUILT_MAVEN: &str = "https://maven.quiltmc.org/repository/release/";
pub const LEGACY_FABRIC_MAVEN: &str = "https://maven.legacyfabric.net/";
/// Modrinth v2 API: project search, versions and files.
pub const MODRINTH_API: &str = "https://api.modrinth.com/v2";
/// CurseForge API: search, files and fingerprints. Needs the user's own key.
pub const CURSEFORGE_API: &str = "https://api.curseforge.com/v1";
/// FTB modpacks: the same public API the FTB App and Prism use.
pub const FTB_API: &str = "https://api.modpacks.ch/public";
/// Mojang's Java runtimes, the same ones the official launcher uses.
pub const JAVA_RUNTIMES: &str =
    "https://launchermeta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";
/// Eclipse Adoptium (Temurin) API: JDKs for versions Mojang does not ship.
pub const ADOPTIUM_API: &str = "https://api.adoptium.net/v3";
pub const LAUNCHER_NAME: &str = "tntl";

pub fn user_agent() -> String {
    format!("{LAUNCHER_NAME}/{}", env!("CARGO_PKG_VERSION_MAJOR"))
}
