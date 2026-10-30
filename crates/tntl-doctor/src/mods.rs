//! Offline checks for the jars in `mods/`.

use std::collections::{BTreeMap, BTreeSet};

use tntl_instance::mods::{ModInfo, ModLoader};

use crate::version::{any_matches, any_matches_series};
use crate::{Finding, Severity, tidy};

/// Facts about the instance the mods are checked against.
pub struct ModContext<'a> {
    /// `None` when the instance has no mod loader.
    pub loader: Option<ModLoader>,
    pub game_version: &'a str,
    pub loader_version: Option<&'a str>,
    pub java_major: Option<u32>,
    pub memory_mb: u32,
    pub total_memory_mb: Option<u64>,
}

pub fn loader_name(loader: ModLoader) -> &'static str {
    match loader {
        ModLoader::Fabric => "Fabric",
        ModLoader::Quilt => "Quilt",
        ModLoader::Forge => "Forge",
        ModLoader::NeoForge => "NeoForge",
        ModLoader::Universal => "Universal",
        ModLoader::Unknown => "Unknown",
    }
}

fn compatible(instance: ModLoader, module: ModLoader, game_version: &str) -> bool {
    tntl_instance::mods::compatible(instance, module, game_version)
}

/// `1.20.1-47.4.28` -> `47.4.28`: Forge-family metadata declares the loader's
/// own build number, the installed build is named after the game as well.
pub fn loader_build_number<'a>(version: &'a str, game_version: &str) -> &'a str {
    version
        .strip_prefix(game_version)
        .and_then(|rest| rest.strip_prefix('-'))
        .filter(|rest| rest.starts_with(|ch: char| ch.is_ascii_digit()))
        .unwrap_or(version)
}

/// Forge-family mods declare the loader's own numbering (`[47,)`,
/// `[47.4.0,)`), while the installed build is named `1.20.1-47.4.28`.
pub fn effective_loader_version(
    instance: ModLoader,
    module: ModLoader,
    version: &str,
    game_version: &str,
) -> Option<String> {
    let forge_family = |loader: ModLoader| matches!(loader, ModLoader::Forge | ModLoader::NeoForge);
    // A universal jar on a Forge instance is read through its `mods.toml`,
    // even though the jar also carries Fabric metadata.
    if forge_family(instance) && (forge_family(module) || module == ModLoader::Universal) {
        return Some(loader_build_number(version, game_version).to_string());
    }
    Some(version.to_string())
}

/// The loader whose metadata a jar's dependency list comes from. A universal
/// jar is read through Forge metadata on Forge and through Fabric metadata
/// elsewhere, so bare version recommendations must be judged by that loader.
fn metadata_loader(module: &ModInfo, instance: Option<ModLoader>) -> ModLoader {
    if module.loader == ModLoader::Universal {
        instance.unwrap_or(ModLoader::Forge)
    } else {
        module.loader
    }
}

/// Forge reads a bare version (`1.20.1-1.2.7`) as a recommendation, so only
/// bracketed Maven ranges can stop the game from loading.
pub fn enforced_ranges(loader: ModLoader, ranges: &[String]) -> Vec<String> {
    ranges
        .iter()
        .filter(|range| {
            let range = range.trim();
            !matches!(loader, ModLoader::Forge | ModLoader::NeoForge)
                || range.starts_with('[')
                || range.starts_with('(')
        })
        .cloned()
        .collect()
}

pub fn check(mods: &[ModInfo], context: &ModContext<'_>) -> Vec<Finding> {
    let enabled: Vec<&ModInfo> = mods.iter().filter(|module| module.enabled).collect();
    let mut findings = Vec::new();

    // Jars the installer switched off because the instance can never load
    // them: they sit in `mods/` looking installed, so say why they are dead.
    for module in mods.iter().filter(|module| !module.enabled) {
        if let Some(blocked) = &module.blocked {
            findings.push(
                Finding::new(Severity::Warning, "blocked_mod")
                    .subject(&module.name)
                    .param("loader", &blocked.loader)
                    .param("required", blocked.required.join(" / "))
                    .param("actual", &blocked.actual)
                    .param("file", &module.file),
            );
        }
    }

    if enabled.is_empty() {
        findings.extend(memory_findings(&enabled, context));
        return tidy(findings);
    }
    if context.loader.is_none() {
        findings.push(
            Finding::new(Severity::Error, "vanilla_mods")
                .param("count", enabled.len().to_string())
                .param("mods", names(&enabled, 4)),
        );
        findings.extend(memory_findings(&enabled, context));
        return tidy(findings);
    }

    let mut enabled_by_id: BTreeMap<String, Vec<&ModInfo>> = BTreeMap::new();
    let mut disabled_ids: BTreeSet<String> = BTreeSet::new();
    let mut nested_ids: BTreeSet<String> = BTreeSet::new();
    for module in mods {
        nested_ids.extend(module.nested.iter().map(|id| id.to_ascii_lowercase()));
        for id in std::iter::once(&module.id).chain(module.provides.iter()) {
            let key = id.to_ascii_lowercase();
            if module.enabled {
                enabled_by_id.entry(key).or_default().push(module);
            } else {
                disabled_ids.insert(key);
            }
        }
    }

    for module in &enabled {
        if !module.recognized {
            findings.push(
                Finding::new(Severity::Warning, "unreadable_mod")
                    .subject(&module.name)
                    .param("file", &module.file)
                    .param("loader", context.loader.map(loader_name).unwrap_or("Unknown")),
            );
            continue;
        }
        let Some(instance_loader) = context.loader else {
            continue;
        };
        if module.loader != ModLoader::Unknown && !compatible(instance_loader, module.loader, context.game_version) {
            findings.push(
                Finding::new(Severity::Error, "wrong_loader")
                    .subject(&module.name)
                    .param("loader", loader_name(module.loader))
                    .param("instanceLoader", loader_name(instance_loader))
                    .param("file", &module.file),
            );
            continue;
        }
        let forge_family = matches!(instance_loader, ModLoader::Forge | ModLoader::NeoForge);
        let fabric_module = matches!(module.loader, ModLoader::Fabric | ModLoader::Quilt);
        let game_check = if forge_family && !fabric_module {
            any_matches_series(context.game_version, &module.game_versions)
        } else {
            any_matches(context.game_version, &module.game_versions)
        };
        if game_check == Some(false) {
            findings.push(
                Finding::new(Severity::Error, "game_version_mismatch")
                    .subject(&module.name)
                    .param("required", join(&module.game_versions))
                    .param("actual", context.game_version)
                    .param("file", &module.file),
            );
        } else if module.game_versions.is_empty() {
            // Forge mods rarely declare the game version, and a jar built for
            // another release then fails with "constructed 0 mods". The file
            // name is the only offline clue.
            let named = file_game_versions(&module.file);
            if !named.is_empty() && !named.iter().any(|version| same_build(version, context.game_version)) {
                // A file name is a hint, not metadata: some projects ship one
                // jar for several game versions. Never call this an error.
                findings.push(
                    Finding::new(Severity::Warning, "filename_version")
                        .subject(&module.name)
                        .param("required", named.join(" / "))
                        .param("actual", context.game_version)
                        .param("file", &module.file),
                );
            }
        }
        if let Some(loader_version) = context.loader_version
            && let Some(actual) =
                effective_loader_version(instance_loader, module.loader, loader_version, context.game_version)
            && any_matches(&actual, &enforced_ranges(instance_loader, &module.loader_versions)) == Some(false)
        {
            findings.push(
                Finding::new(Severity::Error, "loader_version_mismatch")
                    .subject(&module.name)
                    .param("loader", loader_name(instance_loader))
                    .param("required", join(&module.loader_versions))
                    .param("actual", actual)
                    .param("file", &module.file),
            );
        }
        if let Some(java) = context.java_major
            && any_matches(&java.to_string(), &module.java_versions) == Some(false)
        {
            findings.push(
                Finding::new(Severity::Error, "java_requirement")
                    .subject(&module.name)
                    .param("required", join_numbers(&module.java_versions))
                    .param("actual", java.to_string())
                    .param("file", &module.file),
            );
        }
        findings.extend(dependencies(
            module,
            &enabled_by_id,
            &disabled_ids,
            context.loader,
            &nested_ids,
        ));
    }

    findings.extend(duplicates(&enabled_by_id, &nested_ids));
    findings.extend(conflicts(&enabled, &enabled_by_id));
    findings.extend(memory_findings(&enabled, context));
    tidy(findings)
}

/// Dependency ids and ranges from the metadata the instance's loader reads: a
/// universal jar that ships Fabric metadata may declare a different set.
fn dependency_sets(module: &ModInfo, loader: Option<ModLoader>) -> (&[String], &BTreeMap<String, Vec<String>>) {
    let use_fabric = matches!(loader, Some(ModLoader::Fabric | ModLoader::Quilt))
        && module.loader == ModLoader::Universal
        && !module.fabric_dependency.is_empty();
    if use_fabric {
        (&module.fabric_dependency[..], &module.fabric_dependency_ranges)
    } else {
        (&module.dependency[..], &module.dependency_ranges)
    }
}

/// Required dependency ids with the version ranges the game enforces, applying
/// the same rules as [`check`]. The dependency updater selects builds from it.
pub fn dependency_requirements(module: &ModInfo, loader: Option<ModLoader>) -> Vec<(String, Vec<String>)> {
    let (dependencies, ranges_by_id) = dependency_sets(module, loader);
    dependencies
        .iter()
        .map(|dependency| {
            let ranges = ranges_by_id.get(dependency).map_or_else(Vec::new, |ranges| {
                enforced_ranges(metadata_loader(module, loader), ranges)
            });
            (dependency.clone(), ranges)
        })
        .collect()
}

fn dependencies(
    module: &ModInfo,
    enabled_by_id: &BTreeMap<String, Vec<&ModInfo>>,
    disabled_ids: &BTreeSet<String>,
    loader: Option<ModLoader>,
    nested_ids: &BTreeSet<String>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut missing = Vec::new();
    let mut disabled = Vec::new();
    let (dependencies, ranges_by_id) = dependency_sets(module, loader);
    for dependency in dependencies {
        let key = dependency.to_ascii_lowercase();
        if let Some(providers) = enabled_by_id.get(&key)
            && let Some(provider) = providers.first()
        {
            // A bundled library is loaded by the loader, not from this jar, so
            // the host jar's version says nothing about it.
            if let Some(ranges) = ranges_by_id.get(dependency)
                && !nested_ids.contains(&key)
                && any_matches(
                    &provider.version,
                    &enforced_ranges(metadata_loader(module, loader), ranges),
                ) == Some(false)
            {
                findings.push(
                    Finding::new(Severity::Error, "dependency_version")
                        .subject(&module.name)
                        .param("dep", &provider.name)
                        .param("required", join(ranges))
                        .param("actual", &provider.version),
                );
            }
        } else if disabled_ids.contains(&key) {
            disabled.push(dependency.clone());
        } else {
            missing.push(dependency.clone());
        }
    }
    if !missing.is_empty() {
        findings.push(
            Finding::new(Severity::Error, "missing_dependency")
                .subject(&module.name)
                .param("deps", missing.join(", ")),
        );
    }
    if !disabled.is_empty() {
        findings.push(
            Finding::new(Severity::Error, "disabled_dependency")
                .subject(&module.name)
                .param("deps", disabled.join(", ")),
        );
    }
    findings
}

fn duplicates(by_id: &BTreeMap<String, Vec<&ModInfo>>, nested_ids: &BTreeSet<String>) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (id, group) in by_id {
        // Several jars bundling the same library is normal; the loader keeps one.
        if nested_ids.contains(id) {
            continue;
        }
        let mut files: Vec<&str> = group.iter().map(|module| module.file.as_str()).collect();
        files.sort_unstable();
        files.dedup();
        if files.len() < 2 {
            continue;
        }
        findings.push(
            Finding::new(Severity::Error, "duplicate_mod")
                .param("id", id)
                .param("count", files.len().to_string())
                .param("names", files.join(", ")),
        );
    }
    findings
}

const KNOWN_CONFLICTS: &[(&str, &str)] = &[
    ("optifine", "sodium"),
    ("optifine", "embeddium"),
    ("optifine", "rubidium"),
    ("optifine", "iris"),
    ("optifine", "oculus"),
    ("sodium", "embeddium"),
    ("sodium", "rubidium"),
    ("iris", "oculus"),
];

fn conflicts(enabled: &[&ModInfo], by_id: &BTreeMap<String, Vec<&ModInfo>>) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();
    for (left, right) in KNOWN_CONFLICTS {
        if by_id.contains_key(*left) && by_id.contains_key(*right) {
            let mut pair = [name_of(by_id, left), name_of(by_id, right)];
            pair.sort();
            pairs.insert((pair[0].clone(), pair[1].clone()));
        }
    }
    for module in enabled {
        for broken in &module.breaks {
            let key = broken.to_ascii_lowercase();
            let Some(other) = by_id.get(&key) else {
                continue;
            };
            if other.iter().all(|entry| entry.file == module.file) {
                continue;
            }
            // `breaks: {"other": "<1.0"}` only conflicts with those builds;
            // a version the predicate cannot parse must not invent a conflict.
            if let Some(ranges) = module.break_ranges.get(broken)
                && other
                    .iter()
                    .all(|entry| any_matches(&entry.version, ranges) == Some(false))
            {
                continue;
            }
            let mut pair = [module.name.clone(), name_of(by_id, &key)];
            pair.sort();
            pairs.insert((pair[0].clone(), pair[1].clone()));
        }
    }
    for (left, right) in pairs {
        findings.push(Finding::new(Severity::Error, "mod_conflict").param("mods", format!("{left} + {right}")));
    }
    findings
}

fn memory_findings(enabled: &[&ModInfo], context: &ModContext<'_>) -> Vec<Finding> {
    let mut findings = Vec::new();
    let count = enabled.len();
    let recommended = match count {
        0..=40 => 2048,
        41..=100 => 3072,
        101..=220 => 4096,
        _ => 6144,
    };
    if context.memory_mb < recommended {
        findings.push(
            Finding::new(Severity::Warning, "memory_low")
                .param("memory", context.memory_mb.to_string())
                .param("count", count.to_string())
                .param("recommended", recommended.to_string()),
        );
    }
    if let Some(total) = context.total_memory_mb {
        let total = total as u32;
        if total >= 8 * 1024 {
            let limit = total.saturating_sub(4 * 1024).max(2048);
            if context.memory_mb > limit {
                findings.push(
                    Finding::new(Severity::Warning, "memory_high")
                        .param("memory", context.memory_mb.to_string())
                        .param("limit", limit.to_string()),
                );
            }
        }
    }
    findings
}

fn name_of(by_id: &BTreeMap<String, Vec<&ModInfo>>, id: &str) -> String {
    by_id
        .get(id)
        .and_then(|group| group.first())
        .map(|module| module.name.clone())
        .unwrap_or_else(|| id.to_string())
}

fn names(mods: &[&ModInfo], max: usize) -> String {
    mods.iter()
        .take(max)
        .map(|module| module.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Minecraft versions embedded in a jar name, e.g. `1.20.1`. Older mod
/// version numbers such as `1.5.5` or `1.36.0` are ignored by only accepting
/// minors 14 through 30.
fn file_game_versions(file: &str) -> Vec<String> {
    let chars: Vec<char> = file.chars().collect();
    let mut found: Vec<String> = Vec::new();
    let mut index = 0;
    while index + 2 < chars.len() {
        let previous_is_digit = index > 0 && chars[index - 1].is_ascii_digit();
        if chars[index] == '1' && chars[index + 1] == '.' && chars[index + 2].is_ascii_digit() && !previous_is_digit {
            let minor_start = index + 2;
            let mut cursor = minor_start;
            while cursor < chars.len() && chars[cursor].is_ascii_digit() && cursor - minor_start < 2 {
                cursor += 1;
            }
            let minor: u32 = chars[minor_start..cursor]
                .iter()
                .collect::<String>()
                .parse()
                .unwrap_or(0);
            if (14..=30).contains(&minor) {
                if cursor + 1 < chars.len() && chars[cursor] == '.' && chars[cursor + 1].is_ascii_digit() {
                    cursor += 1;
                    while cursor < chars.len() && chars[cursor].is_ascii_digit() {
                        cursor += 1;
                    }
                }
                let version: String = chars[index..cursor].iter().collect();
                if !found.contains(&version) {
                    found.push(version);
                }
                index = cursor;
                continue;
            }
        }
        index += 1;
    }
    found
}

/// `1.20` and `1.20.1` describe the same line; `1.20.4` does not.
fn same_build(candidate: &str, actual: &str) -> bool {
    candidate == actual || candidate.starts_with(&format!("{actual}.")) || actual.starts_with(&format!("{candidate}."))
}

fn join(values: &[String]) -> String {
    values.join(" / ")
}

fn join_numbers(values: &[String]) -> String {
    values
        .iter()
        .map(|value| value.trim_start_matches(">=").trim().to_string())
        .collect::<Vec<_>>()
        .join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(id: &str, name: &str, version: &str, loader: ModLoader) -> ModInfo {
        ModInfo {
            file: format!("{id}.jar"),
            id: id.to_string(),
            name: name.to_string(),
            version: version.to_string(),
            loader,
            enabled: true,
            size: 1,
            modified: 0,
            missing_dependencies: Vec::new(),
            disabled_dependencies: Vec::new(),
            dependency: Vec::new(),
            duplicate: false,
            recognized: true,
            provides: Vec::new(),
            nested: Vec::new(),
            game_versions: Vec::new(),
            loader_versions: Vec::new(),
            java_versions: Vec::new(),
            breaks: Vec::new(),
            break_ranges: BTreeMap::new(),
            dependency_ranges: BTreeMap::new(),
            fabric_dependency: Vec::new(),
            fabric_dependency_ranges: BTreeMap::new(),
            blocked: None,
        }
    }

    fn context() -> ModContext<'static> {
        ModContext {
            loader: Some(ModLoader::NeoForge),
            game_version: "1.21.1",
            loader_version: Some("21.1.80"),
            java_major: Some(21),
            memory_mb: 4096,
            total_memory_mb: Some(16 * 1024),
        }
    }

    fn codes(mods: &[ModInfo], ctx: &ModContext<'_>) -> Vec<String> {
        check(mods, ctx).into_iter().map(|finding| finding.code).collect()
    }

    #[test]
    fn reports_disabled_jars_the_instance_can_never_load() {
        let mut example = module("epicfight", "Epic Fight", "20.14.17", ModLoader::Forge);
        example.enabled = false;
        example.file = "epic-fight.jar.disabled".to_string();
        example.loader_versions = vec!["[47.4.0,)".to_string()];
        example.blocked = Some(tntl_instance::mods::BlockedReason {
            kind: "loader".to_string(),
            loader: "NeoForge".to_string(),
            required: vec!["[47.4.0,)".to_string()],
            actual: "47.1.106".to_string(),
        });
        let mut ctx = context();
        ctx.game_version = "1.20.1";
        ctx.loader_version = Some("1.20.1-47.1.106");
        let findings = check(&[example], &ctx);
        let blocked: Vec<&Finding> = findings
            .iter()
            .filter(|finding| finding.code == "blocked_mod")
            .collect();
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].params.get("actual").map(String::as_str), Some("47.1.106"));
    }

    #[test]
    fn flags_a_fabric_mod_in_a_neoforge_instance() {
        let mods = vec![module("sodium", "Sodium", "0.6.0", ModLoader::Fabric)];
        assert!(codes(&mods, &context()).contains(&"wrong_loader".to_string()));
    }

    #[test]
    fn neoforge_1_20_1_accepts_forge_mods() {
        let mut ctx = context();
        ctx.game_version = "1.20.1";
        ctx.loader_version = Some("1.20.1-47.1.106");
        let mut example = module("example", "Example", "1.0", ModLoader::Forge);
        example.game_versions = vec!["[1.20.1,1.21)".to_string()];
        example.loader_versions = vec!["[47,)".to_string()];
        assert!(codes(&[example], &ctx).is_empty());
    }

    #[test]
    fn sloppy_neoforge_metadata_is_not_reported() {
        // 1.21.1 packs ship JEI with `[1.21, 1.21.1)` and mods that name a
        // bare loader build (`21.1.215`); both run, the loader only enforces
        // bracketed ranges.
        let mut game = module("jei", "JEI", "19.64.0.454", ModLoader::NeoForge);
        game.game_versions = vec!["[1.21, 1.21.1)".to_string()];
        game.loader_versions = vec!["[21.1.238,)".to_string()];
        let mut loader = module("constructionwand", "Construction Wands", "4.2.2", ModLoader::NeoForge);
        loader.game_versions = vec!["1.21.1".to_string()];
        loader.loader_versions = vec!["21.1.215".to_string()];
        let mut ctx = context();
        ctx.loader_version = Some("21.1.248");
        assert!(codes(&[game, loader], &ctx).is_empty());
    }

    #[test]
    fn flags_game_and_loader_ranges() {
        let mut example = module("example", "Example", "1.0", ModLoader::NeoForge);
        example.game_versions = vec!["[1.20.1,1.21)".to_string()];
        example.loader_versions = vec!["[21.2,)".to_string()];
        let codes = codes(&[example], &context());
        assert!(codes.contains(&"game_version_mismatch".to_string()));
        assert!(codes.contains(&"loader_version_mismatch".to_string()));
    }

    #[test]
    fn flags_a_jar_named_for_another_game_version() {
        let mut ctx = context();
        ctx.game_version = "1.20.1";
        let mut example = module(
            "immediatelyfast",
            "ImmediatelyFast",
            "1.5.5+1.20.4",
            ModLoader::NeoForge,
        );
        example.file = "ImmediatelyFast-NeoForge-1.5.5+1.20.4.jar".to_string();
        let findings = check(&[example], &ctx);
        assert!(findings.iter().any(|finding| {
            finding.code == "filename_version"
                && finding.severity == Severity::Warning
                && finding
                    .params
                    .get("required")
                    .is_some_and(|value| value.contains("1.20.4"))
        }));
    }

    #[test]
    fn jar_name_versions_ignore_mod_versions() {
        assert_eq!(file_game_versions("example-1.5.5+1.20.1.jar"), vec!["1.20.1"]);
        assert_eq!(file_game_versions("example-1.36.0_Forge_1.20.jar"), vec!["1.20"]);
        assert!(file_game_versions("ferritecore-6.0.1-forge.jar").is_empty());
        assert!(same_build("1.20", "1.20.1"));
        assert!(!same_build("1.20.4", "1.20.1"));
    }

    #[test]
    fn finds_missing_and_disabled_dependencies() {
        let mut example = module("example", "Example", "1.0", ModLoader::NeoForge);
        example.dependency = vec!["absent".to_string(), "off".to_string()];
        let mut disabled = module("off", "Off", "1.0", ModLoader::NeoForge);
        disabled.enabled = false;
        let codes = codes(&[example, disabled], &context());
        assert!(codes.contains(&"missing_dependency".to_string()));
        assert!(codes.contains(&"disabled_dependency".to_string()));
    }

    #[test]
    fn forge_loader_versions_drop_the_game_prefix() {
        let mut ctx = context();
        ctx.loader = Some(ModLoader::Forge);
        ctx.game_version = "1.20.1";
        ctx.loader_version = Some("1.20.1-47.4.28");
        let mut example = module("epicfight", "Epic Fight", "20.14.17", ModLoader::Forge);
        example.loader_versions = vec!["[47.4.0,)".to_string()];
        assert!(codes(&[example], &ctx).is_empty());

        let mut older = module("older", "Older", "1.0", ModLoader::Forge);
        older.loader_versions = vec!["[47.5,)".to_string()];
        assert!(codes(&[older], &ctx).contains(&"loader_version_mismatch".to_string()));
    }

    #[test]
    fn forge_soft_version_ranges_are_not_enforced() {
        let mut ctx = context();
        ctx.loader = Some(ModLoader::Forge);
        ctx.game_version = "1.20.1";
        ctx.loader_version = Some("1.20.1-47.4.28");
        let mut kacchi = module("kacchi", "Kacchi", "2.0.0", ModLoader::Forge);
        kacchi.dependency = vec!["farmersdelight".to_string()];
        kacchi
            .dependency_ranges
            .insert("farmersdelight".to_string(), vec!["1.20.1-1.2.7".to_string()]);
        let mut delight = module("farmersdelight", "Farmer's Delight", "1.20.1-1.3.4", ModLoader::Forge);
        delight.loader_versions = vec!["[47,)".to_string()];
        assert!(codes(&[kacchi, delight], &ctx).is_empty());
    }

    #[test]
    fn bundled_libraries_are_not_version_checked() {
        let mut ctx = context();
        ctx.loader = Some(ModLoader::Forge);
        ctx.game_version = "1.20.1";
        ctx.loader_version = Some("1.20.1-47.4.28");
        let mut create = module("create", "Create", "6.0.8", ModLoader::Forge);
        create.nested = vec!["flywheel".to_string(), "mixinextras".to_string()];
        create.provides = create.nested.clone();
        create.dependency = vec!["flywheel".to_string()];
        create
            .dependency_ranges
            .insert("flywheel".to_string(), vec!["[1.0.0,2.0)".to_string()]);
        let mut other = module("other", "Other", "1.0", ModLoader::Forge);
        other.nested = vec!["mixinextras".to_string()];
        other.provides = other.nested.clone();
        assert!(codes(&[create, other], &ctx).is_empty());
    }

    #[test]
    fn checks_dependency_versions() {
        let mut example = module("example", "Example", "1.0", ModLoader::NeoForge);
        example.dependency = vec!["library".to_string()];
        example
            .dependency_ranges
            .insert("library".to_string(), vec!["[2,)".to_string()]);
        let library = module("library", "Library", "1.0", ModLoader::NeoForge);
        assert!(codes(&[example, library], &context()).contains(&"dependency_version".to_string()));
    }

    #[test]
    fn known_conflicts_and_duplicates() {
        let mut optifine = module("optifine", "OptiFine", "1.0", ModLoader::NeoForge);
        optifine.breaks = vec!["sodium".to_string()];
        let sodium = module("sodium", "Sodium", "0.6", ModLoader::NeoForge);
        let mut again = sodium.clone();
        again.file = "sodium-2.jar".to_string();
        let codes = codes(&[optifine, sodium, again], &context());
        assert!(codes.contains(&"duplicate_mod".to_string()));
        assert!(codes.contains(&"mod_conflict".to_string()));
    }

    #[test]
    fn memory_advice_scales_with_mod_count() {
        let mut ctx = context();
        ctx.memory_mb = 1024;
        assert!(
            codes(&[module("example", "Example", "1.0", ModLoader::NeoForge)], &ctx)
                .contains(&"memory_low".to_string())
        );
    }
}
