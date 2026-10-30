use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tntl_launch::{CrashKind, LaunchOptions};
use tntl_modrinth::InstallEvent as ModEvent;
use tntl_resolve::{LaunchPlan, MissingKind, VerifyMode};
use tntl_source::{InstallEvent, LoaderKind, endpoints};

use tntl_app::{ContentSource, InstallRequest, Launcher, PackEvent, ProjectKind, list_instances};

#[derive(Parser)]
#[command(name = "tntl", version = env!("CARGO_PKG_VERSION_MAJOR"), about = "Minecraft launcher core (Windows and Linux)")]
struct Cli {
    /// Launcher data directory.
    #[arg(long, global = true, env = "TNTL_ROOT")]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List Java runtimes, or download one from Mojang.
    Java {
        /// List the runtimes Mojang publishes for this platform.
        #[arg(long)]
        offers: bool,
        /// Download a runtime by component, e.g. `java-runtime-gamma`.
        #[arg(long, value_name = "COMPONENT")]
        install: Option<String>,
    },
    /// List versions published by Mojang.
    Versions {
        /// `release`, `snapshot` (releases and snapshots) or `all`
        /// (also alpha and beta builds).
        #[arg(long, default_value = "release", value_name = "CHANNEL")]
        channel: String,
        #[arg(long, default_value_t = 25)]
        limit: usize,
    },
    /// List accounts, or add and manage offline ones.
    Accounts {
        /// Add an offline account and make it the active one.
        #[arg(long, value_name = "NAME")]
        add_offline: Option<String>,
        /// Make this account id active.
        #[arg(long = "use", value_name = "ID")]
        use_account: Option<String>,
        /// Remove an account id.
        #[arg(long, value_name = "ID")]
        remove: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Sign in with a Microsoft account through the device code flow.
    Login {
        /// Public Entra client id; defaults to the launcher setting.
        #[arg(long, value_name = "CLIENT_ID")]
        client_id: Option<String>,
    },
    /// Install missing mod dependencies from Modrinth.
    Deps {
        instance: String,
        /// Replace installed dependencies that sit outside a required range.
        #[arg(long)]
        fix_versions: bool,
    },
    /// Query a server's status page.
    Ping {
        /// `host` or `host:port`.
        address: String,
    },
    /// Search Modrinth modpacks, or install one by project.
    Packs {
        query: Option<String>,
        /// Install this project id or slug.
        #[arg(long, value_name = "PROJECT")]
        install: Option<String>,
        /// Instance name for the install.
        #[arg(long)]
        name: Option<String>,
        /// Ordering: relevance, downloads, follows, newest or updated.
        #[arg(long, default_value = "downloads", value_name = "SORT")]
        sort: String,
        #[arg(long, default_value_t = 15)]
        limit: usize,
    },
    /// Show or change an account's skin.
    Skin {
        /// Account id; `-` means the active account.
        account: String,
        /// PNG file to store (and upload for Microsoft accounts).
        #[arg(long, value_name = "FILE")]
        set: Option<PathBuf>,
        /// `wide` or `slim`.
        #[arg(long, value_name = "MODEL")]
        model: Option<String>,
        /// `on` or `off`: paint an offline skin into the game.
        #[arg(long, value_name = "on|off")]
        apply: Option<String>,
        /// Remove the skin.
        #[arg(long)]
        clear: bool,
        /// Use the skin a nickname publishes online (Mojang, then ely.by).
        #[arg(long, value_name = "NICK")]
        from_online: Option<String>,
        /// Use the skin a nickname publishes on ely.by.
        #[arg(long, value_name = "NICK")]
        from_ely: Option<String>,
        /// Use the skin a premium nickname publishes on Mojang.
        #[arg(long, value_name = "NICK")]
        from_mojang: Option<String>,
        /// Look a nickname up without changing anything.
        #[arg(long, value_name = "NICK")]
        lookup: Option<String>,
        /// Which service `--lookup` asks: `auto`, `mojang` or `ely`.
        #[arg(long, default_value = "auto", value_name = "SOURCE")]
        source: String,
        /// Use one of the launcher's built-in skins.
        #[arg(long, value_name = "KEY")]
        builtin: Option<String>,
    },
    /// Browse the public skin gallery.
    Skins {
        /// Category: `latest`, `top`, `staff`, `slim`, `hd` or `random`.
        #[arg(long, default_value = "latest", value_name = "CATEGORY")]
        filter: String,
        /// Cursor from the previous page (the uuid it printed).
        #[arg(long, value_name = "CURSOR")]
        after: Option<String>,
        #[arg(long, default_value_t = 16)]
        count: usize,
        #[arg(long)]
        json: bool,
    },
    /// Install a vanilla version, or a version with a mod loader on top.
    Install {
        version: String,
        /// Mod loader to install: `fabric`, `quilt`, `legacyfabric`, `forge` or `neoforge`.
        #[arg(long, value_name = "LOADER")]
        loader: Option<String>,
        /// Loader build; defaults to the newest stable one.
        #[arg(long)]
        loader_version: Option<String>,
        /// Skip the asset objects (the game will miss sounds and languages).
        #[arg(long)]
        no_assets: bool,
        /// Do not create an instance after installing.
        #[arg(long)]
        no_instance: bool,
        /// Instance name (defaults to the version id, or `<version>-<loader>`).
        #[arg(long)]
        name: Option<String>,
    },
    /// List loader builds published for a game version.
    Loaders {
        version: String,
        #[arg(long, default_value = "fabric", value_name = "LOADER")]
        loader: String,
        #[arg(long, default_value_t = 15)]
        limit: usize,
    },
    /// Show, change or update the mod loader of an instance.
    Loader {
        instance: String,
        #[command(subcommand)]
        action: Option<LoaderAction>,
    },
    /// Browse and install FTB modpacks from the official FTB catalogue.
    Ftb {
        /// Search term; empty lists the newest packs.
        query: Option<String>,
        /// Show this pack with its version history.
        #[arg(long)]
        pack: Option<u64>,
        /// Install this pack.
        #[arg(long)]
        install: Option<u64>,
        /// FTB version id to install; defaults to the newest build.
        #[arg(long)]
        version: Option<u64>,
        /// Instance name; defaults to the pack name.
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Browse and install CurseForge modpacks (needs an API key).
    Cfpack {
        /// Search term; empty lists the most downloaded packs.
        query: Option<String>,
        /// Install this pack (project id).
        #[arg(long)]
        install: Option<u64>,
        /// CurseForge file id to install; defaults to the newest release.
        #[arg(long)]
        file: Option<u64>,
        /// Instance name; defaults to the pack name.
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "downloads")]
        sort: String,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Register an existing .minecraft directory as an instance (Prism and
    /// MultiMC instance folders are detected and installed).
    Import {
        path: PathBuf,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        version: Option<String>,
        /// Only list the versions found in the directory.
        #[arg(long)]
        list: bool,
    },
    /// Rename an instance (moves its directory).
    Rename { instance: String, name: String },
    /// Remove an instance from the library; the files stay on disk.
    Delete {
        instance: String,
        /// Delete without asking.
        #[arg(long)]
        yes: bool,
        /// Also delete version files that no other instance uses.
        #[arg(long)]
        cleanup: bool,
    },
    /// List instances.
    List,
    /// List versions installed on this machine, or delete them.
    Installed {
        /// Delete one version that no instance uses.
        #[arg(long, value_name = "ID")]
        delete: Option<String>,
        /// Delete every installed version no instance uses.
        #[arg(long)]
        cleanup: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print the launch plan of an instance.
    Plan {
        instance: String,
        #[arg(long)]
        json: bool,
    },
    /// Launch an instance.
    Launch {
        instance: String,
        #[arg(long)]
        dry_run: bool,
        /// Kill the game after this many seconds.
        #[arg(long)]
        timeout: Option<u64>,
        /// Mirror game output to the terminal.
        #[arg(long)]
        echo: bool,
        /// Use this Java executable.
        #[arg(long)]
        java: Option<PathBuf>,
        /// Start the trial world for this run.
        #[arg(long)]
        demo: bool,
    },
    /// Verify every file an instance needs.
    Verify { instance: String },
    /// Re-download missing or corrupt files of an instance and its modpack.
    Repair {
        instance: String,
        #[arg(long)]
        json: bool,
    },
    /// List an instance's mods, or enable/disable one.
    Mods {
        instance: String,
        /// Disable this mod (file name, with or without `.disabled`).
        #[arg(long)]
        disable: Option<String>,
        /// Enable this mod.
        #[arg(long)]
        enable: Option<String>,
        /// Delete this mod file.
        #[arg(long)]
        remove: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List, back up, restore or delete worlds.
    Worlds {
        instance: String,
        /// Zip this world into `<instance>/backups/worlds`.
        #[arg(long, value_name = "WORLD")]
        backup: Option<String>,
        /// Restore a world from a zip or one of this instance's backups.
        #[arg(long, value_name = "ARCHIVE")]
        import: Option<PathBuf>,
        /// Delete this world.
        #[arg(long, value_name = "WORLD")]
        delete: Option<String>,
        /// Copy this world inside the instance.
        #[arg(long, value_name = "WORLD")]
        duplicate: Option<String>,
        /// Delete one backup archive.
        #[arg(long, value_name = "FILE")]
        delete_backup: Option<String>,
        /// List world backups instead of worlds.
        #[arg(long)]
        backups: bool,
        /// Skip the delete confirmation.
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// List a world's datapacks, or switch, add and remove them.
    Datapacks {
        instance: String,
        /// World directory under `saves/`; defaults to the most recent one.
        #[arg(long)]
        world: Option<String>,
        /// Switch this datapack on.
        #[arg(long)]
        enable: Option<String>,
        /// Switch this datapack off (it is moved out of the world).
        #[arg(long)]
        disable: Option<String>,
        /// Delete this datapack.
        #[arg(long)]
        delete: Option<String>,
        /// Copy this zip into the world.
        #[arg(long, value_name = "FILE")]
        add: Option<PathBuf>,
        /// Delete without asking.
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Install a local jar or zip into an instance.
    AddFile {
        instance: String,
        path: PathBuf,
        /// `mod`, `resourcepack` or `shader`.
        #[arg(long, default_value = "mod", value_name = "KIND")]
        kind: String,
    },
    /// Copy an instance, including its game directory, under a new name.
    Duplicate { instance: String, name: String },
    /// Export an instance as a portable `.mrpack`.
    Export {
        instance: String,
        /// Output file; defaults to `<instance>.mrpack`.
        #[arg(long, short)]
        output: Option<PathBuf>,
        /// Pack name shown by other launchers.
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "1.0.0")]
        version: String,
    },
    /// Search Modrinth for mods that fit an instance.
    Search {
        query: String,
        /// Instance whose game version and loader filter the results.
        #[arg(long)]
        instance: Option<String>,
        /// Game version, when no instance is given.
        #[arg(long)]
        version: Option<String>,
        /// Loader key, when no instance is given: `fabric`, `quilt`, `legacyfabric`, `forge` or `neoforge`.
        #[arg(long)]
        loader: Option<String>,
        #[arg(long, default_value = "mod", value_name = "KIND")]
        kind: String,
        /// Ordering: relevance, downloads, follows, newest or updated.
        #[arg(long, default_value = "relevance", value_name = "SORT")]
        sort: String,
        /// Catalogue: `modrinth` or `curseforge`.
        #[arg(long, default_value = "modrinth", value_name = "SOURCE")]
        source: String,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[arg(long, default_value_t = 0)]
        offset: u32,
        #[arg(long)]
        json: bool,
    },
    /// Install a Modrinth project and its required dependencies into an instance.
    Add {
        /// Project id or slug.
        project: String,
        instance: String,
        /// `mod`, `resourcepack` or `shader`.
        #[arg(long, default_value = "mod", value_name = "KIND")]
        kind: String,
        /// Catalogue: `modrinth` or `curseforge`.
        #[arg(long, default_value = "modrinth", value_name = "SOURCE")]
        source: String,
        /// Install this exact build instead of the newest release (Modrinth).
        #[arg(long = "build", value_name = "VERSION_ID")]
        version: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Check installed mods, resource packs or shaders for newer builds.
    Updates {
        instance: String,
        /// `mod`, `resourcepack` or `shader`.
        #[arg(long, default_value = "mod", value_name = "KIND")]
        kind: String,
        /// Download and install the newer files.
        #[arg(long)]
        apply: bool,
        /// Only update these files (defaults to everything outdated).
        #[arg(long = "file", value_name = "FILE")]
        files: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// List resource packs or shaders, or switch one on/off.
    Content {
        instance: String,
        /// `resourcepack` or `shader`.
        #[arg(long, default_value = "resourcepack", value_name = "KIND")]
        kind: String,
        /// Switch this file on.
        #[arg(long)]
        enable: Option<String>,
        /// Switch this file off.
        #[arg(long)]
        disable: Option<String>,
        /// Delete this file from the instance.
        #[arg(long, value_name = "FILE")]
        delete: Option<String>,
        /// Move this resource pack one slot up, so it overrides more packs.
        #[arg(long, value_name = "FILE")]
        up: Option<String>,
        /// Move this resource pack one slot down.
        #[arg(long, value_name = "FILE")]
        down: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Check a Modrinth modpack instance for a newer build, or install one.
    Pack {
        instance: String,
        /// Install the newest build (prefers the one that keeps the game version).
        #[arg(long)]
        apply: bool,
        /// Install this pack version instead of the newest one.
        #[arg(long, value_name = "VERSION_ID")]
        version: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Install a Modrinth modpack (`.mrpack`) as a new instance.
    Modpack {
        /// Path to the .mrpack file.
        path: PathBuf,
        /// Instance name; defaults to the pack name.
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Write an instance directory as a portable `.zip` archive.
    Archive {
        instance: String,
        /// Output file; defaults to `<instance>.zip`.
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
    /// Register a `.zip` instance archive as a new instance.
    ImportArchive {
        archive: PathBuf,
        /// Instance name; defaults to the archive's name.
        #[arg(long)]
        name: Option<String>,
    },
    /// List or edit the multiplayer server list.
    Servers {
        instance: String,
        /// Add a server as `Name|host:port`.
        #[arg(long, value_name = "NAME|ADDRESS")]
        add: Option<String>,
        /// Remove a server by its 0-based index.
        #[arg(long)]
        remove: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// List, show or delete crash reports.
    Crashes {
        instance: String,
        /// Print one report.
        #[arg(long, value_name = "FILE")]
        show: Option<String>,
        /// Delete one report.
        #[arg(long, value_name = "FILE")]
        delete: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// List or delete screenshots.
    Screenshots {
        instance: String,
        #[arg(long, value_name = "FILE")]
        delete: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show how much disk space an instance and the shared files use.
    Disk { instance: String },
    /// Remove logs, crash reports and extracted natives.
    Cleanup {
        instance: String,
        /// Delete without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Change the library metadata of an instance.
    Meta {
        instance: String,
        #[arg(long)]
        group: Option<String>,
        #[arg(long)]
        notes: Option<String>,
        /// Image used as the instance icon.
        #[arg(long, value_name = "IMAGE")]
        icon: Option<PathBuf>,
        /// Remove the custom icon.
        #[arg(long)]
        clear_icon: bool,
    },
    /// Show environment and installation information.
    Doctor,
    /// Check an instance's mods, Java and files without launching it.
    Check {
        instance: String,
        #[arg(long)]
        json: bool,
    },
    /// Analyze a Minecraft log or crash report.
    Analyze {
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum LoaderAction {
    /// Install a loader, or switch to another one.
    Install {
        /// `fabric`, `quilt`, `legacyfabric`, `forge` or `neoforge`.
        loader: String,
        /// Loader build; defaults to the newest stable one.
        #[arg(long)]
        version: Option<String>,
    },
    /// Remove the loader and launch vanilla.
    Remove,
    /// Check whether a newer loader build is published.
    Check,
}

/// Data directory the GUI picked (`bootstrap.json`). The command line and the
/// desktop app must agree on where the library is.
fn configured_root() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = dirs::data_local_dir();
    #[cfg(not(windows))]
    let base = dirs::config_dir();
    let path = base?.join("tntl").join("bootstrap.json");
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let root = value.get("root")?.as_str()?.to_string();
    let path = PathBuf::from(root);
    path.is_dir().then_some(path)
}

fn main() {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("error: {error:#}");
            std::process::exit(1);
        }
    }
}

fn run(cli: Cli) -> Result<i32> {
    let root = cli.root.or_else(configured_root);
    let launcher = Launcher::from_root(root);
    match cli.command {
        Command::Java { offers, install } => cmd_java(&launcher, offers, install),
        Command::Versions { channel, limit } => cmd_versions(&channel, limit),
        Command::Accounts {
            add_offline,
            use_account,
            remove,
            json,
        } => cmd_accounts(&launcher, add_offline, use_account, remove, json),
        Command::Login { client_id } => cmd_login(&launcher, client_id.as_deref()),
        Command::Ping { address } => cmd_ping(&address),
        Command::Deps { instance, fix_versions } => cmd_deps(&launcher, &instance, fix_versions),
        Command::Packs {
            query,
            install,
            name,
            sort,
            limit,
        } => cmd_packs(&launcher, query.as_deref(), install.as_deref(), name, &sort, limit),
        Command::Ftb {
            query,
            pack,
            install,
            version,
            name,
            limit,
        } => cmd_ftb(&launcher, query.as_deref(), pack, install, version, name, limit),
        Command::Cfpack {
            query,
            install,
            file,
            name,
            sort,
            limit,
        } => cmd_cfpack(&launcher, query.as_deref(), install, file, name, &sort, limit),
        Command::Skin {
            account,
            set,
            model,
            apply,
            clear,
            from_online,
            from_ely,
            from_mojang,
            lookup,
            source,
            builtin,
        } => cmd_skin(
            &launcher,
            &account,
            SkinArgs {
                set,
                model: model.as_deref(),
                apply: apply.as_deref(),
                clear,
                from_online: from_online.as_deref(),
                from_ely: from_ely.as_deref(),
                from_mojang: from_mojang.as_deref(),
                lookup: lookup.as_deref(),
                source: &source,
                builtin: builtin.as_deref(),
            },
        ),
        Command::Skins {
            filter,
            after,
            count,
            json,
        } => cmd_skins(&launcher, &filter, after.as_deref(), count, json),
        Command::Install {
            version,
            loader,
            loader_version,
            no_assets,
            no_instance,
            name,
        } => cmd_install(
            &launcher,
            &version,
            loader.as_deref(),
            loader_version,
            no_assets,
            no_instance,
            name,
        ),
        Command::Loaders { version, loader, limit } => cmd_loaders(&launcher, &version, &loader, limit),
        Command::Loader { instance, action } => cmd_loader(&launcher, &instance, action),
        Command::Import {
            path,
            name,
            version,
            list,
        } => cmd_import(&launcher, &path, name, version, list),
        Command::Rename { instance, name } => cmd_rename(&launcher, &instance, &name),
        Command::Delete { instance, yes, cleanup } => cmd_delete(&launcher, &instance, yes, cleanup),
        Command::Installed { delete, cleanup, json } => cmd_installed(&launcher, delete.as_deref(), cleanup, json),
        Command::List => cmd_list(&launcher),
        Command::Plan { instance, json } => cmd_plan(&launcher, &instance, json),
        Command::Launch {
            instance,
            dry_run,
            timeout,
            echo,
            java,
            demo,
        } => cmd_launch(&launcher, &instance, dry_run, timeout, echo, java.as_deref(), demo),
        Command::Verify { instance } => cmd_verify(&launcher, &instance),
        Command::Repair { instance, json } => cmd_repair(&launcher, &instance, json),
        Command::Mods {
            instance,
            disable,
            enable,
            remove,
            json,
        } => cmd_mods(&launcher, &instance, disable, enable, remove, json),
        Command::Worlds {
            instance,
            backup,
            import,
            delete,
            duplicate,
            delete_backup,
            backups,
            yes,
            json,
        } => cmd_worlds(
            &launcher,
            &instance,
            backup,
            import,
            delete,
            duplicate,
            delete_backup,
            backups,
            yes,
            json,
        ),
        Command::Datapacks {
            instance,
            world,
            enable,
            disable,
            delete,
            add,
            yes,
            json,
        } => cmd_datapacks(&launcher, &instance, world, enable, disable, delete, add, yes, json),
        Command::AddFile { instance, path, kind } => cmd_add_file(&launcher, &instance, &path, &kind),
        Command::Duplicate { instance, name } => cmd_duplicate(&launcher, &instance, &name),
        Command::Export {
            instance,
            output,
            name,
            version,
        } => cmd_export(&launcher, &instance, output, name, &version),
        Command::Search {
            query,
            instance,
            version,
            loader,
            kind,
            sort,
            source,
            limit,
            offset,
            json,
        } => cmd_search(
            &launcher,
            SearchArgs {
                query: &query,
                instance: instance.as_deref(),
                version: version.as_deref(),
                loader: loader.as_deref(),
                kind: &kind,
                sort: &sort,
                source: &source,
                limit,
                offset,
                json,
            },
        ),
        Command::Add {
            project,
            instance,
            kind,
            source,
            version,
            json,
        } => cmd_add(&launcher, &project, &instance, &kind, &source, version.as_deref(), json),
        Command::Updates {
            instance,
            kind,
            apply,
            files,
            json,
        } => cmd_updates(&launcher, &instance, &kind, apply, &files, json),
        Command::Content {
            instance,
            kind,
            enable,
            disable,
            delete,
            up,
            down,
            json,
        } => cmd_content(&launcher, &instance, &kind, enable, disable, delete, up, down, json),
        Command::Pack {
            instance,
            apply,
            version,
            json,
        } => cmd_pack(&launcher, &instance, apply, version, json),
        Command::Modpack { path, name, json } => cmd_modpack(&launcher, &path, name, json),
        Command::Archive { instance, output } => cmd_archive(&launcher, &instance, output),
        Command::ImportArchive { archive, name } => cmd_import_archive(&launcher, &archive, name),
        Command::Servers {
            instance,
            add,
            remove,
            json,
        } => cmd_servers(&launcher, &instance, add, remove, json),
        Command::Crashes {
            instance,
            show,
            delete,
            json,
        } => cmd_crashes(&launcher, &instance, show, delete, json),
        Command::Screenshots { instance, delete, json } => cmd_screenshots(&launcher, &instance, delete, json),
        Command::Disk { instance } => cmd_disk(&launcher, &instance),
        Command::Cleanup { instance, yes } => cmd_cleanup(&launcher, &instance, yes),
        Command::Meta {
            instance,
            group,
            notes,
            icon,
            clear_icon,
        } => cmd_meta(&launcher, &instance, group, notes, icon, clear_icon),
        Command::Doctor => cmd_doctor(&launcher),
        Command::Check { instance, json } => cmd_check(&launcher, &instance, json),
        Command::Analyze { path, json } => cmd_analyze(&path, json),
    }
}

fn cmd_check(launcher: &Launcher, instance: &str, json: bool) -> Result<i32> {
    let report = launcher.diagnose_instance(instance, tntl_app::system_memory_mb())?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(exit_for(&report.findings));
    }
    let loader = report
        .loader
        .as_deref()
        .map(|kind| format!("{kind} {}", report.loader_version.as_deref().unwrap_or("")))
        .unwrap_or_else(|| "vanilla".to_string());
    println!(
        "{instance}: {} · {} · {} mod(s), {} MB, Java {}",
        report.game_version,
        loader.trim_end(),
        report.enabled_mods,
        report.memory_mb,
        report
            .java_major
            .map(|major| major.to_string())
            .unwrap_or_else(|| "?".to_string()),
    );
    if report.findings.is_empty() {
        println!("no problems found");
        return Ok(0);
    }
    for finding in &report.findings {
        print_finding(finding);
    }
    Ok(exit_for(&report.findings))
}

fn cmd_analyze(path: &Path, json: bool) -> Result<i32> {
    if !path.is_file() {
        bail!("{} is not a file", path.display());
    }
    let text = tntl_launch::read_tail(path, 8 * 1024 * 1024);
    let analysis = tntl_doctor::log::analyze(&text);
    if json {
        println!("{}", serde_json::to_string_pretty(&analysis)?);
        return Ok(exit_for(&analysis.findings));
    }
    println!(
        "{} line(s): {} info, {} warn, {} error, {} debug",
        analysis.counts.total, analysis.counts.info, analysis.counts.warn, analysis.counts.error, analysis.counts.debug,
    );
    if analysis.findings.is_empty() {
        println!("no known problems found");
        return Ok(0);
    }
    for finding in &analysis.findings {
        print_finding(finding);
    }
    Ok(exit_for(&analysis.findings))
}

fn print_finding(finding: &tntl_doctor::Finding) {
    let severity = match finding.severity {
        tntl_doctor::Severity::Error => "error",
        tntl_doctor::Severity::Warning => "warning",
        tntl_doctor::Severity::Info => "info",
    };
    let mut line = format!("[{severity}] {}", finding.code);
    if let Some(subject) = &finding.subject {
        line.push_str(": ");
        line.push_str(subject);
    }
    if let Some(index) = finding.line {
        line.push_str(&format!(" (line {})", index + 1));
    }
    println!("{line}");
    for (key, value) in &finding.params {
        println!("  {key}: {value}");
    }
    if let Some(evidence) = &finding.evidence {
        println!("  > {evidence}");
    }
}

fn exit_for(findings: &[tntl_doctor::Finding]) -> i32 {
    if findings
        .iter()
        .any(|finding| finding.severity == tntl_doctor::Severity::Error)
    {
        1
    } else {
        0
    }
}

fn cmd_java(launcher: &Launcher, offers: bool, install: Option<String>) -> Result<i32> {
    if offers {
        for offer in launcher.java_offers()? {
            let released = offer.released.unwrap_or_default();
            println!("{:<24} {:<12} {}", offer.component, offer.version, released);
        }
        return Ok(0);
    }
    if let Some(component) = install {
        let runtime = launcher.install_java_runtime(&component, &|event: tntl_app::RuntimeEvent| match event {
            tntl_app::RuntimeEvent::Manifest {
                component,
                version,
                files,
            } => println!("{component} {version}: {files} files"),
            tntl_app::RuntimeEvent::Installed { path, bytes } => {
                println!("  {path} ({:.1} MiB)", bytes as f64 / (1024.0 * 1024.0));
            }
            tntl_app::RuntimeEvent::Downloading { .. } | tntl_app::RuntimeEvent::Skipped { .. } => {}
        })?;
        if runtime.skipped {
            println!("{} {} is already installed", runtime.component, runtime.version);
        } else {
            println!(
                "installed {} {}: {} files, {:.1} MiB",
                runtime.component,
                runtime.version,
                runtime.files,
                runtime.bytes as f64 / (1024.0 * 1024.0)
            );
        }
        println!("java: {}", runtime.java.display());
        return Ok(0);
    }
    let installs = launcher.java_list();
    if installs.is_empty() {
        println!("no Java runtime found");
        return Ok(0);
    }
    for install in installs {
        println!(
            "Java {}  {}  {}",
            install.major,
            install.version,
            install.path.display()
        );
        if let Some(vendor) = &install.vendor {
            println!("  vendor: {vendor}");
        }
    }
    Ok(0)
}

fn cmd_accounts(
    launcher: &Launcher,
    add: Option<String>,
    use_account: Option<String>,
    remove: Option<String>,
    json: bool,
) -> Result<i32> {
    if let Some(name) = add {
        let view = launcher.add_offline_account(&name)?;
        println!("added {} ({})", view.name, view.id);
    }
    if let Some(id) = use_account {
        let view = launcher.set_current_account(&id)?;
        println!("active account: {} ({})", view.name, view.id);
    }
    if let Some(id) = remove {
        launcher.remove_account(&id)?;
        println!("removed {id}");
    }
    let response = launcher.accounts()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(0);
    }
    if response.accounts.is_empty() {
        println!("no accounts yet; add one with `tntl accounts --add-offline NAME`");
        return Ok(0);
    }
    for account in &response.accounts {
        let mark = if account.active { '*' } else { ' ' };
        let skin = if account.has_skin { " skin" } else { "" };
        println!(
            "{mark} {:<18} {:<10}{} {}",
            account.name, account.kind, skin, account.id
        );
    }
    if !response.client_id {
        println!("Microsoft sign-in is off: set msa_client_id in launcher.json or TNTL_MSA_CLIENT_ID");
    }
    Ok(0)
}

fn cmd_deps(launcher: &Launcher, instance: &str, fix_versions: bool) -> Result<i32> {
    let report = launcher.install_missing_dependencies(instance, &|event| {
        if let ModEvent::Installed { title, .. } = &event {
            println!("  installed {title}");
        }
    })?;
    println!("installed {} dependency project(s)", report.installed.len());
    if !report.unresolved.is_empty() {
        println!("no exact Modrinth match: {}", report.unresolved.join(", "));
    }
    if fix_versions {
        let fixed = launcher.fix_dependency_versions(instance, &|event| {
            if let ModEvent::Installed { title, version, .. } = &event {
                println!("  replaced {title} {version}");
            }
        })?;
        for update in &fixed.updated {
            println!("{}: {} -> {} ({})", update.name, update.from, update.to, update.file);
        }
        println!("replaced {} dependency build(s)", fixed.updated.len());
        if !fixed.unresolved.is_empty() {
            println!("no matching build: {}", fixed.unresolved.join(", "));
        }
        return Ok(if fixed.unresolved.is_empty() { 0 } else { 1 });
    }
    Ok(0)
}

fn cmd_ping(address: &str) -> Result<i32> {
    let server = tntl_instance::ServerAddress::parse(address).context("invalid address")?;
    let status = tntl_status::ping(&server.host, server.port, Duration::from_secs(5))?;
    println!("{}", status.address);
    println!("  version  {}", status.version);
    println!("  players  {}/{}", status.online, status.max);
    println!("  latency  {} ms", status.latency_ms);
    println!("  motd     {}", status.motd);
    Ok(0)
}

fn cmd_packs(
    launcher: &Launcher,
    query: Option<&str>,
    install: Option<&str>,
    name: Option<String>,
    sort: &str,
    limit: usize,
) -> Result<i32> {
    if let Some(project) = install {
        let report = launcher.install_modpack_project(
            project,
            name.as_deref(),
            &|event| {
                if event.stage == "downloading" || event.stage == "assets" {
                    print!("\r  {} {}/{}", event.stage, event.done, event.total);
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }
            },
            &|_| {},
        )?;
        println!(
            "\ninstalled {} as {} ({})",
            report.name, report.instance, report.game_version
        );
        return Ok(0);
    }
    let page = launcher.search_modpacks(query.unwrap_or(""), sort, limit as u32, 0)?;
    for hit in &page.hits {
        println!("{:<40} {:<12} {}", hit.title, hit.project_id, hit.downloads);
    }
    println!("{} results", page.total_hits);
    Ok(0)
}

fn cmd_ftb(
    launcher: &Launcher,
    query: Option<&str>,
    pack: Option<u64>,
    install: Option<u64>,
    version: Option<u64>,
    name: Option<String>,
    limit: u32,
) -> Result<i32> {
    if let Some(pack_id) = install {
        let report = launcher.install_ftb_pack(
            pack_id,
            version,
            name.as_deref(),
            &|event| {
                if event.stage == "downloading" || event.stage == "assets" {
                    print!("\r  {} {}/{}", event.stage, event.done, event.total);
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }
            },
            &|_| {},
        )?;
        println!(
            "\ninstalled {} {} as {} ({} + {})",
            report.name,
            report.version_id,
            report.instance,
            report.game_version,
            report.loader.as_deref().unwrap_or("vanilla")
        );
        return Ok(0);
    }
    if let Some(pack_id) = pack {
        let pack = launcher.ftb_pack(pack_id)?;
        println!("{} ({})", pack.name, pack.id);
        if !pack.synopsis.is_empty() {
            println!("{}", pack.synopsis);
        }
        if !pack.tags.is_empty() {
            println!("tags: {}", pack.tags.join(", "));
        }
        for version in &pack.versions {
            println!(
                "{:<8} {:<9} {:<8} {:<8} {:<7} {}",
                version.id,
                version.name,
                version.game_version,
                version.loader.as_deref().unwrap_or("vanilla"),
                version.kind,
                version.loader_version.as_deref().unwrap_or("")
            );
        }
        return Ok(0);
    }
    let packs = launcher.search_ftb_packs(query.unwrap_or(""), limit)?;
    for pack in &packs {
        let latest = pack.versions.last();
        let game = latest.map(|version| version.game_version.as_str()).unwrap_or("-");
        let loader = latest
            .and_then(|version| version.loader.as_deref())
            .unwrap_or("vanilla");
        println!(
            "{:<6} {:<38} {:<9} {:<8} {}",
            pack.id, pack.name, game, loader, pack.installs
        );
    }
    Ok(0)
}

fn cmd_cfpack(
    launcher: &Launcher,
    query: Option<&str>,
    install: Option<u64>,
    file: Option<u64>,
    name: Option<String>,
    sort: &str,
    limit: u32,
) -> Result<i32> {
    if let Some(project) = install {
        let report = launcher.install_curseforge_pack(
            project,
            file,
            name.as_deref(),
            &|event| {
                if event.stage == "downloading" || event.stage == "assets" {
                    print!("\r  {} {}/{}", event.stage, event.done, event.total);
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }
            },
            &|event| {
                if let tntl_app::PackEvent::Downloading {
                    files_done,
                    files_total,
                    ..
                } = event
                {
                    print!("\r  {files_done}/{files_total}");
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }
            },
        )?;
        println!(
            "\ninstalled {} {} as {} ({} + {})",
            report.name,
            report.version_id,
            report.instance,
            report.game_version,
            report.loader.as_deref().unwrap_or("vanilla")
        );
        return Ok(0);
    }
    let page = launcher.search_curseforge_packs(query.unwrap_or(""), sort, limit, 0)?;
    for pack in &page.hits {
        let latest = pack.files.last();
        let version = latest.map(|file| file.version.as_str()).unwrap_or("-");
        let game = latest
            .and_then(|file| {
                file.game_versions
                    .iter()
                    .find(|version| version.starts_with("1.") || version.starts_with("26."))
            })
            .map(String::as_str)
            .unwrap_or("-");
        println!(
            "{:<7} {:<40} {:<26} {:<8} {}",
            pack.project_id, pack.title, version, game, pack.downloads
        );
    }
    println!("{} results", page.total_hits);
    Ok(0)
}

fn cmd_login(launcher: &Launcher, client_id: Option<&str>) -> Result<i32> {
    let code = launcher.start_msa_login_with(client_id)?;
    if code.message.is_empty() {
        println!("open {} and enter {}", code.verification_uri, code.user_code);
    } else {
        println!("{}", code.message);
    }
    let mut interval = Duration::from_secs(code.interval.max(1));
    let started = std::time::Instant::now();
    loop {
        if started.elapsed() > Duration::from_secs(code.expires_in.max(60)) {
            bail!("the sign-in code expired");
        }
        match launcher.poll_msa_login_with(client_id, &code.device_code)? {
            tntl_app::MsaPoll::SignedIn { account } => {
                println!("signed in as {} ({})", account.name, account.id);
                return Ok(0);
            }
            tntl_app::MsaPoll::SlowDown => {
                interval = (interval + Duration::from_secs(5)).min(Duration::from_secs(60));
                std::thread::sleep(interval);
            }
            tntl_app::MsaPoll::Pending => std::thread::sleep(interval),
        }
    }
}

fn cmd_skins(launcher: &Launcher, filter: &str, after: Option<&str>, count: usize, json: bool) -> Result<i32> {
    let page = launcher.skin_gallery(filter, after, count)?;
    if json {
        let ids: Vec<_> = page
            .skins
            .iter()
            .map(|skin| {
                serde_json::json!({
                    "id": skin.id,
                    "time": skin.time,
                    "bytes": skin.skin.len(),
                    "slim": skin.slim,
                })
            })
            .collect();
        let report = serde_json::json!({"next": page.next, "skins": ids});
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(0);
    }
    println!("{}: {} skin(s) from MineSkin", filter, page.skins.len());
    for skin in &page.skins {
        println!("  {:<12} {:>8} bytes", skin.id, skin.skin.len());
    }
    match page.next {
        Some(next) => println!("next: --after {next}"),
        None => println!("next: end of the list"),
    }
    Ok(0)
}

/// Everything `tntl skin` can do in one call.
struct SkinArgs<'a> {
    set: Option<PathBuf>,
    model: Option<&'a str>,
    apply: Option<&'a str>,
    clear: bool,
    from_online: Option<&'a str>,
    from_ely: Option<&'a str>,
    from_mojang: Option<&'a str>,
    lookup: Option<&'a str>,
    source: &'a str,
    builtin: Option<&'a str>,
}

fn cmd_skin(launcher: &Launcher, account: &str, args: SkinArgs<'_>) -> Result<i32> {
    let id = if account == "-" {
        launcher
            .accounts()?
            .current
            .context("there is no active account; pass an account id")?
    } else {
        account.to_string()
    };
    let SkinArgs {
        set,
        model,
        apply,
        clear,
        from_online,
        from_ely,
        from_mojang,
        lookup,
        source,
        builtin,
    } = args;
    let mut view = None;
    if let Some(path) = set {
        let bytes = std::fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
        view = Some(launcher.set_account_skin(&id, &bytes, model)?);
    }
    if let Some(key) = builtin {
        view = Some(launcher.set_default_skin(&id, key)?);
    }
    if let Some(nickname) = from_online {
        view = Some(launcher.apply_online_skin(&id, nickname, "auto")?);
    }
    if let Some(nickname) = from_ely {
        view = Some(launcher.apply_online_skin(&id, nickname, "ely")?);
    }
    if let Some(nickname) = from_mojang {
        view = Some(launcher.apply_online_skin(&id, nickname, "mojang")?);
    }
    if let Some(nickname) = lookup {
        let found = launcher.preview_online_skin(nickname, source)?;
        println!(
            "{} ({}) from {}: {} bytes, {} model",
            found.name,
            found.nickname,
            found.source,
            found.skin.len(),
            if found.slim { "slim" } else { "wide" }
        );
        return Ok(0);
    }
    if clear {
        view = Some(launcher.clear_account_skin(&id)?);
    }
    if let Some(apply) = apply {
        let enabled = match apply.trim().to_ascii_lowercase().as_str() {
            "on" => true,
            "off" => false,
            other => bail!("--apply takes on or off, not {other:?}"),
        };
        view = Some(launcher.update_account(&id, model, Some(enabled))?);
    } else if model.is_some() && view.is_none() {
        view = Some(launcher.update_account(&id, model, None)?);
    }
    match view {
        Some(view) => println!(
            "{}: skin={} model={} apply={}",
            view.name,
            if view.has_skin { "yes" } else { "no" },
            view.skin_model,
            view.apply_skin
        ),
        None => {
            let response = launcher.accounts()?;
            let entry = response
                .accounts
                .iter()
                .find(|entry| entry.id == id)
                .with_context(|| format!("no account {id}"))?;
            println!(
                "{}: skin={} model={} apply={} cape={}",
                entry.name,
                if entry.has_skin { "yes" } else { "no" },
                entry.skin_model,
                entry.apply_skin,
                entry.cape.as_deref().unwrap_or("none")
            );
        }
    }
    Ok(0)
}

fn cmd_versions(channel: &str, limit: usize) -> Result<i32> {
    let http = tntl_source::Http::new()?;
    let manifest = http.version_manifest()?;
    println!("latest release: {}", manifest.latest.release);
    println!("latest snapshot: {}", manifest.latest.snapshot);
    let mut shown = 0;
    let channel = channel.trim().to_ascii_lowercase();
    for version in &manifest.versions {
        let wanted = match channel.as_str() {
            "" | "release" | "stable" => version.version_type == "release",
            "snapshot" | "snapshots" => version.version_type != "old_alpha" && version.version_type != "old_beta",
            _ => true,
        };
        if !wanted {
            continue;
        }
        println!("{:<20} {}", version.id, version.version_type);
        shown += 1;
        if shown >= limit {
            break;
        }
    }
    Ok(0)
}

fn cmd_install(
    launcher: &Launcher,
    version: &str,
    loader: Option<&str>,
    loader_version: Option<String>,
    no_assets: bool,
    no_instance: bool,
    name: Option<String>,
) -> Result<i32> {
    let mut request = InstallRequest::new(version);
    request.assets = !no_assets;
    if let Some(loader) = loader {
        let kind = LoaderKind::from_key(loader).with_context(|| {
            format!("unknown loader {loader}; expected fabric, quilt, legacyfabric, forge or neoforge")
        })?;
        request.loader = Some(kind);
        request.loader_version = loader_version;
    } else if loader_version.is_some() {
        bail!("--loader-version needs --loader");
    }
    let instance_name = if no_instance {
        None
    } else {
        Some(name.unwrap_or_else(|| request.instance_name()))
    };
    request.instance = instance_name.clone();

    let last_stage = std::sync::Mutex::new(String::new());
    let report = launcher.install(&request, &|event: InstallEvent| {
        let mut stage = last_stage.lock().unwrap_or_else(|poison| poison.into_inner());
        if *stage != event.stage {
            println!("{}: {} files", event.stage, event.total);
            *stage = event.stage.to_string();
        }
    })?;
    println!(
        "installed {}: {} downloaded, {} already valid, {:.1} MiB",
        report.version_id,
        report.downloaded,
        report.skipped,
        report.bytes as f64 / (1024.0 * 1024.0)
    );
    if let Some(instance_name) = instance_name {
        println!("instance {instance_name} created");
        println!("launch it with: tntl launch {instance_name}");
    }
    Ok(0)
}

fn cmd_loaders(launcher: &Launcher, version: &str, loader: &str, limit: usize) -> Result<i32> {
    let kind = LoaderKind::from_key(loader)
        .with_context(|| format!("unknown loader {loader}; expected fabric, quilt, legacyfabric, forge or neoforge"))?;
    let versions = launcher.loader_versions(version, kind)?;
    if versions.is_empty() {
        println!("no {kind} build is published for {version}");
        return Ok(0);
    }
    for entry in versions.iter().take(limit) {
        let tag = if entry.stable { "stable" } else { "beta" };
        println!("{:<20} {tag}", entry.version);
    }
    Ok(0)
}

fn cmd_loader(launcher: &Launcher, instance: &str, action: Option<LoaderAction>) -> Result<i32> {
    match action {
        None => {
            let status = launcher.loader_status(instance)?;
            match (status.kind, status.version) {
                (Some(kind), version) => println!(
                    "{instance}: {kind} {} on {}",
                    version.unwrap_or_else(|| "?".to_string()),
                    status.game_version
                ),
                (None, _) => println!("{instance}: vanilla {}", status.game_version),
            }
            println!("version id: {}", status.version_id);
        }
        Some(LoaderAction::Install { loader, version }) => {
            let kind = LoaderKind::from_key(&loader).with_context(|| {
                format!("unknown loader {loader}; expected fabric, quilt, legacyfabric, forge or neoforge")
            })?;
            let last_stage = std::sync::Mutex::new(String::new());
            let status = launcher.install_loader(instance, kind, version.as_deref(), &|event: InstallEvent| {
                let mut stage = last_stage.lock().unwrap_or_else(|poison| poison.into_inner());
                if *stage != event.stage {
                    println!("{}: {} files", event.stage, event.total);
                    *stage = event.stage.to_string();
                }
            })?;
            println!(
                "{instance}: {} {} on {}",
                status.kind.unwrap_or_else(|| "vanilla".to_string()),
                status.version.unwrap_or_default(),
                status.game_version
            );
        }
        Some(LoaderAction::Remove) => {
            let status = launcher.remove_loader(instance)?;
            println!("{instance}: vanilla {} ({})", status.game_version, status.version_id);
        }
        Some(LoaderAction::Check) => {
            let update = launcher.check_loader_update(instance)?;
            if update.newer {
                println!(
                    "{} {} -> {} (tntl loader {instance} install {} --version {})",
                    update.kind, update.current, update.latest, update.kind, update.latest
                );
            } else {
                println!("{} {} is the newest build", update.kind, update.current);
            }
        }
    }
    Ok(0)
}

fn cmd_import(
    launcher: &Launcher,
    path: &Path,
    name: Option<String>,
    version: Option<String>,
    list: bool,
) -> Result<i32> {
    if list {
        let scan = launcher.import_candidates(path)?;
        if scan.versions.is_empty() {
            println!("{} has no readable versions", scan.root.display());
        }
        for candidate in &scan.versions {
            let loader = match (&candidate.loader, &candidate.loader_version) {
                (Some(loader), Some(version)) => format!("{loader} {version}"),
                (Some(loader), None) => loader.clone(),
                _ => "vanilla".to_string(),
            };
            println!("{}\t{}\t{loader}", candidate.id, candidate.game_version);
        }
        return Ok(0);
    }
    if path.join("mmc-pack.json").is_file() || path.join("instances").is_dir() {
        let instance = launcher.import_prism_instance(path, name.as_deref(), &|event| {
            if event.stage == "downloading" || event.stage == "assets" {
                print!("\r  {} {}/{}", event.stage, event.done, event.total);
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
        })?;
        println!(
            "\nimported Prism instance {} as {} (game dir {})",
            instance.version_id,
            instance.name,
            instance.game_dir(&launcher.instance_dir(&instance.name)?).display()
        );
        return Ok(0);
    }
    let instance = launcher.import_instance(path, name.as_deref(), version.as_deref(), &|event| {
        if event.stage != "manifest" && event.stage != "metadata" {
            print!("\r  {} {}/{}", event.stage, event.done, event.total);
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
    })?;
    println!(
        "\nimported {} as instance {} (root {})",
        instance.version_id,
        instance.name,
        instance.game_dir(&launcher.instance_dir(&instance.name)?).display()
    );
    Ok(0)
}

fn cmd_rename(launcher: &Launcher, old: &str, new: &str) -> Result<i32> {
    let dir = launcher.rename_instance(old, new)?;
    println!("renamed {old} to {new} ({})", dir.display());
    Ok(0)
}

fn cmd_delete(launcher: &Launcher, name: &str, yes: bool, cleanup: bool) -> Result<i32> {
    if !yes && !confirm(&format!("remove instance {name} from the library? its files are kept"))? {
        println!("cancelled");
        return Ok(0);
    }
    // Unregistering never touches the disk; the user deletes files by hand.
    let dir = launcher.unregister(name)?;
    println!("removed {name} from the library; files kept in {}", dir.display());
    if cleanup {
        let (removed, bytes) = launcher.delete_unused_versions()?;
        for id in removed {
            println!("removed unused version {id}");
        }
        println!("freed {}", human_bytes(bytes));
    }
    Ok(0)
}

fn cmd_installed(launcher: &Launcher, delete: Option<&str>, cleanup: bool, json: bool) -> Result<i32> {
    if let Some(id) = delete {
        launcher.delete_version(id)?;
        println!("deleted version {id}");
        return Ok(0);
    }
    if cleanup {
        let (removed, bytes) = launcher.delete_unused_versions()?;
        if removed.is_empty() {
            println!("no unused versions");
        } else {
            println!("removed {} version(s), {} freed", removed.len(), human_bytes(bytes));
            for id in removed {
                println!("  {id}");
            }
        }
        return Ok(0);
    }
    let versions = launcher.installed_version_details();
    if json {
        println!("{}", serde_json::to_string_pretty(&versions)?);
        return Ok(0);
    }
    if versions.is_empty() {
        println!("no versions installed in {}", launcher.versions_dir().display());
        return Ok(0);
    }
    for version in versions {
        let users = if version.instances.is_empty() {
            "unused".to_string()
        } else {
            format!("used by {}", version.instances.join(", "))
        };
        println!("{:<28} {:>9}  {users}", version.id, human_bytes(version.bytes));
    }
    Ok(0)
}

fn confirm(question: &str) -> Result<bool> {
    use std::io::Write;
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

fn cmd_list(launcher: &Launcher) -> Result<i32> {
    let instances = list_instances(launcher)?;
    if instances.is_empty() {
        println!("no instances in {}", launcher.instances_dir().display());
        return Ok(0);
    }
    for instance in instances {
        let root = instance
            .root
            .as_ref()
            .map(|root| root.display().to_string())
            .unwrap_or_else(|| "launcher root".to_string());
        println!("{:<24} {:<20} {}", instance.name, instance.version_id, root);
    }
    Ok(0)
}

fn cmd_plan(launcher: &Launcher, name: &str, json: bool) -> Result<i32> {
    let instance = launcher.load_instance(name)?;
    let plan = launcher.plan_instance_unattended(&instance, None, VerifyMode::Exists)?;
    let plan = plan.redacted();
    if json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        return Ok(0);
    }
    print_plan(&plan);
    Ok(0)
}

fn cmd_verify(launcher: &Launcher, name: &str) -> Result<i32> {
    let instance = launcher.load_instance(name)?;
    let plan = launcher.plan_instance_unattended(&instance, None, VerifyMode::Hash)?;
    let broken = launcher.broken_loader_outputs(name)?;
    if plan.missing.is_empty() && broken.is_empty() {
        println!("all files are valid");
        return Ok(0);
    }
    if !plan.missing.is_empty() {
        let corrupt = plan.missing.iter().filter(|entry| entry.path.is_file()).count();
        let missing = plan.missing.len() - corrupt;
        let bytes: u64 = plan.missing.iter().filter_map(|entry| entry.size).sum();
        println!(
            "{} problem(s): {missing} missing, {corrupt} corrupt, {} to fetch",
            plan.missing.len(),
            human_bytes(bytes)
        );
        let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
        for entry in &plan.missing {
            *kinds.entry(missing_kind_label(entry.kind)).or_default() += 1;
        }
        for (kind, count) in kinds {
            println!("  {kind:<14} {count}");
        }
        print_missing(&plan);
    }
    if !broken.is_empty() {
        println!("{} loader file(s) no longer match the installer record:", broken.len());
        for path in &broken {
            let shown = path.strip_prefix(&launcher.root).unwrap_or(path);
            println!("  {}", shown.display());
        }
    }
    println!("run `tntl repair {name}` to fetch or rebuild them");
    Ok(2)
}

fn cmd_repair(launcher: &Launcher, name: &str, json: bool) -> Result<i32> {
    let last_stage = std::sync::Mutex::new(String::new());
    // `--json` owns stdout; progress lines go to stderr so the output parses.
    let progress = |line: String| {
        if json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };
    let report = launcher.repair_instance(
        name,
        &|event: InstallEvent| {
            let mut stage = last_stage.lock().unwrap_or_else(|poison| poison.into_inner());
            if *stage != event.stage {
                progress(format!("{}: {} file(s)", event.stage, event.total));
                *stage = event.stage.to_string();
            }
        },
        &|event: PackEvent| {
            if let PackEvent::Downloading {
                files_total,
                path,
                files_done,
                ..
            } = event
                && files_done == 0
            {
                progress(format!("pack: {files_total} file(s), starting with {path}"));
            }
        },
    )?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(0);
    }
    let fixed = report.game_downloaded + report.pack_downloaded;
    let checked = report.game_downloaded + report.game_skipped + report.pack_downloaded + report.pack_skipped;
    match &report.pack {
        Some(pack) => println!("{pack}: {checked} file(s) checked, {fixed} fetched"),
        None => println!("{checked} file(s) checked, {fixed} fetched"),
    }
    if fixed == 0 {
        println!("nothing was missing");
    } else {
        println!("fetched {} ({})", fixed, human_bytes(report.bytes));
    }
    Ok(0)
}

fn cmd_launch(
    launcher: &Launcher,
    name: &str,
    dry_run: bool,
    timeout: Option<u64>,
    echo: bool,
    java: Option<&Path>,
    demo: bool,
) -> Result<i32> {
    launcher.ensure_not_running(name)?;
    let mut instance = launcher.load_instance(name)?;
    if demo {
        instance.demo = true;
    }
    launcher.prepare_skin(name)?;
    let plan = launcher.plan_instance(&instance, java, VerifyMode::Exists)?;
    if dry_run {
        print_plan(&plan.redacted());
        return Ok(0);
    }
    if !plan.missing.is_empty() {
        print_missing(&plan);
        if plan.missing.iter().any(|missing| missing.kind == MissingKind::Java) {
            bail!("no usable Java found; run `tntl java` to see what is installed");
        }
        let environment = launcher.environment(name)?;
        match environment.loader.as_deref() {
            Some(loader) => bail!(
                "instance {name} is incomplete; run `tntl install {} --loader {loader}`",
                environment.game_version
            ),
            None => bail!(
                "instance {name} is incomplete; run `tntl install {}`",
                environment.game_version
            ),
        }
    }
    let instance_dir = launcher.instance_dir(name)?;
    let log_path = instance_dir.join("logs").join(format!("{}.log", timestamp()));
    let mut options = LaunchOptions::new(&log_path);
    options.echo = echo;
    let process = tntl_launch::launch(&plan, &options)?;
    launcher.mark_running(name, process.id())?;
    println!("launched {} (pid {})", instance.version_id, process.id());
    println!("log: {}", log_path.display());
    let outcome = process.wait_timeout(timeout.map(Duration::from_secs))?;
    launcher.clear_running(name);
    if let Err(error) = launcher.record_playtime(name, outcome.duration_ms) {
        eprintln!("cannot record play time: {error:#}");
    }
    println!(
        "game exited with {:?} after {:.1}s",
        outcome.exit_code,
        outcome.duration_ms as f64 / 1000.0
    );
    if let Some(crash) = outcome.crash {
        if crash.kind == CrashKind::Terminated {
            println!("game terminated");
            return Ok(0);
        }
        println!("crash: {:?}", crash.kind);
        println!("  {}", crash.summary);
        for hint in crash.hints {
            println!("  - {hint}");
        }
        if crash.kind == CrashKind::Unknown {
            println!("check the log: {}", outcome.log_path.display());
        }
        return Ok(1);
    }
    Ok(0)
}

fn cmd_mods(
    launcher: &Launcher,
    instance: &str,
    disable: Option<String>,
    enable: Option<String>,
    remove: Option<String>,
    json: bool,
) -> Result<i32> {
    let actions = [enable.is_some(), disable.is_some(), remove.is_some()];
    if actions.iter().filter(|action| **action).count() > 1 {
        bail!("pick only one of --enable, --disable or --remove");
    }
    let mods = match (enable, disable, remove) {
        (Some(file), None, None) => launcher.set_mod_enabled(instance, &file, true)?,
        (None, Some(file), None) => launcher.set_mod_enabled(instance, &file, false)?,
        (None, None, Some(file)) => launcher.remove_mod(instance, &file)?,
        (None, None, None) => launcher.mods(instance)?,
        _ => unreachable!("exclusive flags are checked above"),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&mods)?);
        return Ok(0);
    }
    if mods.is_empty() {
        println!("no mods installed for {instance}");
        return Ok(0);
    }
    for entry in mods {
        let state = if entry.enabled { "on" } else { "off" };
        let blocked = match &entry.blocked {
            Some(blocked) => format!(
                "  [needs {} {}; instance has {}]",
                blocked.loader,
                blocked.required.join(" / "),
                blocked.actual
            ),
            None => String::new(),
        };
        println!(
            "{state:<4} {:<28} {:<12} {}{blocked}",
            entry.name, entry.version, entry.file
        );
    }
    Ok(0)
}

fn cmd_add_file(launcher: &Launcher, instance: &str, path: &Path, kind: &str) -> Result<i32> {
    let kind = ProjectKind::from_key(kind)
        .with_context(|| format!("unknown kind {kind}; expected mod, resourcepack or shader"))?;
    if kind == ProjectKind::Mod {
        let mods = launcher.add_local_mod(instance, path)?;
        println!("installed {} into {instance} ({} mods)", path.display(), mods.len());
    } else {
        let listing = launcher.add_local_content(instance, kind, path)?;
        println!(
            "installed {} into {instance} ({} {kind} files)",
            path.display(),
            listing.files.len()
        );
    }
    Ok(0)
}

fn cmd_duplicate(launcher: &Launcher, instance: &str, name: &str) -> Result<i32> {
    let dir = launcher.duplicate_instance(instance, name)?;
    println!("duplicated {instance} to {name} ({})", dir.display());
    Ok(0)
}

fn cmd_export(
    launcher: &Launcher,
    instance: &str,
    output: Option<PathBuf>,
    name: Option<String>,
    version: &str,
) -> Result<i32> {
    let output = output.unwrap_or_else(|| PathBuf::from(format!("{instance}.mrpack")));
    let report = launcher.export_instance(instance, &output, name.as_deref(), version)?;
    println!(
        "exported {instance} to {} ({} files, {})",
        output.display(),
        report.files,
        human_bytes(report.bytes)
    );
    Ok(0)
}

fn cmd_archive(launcher: &Launcher, instance: &str, output: Option<PathBuf>) -> Result<i32> {
    let output = output.unwrap_or_else(|| PathBuf::from(format!("{instance}.zip")));
    let report = launcher.export_archive(instance, &output)?;
    println!(
        "exported {instance} to {} ({} files, {})",
        report.path.display(),
        report.files,
        human_bytes(report.bytes)
    );
    Ok(0)
}

fn cmd_import_archive(launcher: &Launcher, archive: &Path, name: Option<String>) -> Result<i32> {
    let name = name.unwrap_or_else(|| {
        archive
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_else(|| "instance".to_string())
    });
    let name = tntl_app::sanitize_name(&name);
    let instance = launcher.import_archive(archive, &name)?;
    println!("imported {} as {}", archive.display(), instance.name);
    Ok(0)
}

fn cmd_servers(
    launcher: &Launcher,
    instance: &str,
    add: Option<String>,
    remove: Option<usize>,
    json: bool,
) -> Result<i32> {
    let mut servers = launcher.servers(instance)?;
    let changed = add.is_some() || remove.is_some();
    if let Some(spec) = add {
        let (name, ip) = spec
            .split_once('|')
            .with_context(|| format!("expected `Name|host:port`, got {spec}"))?;
        if ip.trim().is_empty() {
            bail!("server address is empty");
        }
        servers.push(tntl_instance::servers::ServerEntry {
            name: name.trim().to_string(),
            ip: ip.trim().to_string(),
            accept_textures: true,
            index: None,
        });
    }
    if let Some(index) = remove {
        if index >= servers.len() {
            bail!("there is no server at index {index}; use `tntl servers {instance}` to see the list");
        }
        servers.remove(index);
    }
    if changed {
        servers = launcher.save_servers(instance, &servers)?;
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&servers)?);
        return Ok(0);
    }
    if servers.is_empty() {
        println!("no servers in {instance}");
        return Ok(0);
    }
    for (index, server) in servers.iter().enumerate() {
        println!("{index:>2}  {:<24} {}", server.name, server.ip);
    }
    Ok(0)
}

fn cmd_crashes(
    launcher: &Launcher,
    instance: &str,
    show: Option<String>,
    delete: Option<String>,
    json: bool,
) -> Result<i32> {
    if let Some(file) = show {
        println!("{}", launcher.read_crash_report(instance, &file)?);
        return Ok(0);
    }
    if let Some(file) = delete {
        launcher.delete_crash_report(instance, &file)?;
        println!("deleted {file}");
    }
    let reports = launcher.crash_reports(instance)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
        return Ok(0);
    }
    if reports.is_empty() {
        println!("no crash reports in {instance}");
        return Ok(0);
    }
    for report in reports {
        println!("{:>10}  {}", human_bytes(report.bytes), report.file);
    }
    Ok(0)
}

fn cmd_screenshots(launcher: &Launcher, instance: &str, delete: Option<String>, json: bool) -> Result<i32> {
    if let Some(file) = delete {
        launcher.delete_screenshot(instance, &file)?;
        println!("deleted {file}");
    }
    let shots = launcher.screenshots(instance)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&shots)?);
        return Ok(0);
    }
    if shots.is_empty() {
        println!("no screenshots in {instance}");
        return Ok(0);
    }
    for shot in shots {
        println!("{:>10}  {}", human_bytes(shot.bytes), shot.file);
    }
    Ok(0)
}

fn cmd_disk(launcher: &Launcher, instance: &str) -> Result<i32> {
    let usage = launcher.disk_usage(instance)?;
    for entry in &usage.entries {
        println!(
            "{:>10}  {:<20} {}",
            human_bytes(entry.bytes),
            entry.label,
            entry.path.display()
        );
    }
    println!("{:>10}  total", human_bytes(usage.total));
    Ok(0)
}

fn cmd_cleanup(launcher: &Launcher, instance: &str, yes: bool) -> Result<i32> {
    if !yes && !confirm(&format!("remove logs, crash reports and natives of {instance}?"))? {
        println!("cancelled");
        return Ok(0);
    }
    let report = launcher.cleanup(instance)?;
    if report.removed.is_empty() {
        println!("nothing to remove");
    } else {
        // The service returns stable keys; the UI translates them.
        let labels: Vec<&str> = report
            .removed
            .iter()
            .map(|key| match key.as_str() {
                "crash_reports" => "crash reports",
                other => other,
            })
            .collect();
        println!("removed {} ({})", labels.join(", "), human_bytes(report.bytes));
    }
    Ok(0)
}

fn cmd_meta(
    launcher: &Launcher,
    instance: &str,
    group: Option<String>,
    notes: Option<String>,
    icon: Option<PathBuf>,
    clear_icon: bool,
) -> Result<i32> {
    if let Some(icon) = &icon {
        launcher.set_instance_icon(instance, icon)?;
        println!("icon: {}", icon.display());
    }
    if clear_icon {
        launcher.clear_instance_icon(instance)?;
        println!("icon cleared");
    }
    if group.is_some() || notes.is_some() {
        let mut config = launcher.load_instance(instance)?;
        if let Some(group) = group {
            config.group = Some(group).filter(|group| !group.trim().is_empty());
        }
        if let Some(notes) = notes {
            config.notes = notes;
        }
        launcher.save_instance(&config)?;
    }
    let config = launcher.load_instance(instance)?;
    println!("instance: {}", config.name);
    println!("group:    {}", config.group.as_deref().unwrap_or("-"));
    println!(
        "notes:    {}",
        if config.notes.is_empty() { "-" } else { &config.notes }
    );
    println!(
        "icon:     {}",
        config
            .icon
            .as_ref()
            .map(|icon| icon.display().to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    Ok(0)
}

#[allow(clippy::too_many_arguments)]
fn cmd_worlds(
    launcher: &Launcher,
    instance: &str,
    backup: Option<String>,
    import: Option<PathBuf>,
    delete: Option<String>,
    duplicate: Option<String>,
    delete_backup: Option<String>,
    backups: bool,
    yes: bool,
    json: bool,
) -> Result<i32> {
    if let Some(world) = backup {
        let path = launcher.backup_world(instance, &world)?;
        println!("backed up {world} to {}", path.display());
    }
    if let Some(archive) = import {
        let archive = if archive.exists() {
            archive
        } else {
            launcher
                .instance_dir(instance)?
                .join("backups")
                .join("worlds")
                .join(archive)
        };
        let world = launcher.import_world(instance, &archive)?;
        println!("imported world {} from {}", world.dir, archive.display());
    }
    if let Some(world) = delete {
        if !yes && !confirm(&format!("delete world {world}?"))? {
            println!("cancelled");
            return Ok(0);
        }
        launcher.delete_world(instance, &world)?;
        println!("deleted {world}");
    }
    if let Some(world) = duplicate {
        let copy = launcher.duplicate_world(instance, &world)?;
        println!("copied {world} to {}", copy.dir);
    }
    if let Some(file) = delete_backup {
        if !yes && !confirm(&format!("delete backup {file}?"))? {
            println!("cancelled");
            return Ok(0);
        }
        let left = launcher.delete_world_backup(instance, &file)?;
        println!("deleted {file}; {} backup(s) left", left.len());
    }
    if backups {
        let list = launcher.world_backups(instance)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&list)?);
            return Ok(0);
        }
        if list.is_empty() {
            println!("no world backups for {instance}");
            return Ok(0);
        }
        for entry in list {
            println!("{:>10}  {}", human_bytes(entry.bytes), entry.file);
        }
        return Ok(0);
    }
    let worlds = launcher.worlds(instance)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&worlds)?);
        return Ok(0);
    }
    if worlds.is_empty() {
        println!("no worlds in {instance}");
        return Ok(0);
    }
    for world in worlds {
        let mode = world.game_mode.unwrap_or_else(|| "?".to_string());
        let version = world.version.unwrap_or_else(|| "?".to_string());
        let played = world
            .last_played_ms
            .map(|ms| {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_millis() as i64)
                    .unwrap_or_default();
                let age = now.saturating_sub(ms).max(0) / 1000;
                if age < 3600 {
                    format!("{}m ago", age / 60)
                } else if age < 86_400 {
                    format!("{}h ago", age / 3600)
                } else {
                    format!("{}d ago", age / 86_400)
                }
            })
            .unwrap_or_else(|| "?".to_string());
        println!(
            "{:<24} {:<10} {:<10} {:>10}  {}",
            world.dir,
            version,
            mode,
            human_bytes(world.size),
            played
        );
    }
    Ok(0)
}

#[allow(clippy::too_many_arguments)]
fn cmd_datapacks(
    launcher: &Launcher,
    instance: &str,
    world: Option<String>,
    enable: Option<String>,
    disable: Option<String>,
    delete: Option<String>,
    add: Option<PathBuf>,
    yes: bool,
    json: bool,
) -> Result<i32> {
    let mutating = enable.is_some() || disable.is_some() || delete.is_some() || add.is_some();
    if mutating && world.is_none() {
        // The default world is the most recently played one; acting on it by
        // accident is how packs end up deleted from the wrong save.
        bail!("pass --world <save> when adding, enabling, disabling or deleting datapacks");
    }
    let world = match world {
        Some(world) => world,
        None => match launcher.worlds(instance)?.into_iter().next() {
            Some(world) => world.dir,
            None => bail!("{instance} has no worlds; pass --world"),
        },
    };
    let mut packs = launcher.datapacks(instance, &world)?;
    if let Some(file) = add {
        packs = launcher.add_datapack(instance, &world, &file)?;
        println!("added {} to {world}", file.display());
    }
    if let Some(file) = disable {
        packs = launcher.set_datapack_enabled(instance, &world, &file, false)?;
        println!("disabled {file} in {world}");
    }
    if let Some(file) = enable {
        packs = launcher.set_datapack_enabled(instance, &world, &file, true)?;
        println!("enabled {file} in {world}");
    }
    if let Some(file) = delete {
        if !yes && !confirm(&format!("delete datapack {file} from {world}?"))? {
            println!("cancelled");
            return Ok(0);
        }
        packs = launcher.delete_datapack(instance, &world, &file)?;
        println!("deleted {file} from {world}");
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&packs)?);
        return Ok(0);
    }
    if packs.is_empty() {
        println!("no datapacks in {world}");
        return Ok(0);
    }
    for pack in packs {
        println!(
            "{} {:>10}  {}",
            if pack.enabled { "on " } else { "off" },
            human_bytes(pack.bytes),
            pack.name
        );
    }
    Ok(0)
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

struct SearchArgs<'a> {
    query: &'a str,
    instance: Option<&'a str>,
    version: Option<&'a str>,
    loader: Option<&'a str>,
    kind: &'a str,
    sort: &'a str,
    source: &'a str,
    limit: u32,
    offset: u32,
    json: bool,
}

fn cmd_search(launcher: &Launcher, args: SearchArgs<'_>) -> Result<i32> {
    let kind = ProjectKind::from_key(args.kind).with_context(|| {
        format!(
            "unknown project kind {}; expected mod, resourcepack or shader",
            args.kind
        )
    })?;
    let source = ContentSource::from_key(args.source)
        .with_context(|| format!("unknown source {}; expected modrinth or curseforge", args.source))?;
    let (game, loader, hits, total) = match args.instance {
        Some(name) => {
            let page = launcher.search_projects(
                name,
                kind,
                args.query,
                args.sort,
                args.limit,
                args.offset,
                source,
                args.version,
            )?;
            (page.game_version, page.loader, page.hits, page.total_hits)
        }
        None => {
            let game = args
                .version
                .context("pass --instance, or both --version and --loader")?;
            let loader = args.loader.context("pass --instance, or both --version and --loader")?;
            let page = launcher.search_catalogue(
                kind,
                game,
                if kind == ProjectKind::Mod { loader } else { "" },
                args.query,
                args.sort,
                args.limit,
                args.offset,
                source,
            )?;
            (game.to_string(), loader.to_string(), page.hits, page.total_hits)
        }
    };
    if args.json {
        let body = serde_json::json!({
            "game_version": game,
            "loader": loader,
            "total_hits": total,
            "hits": hits,
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(0);
    }
    if hits.is_empty() {
        println!("no mods found for {game} with {loader}");
        return Ok(0);
    }
    println!("{total} results ({game} with {loader})");
    for hit in hits {
        println!("{:<28} {:>10}  {}", hit.slug, hit.downloads, hit.title);
        if !hit.description.is_empty() {
            println!("  {}", hit.description);
        }
    }
    Ok(0)
}

fn cmd_add(
    launcher: &Launcher,
    project: &str,
    instance: &str,
    kind: &str,
    source: &str,
    version: Option<&str>,
    json: bool,
) -> Result<i32> {
    let kind = ProjectKind::from_key(kind)
        .with_context(|| format!("unknown project kind {kind}; expected mod, resourcepack or shader"))?;
    let source = ContentSource::from_key(source)
        .with_context(|| format!("unknown source {source}; expected modrinth or curseforge"))?;
    let installed = match version {
        Some(version) => {
            launcher.install_project_version(instance, kind, project, version, source, &|event| match event {
                ModEvent::Resolving { project } => println!("resolving {project}"),
                ModEvent::Installed {
                    title,
                    version,
                    file,
                    dependency,
                    ..
                } => println!("installed {title} {version} ({file}, {})", role(dependency, kind)),
                ModEvent::Skipped { title, dependency, .. } => {
                    println!("already installed {title} ({})", role(dependency, kind))
                }
                ModEvent::Downloading { .. } => {}
            })?
        }
        None => launcher.install_projects(
            instance,
            kind,
            &[project.to_string()],
            source,
            None,
            &|event| match event {
                ModEvent::Resolving { project } => println!("resolving {project}"),
                ModEvent::Installed {
                    title,
                    version,
                    file,
                    dependency,
                    ..
                } => println!("installed {title} {version} ({file}, {})", role(dependency, kind)),
                ModEvent::Skipped { title, dependency, .. } => {
                    println!("already installed {title} ({})", role(dependency, kind))
                }
                ModEvent::Downloading { .. } => {}
            },
        )?,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&installed)?);
    }
    Ok(0)
}

fn cmd_updates(
    launcher: &Launcher,
    instance: &str,
    kind: &str,
    apply: bool,
    files: &[String],
    json: bool,
) -> Result<i32> {
    let kind = ProjectKind::from_key(kind)
        .with_context(|| format!("unknown project kind {kind}; expected mod, resourcepack or shader"))?;
    if apply {
        let applied = launcher.update_content(instance, kind, files, &|event| match event {
            ModEvent::Resolving { project } => println!("updating {project}"),
            ModEvent::Installed {
                title, version, file, ..
            } => println!("updated {title} to {version} ({file})"),
            ModEvent::Skipped { .. } | ModEvent::Downloading { .. } => {}
        })?;
        if json {
            println!("{}", serde_json::to_string_pretty(&applied)?);
        } else if applied.is_empty() {
            println!("nothing to update in {instance}");
        }
        return Ok(0);
    }

    let updates = launcher.check_updates(instance, kind)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&updates)?);
        return Ok(0);
    }
    if updates.is_empty() {
        println!("all {kind}s of {instance} are up to date");
        return Ok(0);
    }
    println!("{} update(s) available:", updates.len());
    for update in &updates {
        println!(
            "{:<28} {} -> {}  {}",
            update.name, update.current_version, update.latest_version, update.latest_file
        );
    }
    println!("run `tntl updates {instance} --kind {kind} --apply` to install them");
    Ok(0)
}

#[allow(clippy::too_many_arguments)]
fn cmd_content(
    launcher: &Launcher,
    instance: &str,
    kind: &str,
    enable: Option<String>,
    disable: Option<String>,
    delete: Option<String>,
    up: Option<String>,
    down: Option<String>,
    json: bool,
) -> Result<i32> {
    let kind = ProjectKind::from_key(kind)
        .with_context(|| format!("unknown project kind {kind}; expected resourcepack or shader"))?;
    let actions = [&enable, &disable, &delete, &up, &down]
        .iter()
        .filter(|action| action.is_some())
        .count();
    if actions > 1 {
        bail!("pick one of --enable, --disable, --delete, --up or --down");
    }
    let listing = match (enable, disable, delete, up, down) {
        (Some(file), ..) => launcher.set_content_enabled(instance, kind, &file, true)?,
        (_, Some(file), ..) => launcher.set_content_enabled(instance, kind, &file, false)?,
        (_, _, Some(file), ..) => launcher.remove_content(instance, kind, &file)?,
        (_, _, _, Some(file), _) => launcher.move_resource_pack(instance, &file, true)?,
        (_, _, _, _, Some(file)) => launcher.move_resource_pack(instance, &file, false)?,
        _ => launcher.content(instance, kind)?,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&listing)?);
        return Ok(0);
    }
    if listing.files.is_empty() {
        println!("no {kind}s installed for {instance}");
        return Ok(0);
    }
    if !listing.toggle_supported {
        println!("switching is unavailable: install Iris or OptiFine to enable shaders from here");
    }
    for entry in listing.files {
        let state = if entry.enabled { "on" } else { "off" };
        println!(
            "{state:<4} {:<44} {:.1} MiB",
            entry.file,
            entry.bytes as f64 / (1024.0 * 1024.0)
        );
    }
    Ok(0)
}

fn cmd_pack(launcher: &Launcher, instance: &str, apply: bool, version: Option<String>, json: bool) -> Result<i32> {
    let update = launcher.check_pack_update(instance)?;
    if apply {
        // An explicit version may be older than the current build (a rollback),
        // so it is not looked up in the "newer builds" list.
        let chosen = match version {
            Some(id) => id,
            None => update
                .targets
                .first()
                .with_context(|| format!("{} is up to date ({})", update.name, update.current_version))?
                .version_id
                .clone(),
        };
        let last_stage = std::sync::Mutex::new(String::new());
        let report = launcher.update_pack(
            instance,
            &chosen,
            &|event: InstallEvent| {
                let mut stage = last_stage.lock().unwrap_or_else(|poison| poison.into_inner());
                if *stage != event.stage {
                    println!("{}: {} files", event.stage, event.total);
                    *stage = event.stage.to_string();
                }
            },
            &|event: PackEvent| match event {
                PackEvent::Installed { path, .. } => println!("downloaded {path}"),
                PackEvent::Skipped { path } => println!("already present: {path}"),
                PackEvent::Extracting { prefix, entries } => {
                    println!("copied {entries} file(s) from {prefix}/");
                }
                PackEvent::Downloading { .. } => {}
                PackEvent::Archive { .. } => {}
            },
        )?;
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
            return Ok(0);
        }
        println!(
            "updated {} to {} ({}{}), removed {} stale file(s)",
            report.name,
            report.version_number,
            report.game_version,
            report
                .loader
                .as_deref()
                .map(|loader| format!(" + {loader}"))
                .unwrap_or_default(),
            report.removed
        );
        println!(
            "{} pack file(s): {} downloaded, {} already valid, {:.1} MiB; {} override file(s)",
            report.files.files,
            report.files.downloaded,
            report.files.skipped,
            report.files.bytes as f64 / (1024.0 * 1024.0),
            report.files.overrides
        );
        return Ok(0);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&update)?);
        return Ok(0);
    }
    println!(
        "{} {} (Minecraft {})",
        update.name, update.current_version, update.current_game_version
    );
    if update.targets.is_empty() {
        println!("up to date");
        return Ok(0);
    }
    for target in &update.targets {
        let kind = if target.in_place {
            "same game version"
        } else {
            "new game version"
        };
        println!(
            "{:<12} {} ({}, {}, {:.1} MiB, {kind})",
            target.version_id,
            target.version_number,
            target.game_version,
            target.version_type,
            target.size as f64 / (1024.0 * 1024.0)
        );
    }
    println!("run `tntl pack {instance} --apply` to install the first one");
    Ok(0)
}

fn cmd_modpack(launcher: &Launcher, path: &Path, name: Option<String>, json: bool) -> Result<i32> {
    let last_stage = std::sync::Mutex::new(String::new());
    let report = launcher.install_modpack(
        path,
        name.as_deref(),
        &|event: InstallEvent| {
            let mut stage = last_stage.lock().unwrap_or_else(|poison| poison.into_inner());
            if *stage != event.stage {
                println!("{}: {} files", event.stage, event.total);
                *stage = event.stage.to_string();
            }
        },
        &|event: PackEvent| match event {
            PackEvent::Installed { path, bytes } => {
                println!("downloaded {path} ({:.1} MiB)", bytes as f64 / (1024.0 * 1024.0));
            }
            PackEvent::Skipped { path } => println!("already present: {path}"),
            PackEvent::Extracting { prefix, entries } => {
                println!("copied {entries} file(s) from {prefix}/");
            }
            PackEvent::Downloading { .. } => {}
            PackEvent::Archive { .. } => {}
        },
    )?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(0);
    }
    println!(
        "installed {} ({}{}) as instance {}",
        report.name,
        report.game_version,
        report
            .loader
            .as_deref()
            .map(|loader| format!(" + {loader}"))
            .unwrap_or_default(),
        report.instance
    );
    println!(
        "{} pack file(s): {} downloaded, {} already valid, {:.1} MiB; {} override file(s)",
        report.files.files,
        report.files.downloaded,
        report.files.skipped,
        report.files.bytes as f64 / (1024.0 * 1024.0),
        report.files.overrides
    );
    println!("launch it with: tntl launch {}", report.instance);
    Ok(0)
}

fn role(dependency: bool, kind: ProjectKind) -> &'static str {
    if dependency { "dependency" } else { kind.key() }
}

fn cmd_doctor(launcher: &Launcher) -> Result<i32> {
    println!("tntl {}", env!("CARGO_PKG_VERSION_MAJOR"));
    println!("platform: {} {}", std::env::consts::OS, std::env::consts::ARCH);
    println!("data dir: {}", launcher.root.display());
    println!("instances: {}", list_instances(launcher)?.len());
    println!("java:");
    let managed = launcher.java_dir();
    let installs = launcher.java_installs();
    if installs.is_empty() {
        println!("  none found");
    }
    for install in installs {
        let origin = if install.path.starts_with(&managed) {
            "managed"
        } else {
            "system"
        };
        println!(
            "  {} ({} {}, {origin})",
            install.path.display(),
            install.major,
            install.version
        );
    }
    println!("endpoints:");
    println!("  manifest {}", endpoints::VERSION_MANIFEST);
    println!("  libraries {}", endpoints::LIBRARIES);
    println!("  resources {}", endpoints::RESOURCES);
    println!("  fabric {}", endpoints::FABRIC_META);
    println!("  quilt {}", endpoints::QUILT_META);
    println!("  legacyfabric {}", endpoints::LEGACY_FABRIC_META);
    println!("  adoptium {}", endpoints::ADOPTIUM_API);
    let network = tntl_source::network_config();
    println!("proxy: {}", network.mode.key());
    if let Some(url) = network.url {
        println!("  {url}");
    }
    Ok(0)
}

fn print_plan(plan: &LaunchPlan) {
    println!("version: {}", plan.version_id);
    println!("java: {}", plan.java.display());
    println!("main class: {}", plan.main_class);
    println!("working dir: {}", plan.working_dir.display());
    println!("classpath ({}):", plan.classpath.len());
    for entry in &plan.classpath {
        println!("  {}", entry.display());
    }
    println!("jvm args ({}):", plan.jvm_args.len());
    for arg in &plan.jvm_args {
        println!("  {arg}");
    }
    println!("game args ({}):", plan.game_args.len());
    for arg in &plan.game_args {
        println!("  {arg}");
    }
    if !plan.natives.is_empty() {
        println!("natives ({}):", plan.natives.len());
        for native in &plan.natives {
            println!("  {}", native.path.display());
        }
    }
    if !plan.missing.is_empty() {
        println!("missing ({}):", plan.missing.len());
        print_missing(plan);
    }
    for warning in &plan.warnings {
        println!("warning [{}]: {}", warning.code, warning.message);
    }
}

fn missing_kind_label(kind: MissingKind) -> &'static str {
    match kind {
        MissingKind::VersionJson => "version json",
        MissingKind::ClientJar => "client jar",
        MissingKind::Library => "library",
        MissingKind::Native => "native",
        MissingKind::AssetIndex => "asset index",
        MissingKind::Asset => "asset",
        MissingKind::LoggingConfig => "logging config",
        MissingKind::Java => "java",
    }
}

fn print_missing(plan: &LaunchPlan) {
    for missing in &plan.missing {
        let state = if missing.path.is_file() { "corrupt" } else { "missing" };
        let size = missing
            .size
            .map(|size| format!(", {}", human_bytes(size)))
            .unwrap_or_default();
        println!(
            "  [{state}] {} ({}{size})",
            missing.path.display(),
            missing_kind_label(missing.kind)
        );
        if let Some(url) = &missing.url {
            println!("          {url}");
        }
    }
}

/// Milliseconds: two launches within the same second used to share a log file.
fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}
