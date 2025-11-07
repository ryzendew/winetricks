//! Winetricks CLI

use clap::Parser;
use std::io::{self, Write};
use std::process;
use std::str::FromStr;
use tracing::{info, warn};
use winetricks_lib::{Config, Executor, Result, VerbCategory, VerbRegistry, WinetricksError};

async fn install_verb(config: &Config, verb_name: &str) -> Result<()> {
    let mut executor = Executor::new(config.clone()).await?;
    executor.install_verb(verb_name).await
}

async fn uninstall_verb(config: &Config, verb_name: &str) -> Result<()> {
    let mut executor = Executor::new(config.clone()).await?;
    executor.uninstall_verb(verb_name).await
}

fn print_help() {
    println!(
        r#"Winetricks - Package manager for Wine
A fast, modern rewrite of winetricks in Rust

Usage: winetricks [OPTIONS] [COMMAND|VERB] ...

Executes verbs to install applications, DLLs, fonts, or change Wine settings.

COMMANDS:
    list                  List categories
    list-all              List all categories and their verbs
    list-installed        List already-installed verbs
    list-cached           List verbs with cached files
    list-download         List verbs that auto-download (media=download)
    list-manual-download  List verbs that require manual download
    
    apps list             List verbs in category 'applications'
    benchmarks list       List verbs in category 'benchmarks'
    dlls list             List verbs in category 'dlls'
    fonts list            List verbs in category 'fonts'
    settings list         List verbs in category 'settings'
    
    reinstall VERB        Force reinstall a verb (removes from log, reinstalls)
    uninstall VERB        Uninstall a verb (removes from log, attempts cleanup)
    help                  Open winetricks wiki in browser
    folder                Open WINEPREFIX in file manager
    winecfg               Run Wine configuration GUI
    regedit               Run Windows registry editor
    taskmgr               Run Windows task manager
    explorer              Run Windows explorer
    uninstaller           Run Windows uninstaller
    shell                 Open interactive Wine shell
    winecmd               Open Wine command prompt (cmd.exe)
    prefix=NAME           Select WINEPREFIX
    arch=32|64            Set wine architecture (win32/win64)
    renderer=opengl|vulkan Set Wine D3D renderer (opengl or vulkan)
    annihilate            Delete WINEPREFIX (WARNING: deletes all data!)
    
    VERB_NAME             Install a verb (e.g., dotnet48, vcrun2019, corefonts)

OPTIONS:
    --country=CC          Set country code to CC
    -f, --force           Don't check whether packages were already installed
    --gui                 Show GUI diagnostics (GUI not yet implemented)
    --gui=OPT             Set GUI engine (kdialog or zenity)
    --isolate             Install each app in its own WINEPREFIX
    --no-clean            Don't delete temp directories
    --no-isolate          Don't isolate apps (use shared prefix)
    --optin               Opt in to reporting
    --optout              Opt out of reporting
    -q, --unattended      Don't ask any questions, install automatically
    --self-update         Update this application (coming soon)
    --update-rollback     Rollback last self update (coming soon)
    -t, --torify          Run downloads under torify, if available
    --verify              Run automated GUI tests (coming soon)
    -v, --verbose         Echo all commands as they are executed
    -vv, --really-verbose Really verbose mode
    -h, --help            Display this message and exit
    -V, --version         Display version and exit

EXAMPLES:
    winetricks list                          # List categories
    winetricks dlls list                     # List DLL verbs
    winetricks dotnet48                      # Install .NET 4.8
    winetricks -q vcrun2019                  # Install silently
    winetricks --force corefonts             # Force reinstall
    winetricks reinstall dotnet48            # Force reinstall (same as --force)
    winetricks uninstall dotnet48            # Uninstall a verb
    winetricks list-installed                # Show installed verbs with details
    winetricks help                          # Open wiki in browser
    winetricks annihilate                    # Delete WINEPREFIX (with confirmation)
    winetricks prefix=myprefix dotnet48      # Install to custom prefix
    winetricks renderer=vulkan dotnet48      # Install with Vulkan renderer
    winetricks wayland=wayland dotnet48      # Install using Wayland display driver

NOTE: This is a pure Rust rewrite of winetricks.
      All verb installations are handled directly by the Rust implementation.
"#
    );
}

#[derive(Parser)]
#[command(name = "winetricks")]
#[command(about = "A fast, modern package manager for Wine")]
#[command(version)]
#[command(long_about = r#"Winetricks - Package manager for Wine

Executes verbs to install applications, DLLs, fonts, or change Wine settings.

COMMANDS:
    list                  List categories
    list-all              List all categories and their verbs
    list-installed        List already-installed verbs
    list-cached           List verbs with cached files
    list-download         List verbs that auto-download (media=download)
    list-manual-download  List verbs that require manual download
    
    apps list             List verbs in category 'applications'
    benchmarks list       List verbs in category 'benchmarks'
    dlls list             List verbs in category 'dlls'
    fonts list            List verbs in category 'fonts'
    settings list         List verbs in category 'settings'
    
    reinstall VERB        Force reinstall a verb (removes from log, reinstalls)
    uninstall VERB        Uninstall a verb (removes from log, attempts cleanup)
    help                  Open winetricks wiki in browser
    folder                Open WINEPREFIX in file manager
    winecfg               Run Wine configuration GUI
    regedit               Run Windows registry editor
    taskmgr               Run Windows task manager
    explorer              Run Windows explorer
    uninstaller           Run Windows uninstaller
    shell                 Open interactive Wine shell
    winecmd               Open Wine command prompt (cmd.exe)
    prefix=NAME           Select WINEPREFIX
    arch=32|64            Set wine architecture (win32/win64)
    renderer=opengl|vulkan Set Wine D3D renderer (opengl or vulkan)
    annihilate            Delete WINEPREFIX (WARNING: deletes all data!)
    
    VERB_NAME             Install a verb (e.g., dotnet48, vcrun2019, corefonts)

EXAMPLES:
    winetricks list                          # List categories
    winetricks dlls list                     # List DLL verbs
    winetricks dotnet48                      # Install .NET 4.8
    winetricks -q vcrun2019                  # Install silently
    winetricks --force corefonts             # Force reinstall
    winetricks reinstall dotnet48            # Force reinstall (same as --force)
    winetricks uninstall dotnet48            # Uninstall a verb
    winetricks help                          # Open wiki in browser
    winetricks annihilate                    # Delete WINEPREFIX (with confirmation)
    winetricks prefix=myprefix dotnet48      # Install to custom prefix"#)]
struct Cli {
    /// Commands or verbs to execute
    #[arg(trailing_var_arg = true, help = "Command or verb name(s) to execute")]
    commands: Vec<String>,

    /// Set country code to CC and don't detect your IP address
    #[arg(long)]
    country: Option<String>,

    /// Don't check whether packages were already installed
    #[arg(short = 'f', long)]
    force: bool,

    /// Show gui diagnostics even when driven by commandline
    #[arg(long)]
    gui: bool,

    /// Set GUI engine (kdialog or zenity) to override
    #[arg(long)]
    gui_opt: Option<String>,

    /// Install each app or game in its own bottle (WINEPREFIX)
    #[arg(long)]
    isolate: bool,

    /// Don't delete temp directories (useful during debugging)
    #[arg(long)]
    no_clean: bool,

    /// Don't isolate apps (use shared prefix)
    #[arg(long)]
    no_isolate: bool,

    /// Opt in to reporting which verbs you use to the Winetricks maintainers
    #[arg(long)]
    optin: bool,

    /// Opt out of reporting which verbs you use to the Winetricks maintainers
    #[arg(long)]
    optout: bool,

    /// Don't ask any questions, just install automatically
    #[arg(short = 'q', long)]
    unattended: bool,

    /// Update this application to the last version
    #[arg(long)]
    self_update: bool,

    /// Rollback the last self update
    #[arg(long)]
    update_rollback: bool,

    /// Run downloads under torify, if available
    #[arg(short = 't', long)]
    torify: bool,

    /// Run (automated) GUI tests for verbs, if available
    #[arg(long)]
    verify: bool,

    /// Echo all commands as they are executed
    #[arg(short = 'v', long)]
    verbose: bool,

    /// Really verbose (set -x equivalent)
    #[arg(short = 'v', long = "really-verbose", action = clap::ArgAction::Count)]
    really_verbose: u8,

    /// Display this message and exit
    #[arg(short = 'h', long)]
    help: bool,

    /// Display version and exit
    #[arg(short = 'V', long)]
    version: bool,
}

// Commands are parsed from the commands vector
// They can be: list, list-all, list-cached, list-download, list-manual-download,
// list-installed, apps list, dlls list, fonts list, settings list, benchmarks list,
// arch=32|64, prefix=foobar, annihilate, folder, winecfg, regedit, taskmgr, explorer,
// uninstaller, shell, winecmd, or verb names

#[tokio::main]
async fn main() -> Result<()> {
    // Check arguments - if no arguments or only program name, launch GUI
    let args: Vec<String> = std::env::args().collect();

    // If no arguments provided (just program name), launch GUI
    if args.len() == 1 {
        // Launch GUI
        let gui_paths = [
            // Try same directory as winetricks binary
            std::env::current_exe().ok().and_then(|mut path| {
                path.set_file_name("winetricks-gui");
                if path.exists() {
                    Some(path)
                } else {
                    None
                }
            }),
            // Try /usr/bin/winetricks-gui
            Some(std::path::PathBuf::from("/usr/bin/winetricks-gui")),
            // Try /usr/local/bin/winetricks-gui
            Some(std::path::PathBuf::from("/usr/local/bin/winetricks-gui")),
        ];

        for gui_path in gui_paths.iter().flatten() {
            if gui_path.exists() {
                if let Err(e) = std::process::Command::new(gui_path).spawn() {
                    eprintln!("Failed to launch GUI: {}", e);
                    eprintln!("Falling back to CLI help...");
                    break;
                } else {
                    return Ok(());
                }
            }
        }

        // GUI not found, show help
        eprintln!("Winetricks GUI not found. Use 'winetricks --help' for CLI usage.");
        eprintln!("To use GUI: install winetricks-gui alongside winetricks.");
        print_help();
        return Ok(());
    }

    // Arguments provided - use CLI
    let cli = Cli::parse();

    // Handle version and help early (before logging)
    if cli.version {
        println!("winetricks 0.1.0 (Rust rewrite)");
        return Ok(());
    }

    // Handle help manually since clap's auto-help might not work with trailing_var_arg
    if cli.help {
        print_help();
        return Ok(());
    }

    // Determine verbosity level
    // If unattended (-q), suppress all logging unless verbose is explicitly set
    let verbosity = if cli.really_verbose > 0 {
        2
    } else if cli.verbose {
        1
    } else {
        0
    };

    // Setup logging (only if not showing help/version)
    // Unattended mode still shows progress, just suppresses GUI and prompts
    let log_level = match verbosity {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };

    tracing_subscriber::fmt()
        .with_env_filter(format!("winetricks={}", log_level))
        .init();

    // Load configuration
    let mut config = Config::new()?;
    config.verbosity = verbosity;
    config.force = cli.force;
    config.unattended = cli.unattended;
    config.torify = cli.torify;
    config.isolate = cli.isolate;
    config.no_clean = cli.no_clean;

    // --no-isolate overrides --isolate if both are set
    if cli.no_isolate {
        config.isolate = false;
    }

    // Handle WINEPREFIX from environment or prefix= command
    if let Ok(prefix) = std::env::var("WINEPREFIX") {
        config.wineprefix = Some(prefix.into());
    }

    // Set WINEARCH from config if specified
    if let Some(ref arch) = config.winearch {
        std::env::set_var("WINEARCH", arch);
    }

    // Set WINE_D3D_CONFIG from config if specified
    // Wine uses WINE_D3D_CONFIG="renderer=<value>" format
    // Valid values: gl (OpenGL/wined3d), vulkan, gdi, no3d, etc.
    if let Some(ref renderer) = config.renderer {
        // Normalize renderer name to Wine format
        let wine_renderer = match renderer.to_lowercase().as_str() {
            "opengl" | "gl" | "w" => "gl",     // Wine uses 'gl' for OpenGL
            "vulkan" | "vk" | "v" => "vulkan", // Wine uses 'vulkan' for Vulkan
            "gdi" => "gdi",
            "no3d" => "no3d",
            _ => {
                eprintln!(
                    "Warning: Unknown renderer '{}'. Using '{}'.",
                    renderer, renderer
                );
                renderer.as_str()
            }
        };
        std::env::set_var("WINE_D3D_CONFIG", &format!("renderer={}", wine_renderer));
        info!("Set WINE_D3D_CONFIG=renderer={}", wine_renderer);
    }

    // Set DISPLAY environment variable for Wayland/XWayland
    // Wayland: DISPLAY="" or unset
    // XWayland: DISPLAY=":0" (use current X11 display)
    if let Some(ref wayland) = config.wayland {
        match wayland.to_lowercase().as_str() {
            "wayland" => {
                // Force Wayland by unsetting DISPLAY
                std::env::remove_var("DISPLAY");
                info!("Set DISPLAY= (using Wayland)");
            }
            "xwayland" | "x11" => {
                // Use XWayland - set DISPLAY to current or default
                let display_val = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
                std::env::set_var("DISPLAY", &display_val);
                info!("Set DISPLAY={} (using XWayland)", display_val);
            }
            "auto" | _ => {
                // Auto - don't modify DISPLAY, let system decide
                info!("Using auto display server detection");
            }
        }
    }

    config.ensure_dirs()?;

    // Initialize cache from source JSON files if needed (or download from GitHub)
    config.ensure_cache_initialized().await?;

    // Show startup message
    info!("Winetricks starting...");

    // Debug: Log what commands were parsed
    if !cli.commands.is_empty() {
        info!("Parsed commands: {:?}", cli.commands);
    }
    info!(
        "Force flag: {}, Unattended flag: {}",
        cli.force, cli.unattended
    );

    // Parse commands
    // If only flags were provided without commands, show help
    // (GUI should only launch when NO arguments at all, which is handled earlier)
    if cli.commands.is_empty() {
        if cli.force || cli.unattended || cli.verbose || cli.torify || cli.isolate || cli.no_clean {
            eprintln!("Error: Flags provided but no command/verb specified.");
            eprintln!("Usage: winetricks [FLAGS] <command|verb>");
            eprintln!("Example: winetricks --force -q dotnet48");
            return Err(WinetricksError::Config("No command specified".into()));
        }
        println!("No commands specified. Use --help for usage.");
        return Ok(());
    }

    // Process commands in order - handle arch= and prefix= first
    let mut i = 0;
    while i < cli.commands.len() {
        let cmd = &cli.commands[i];

        // Process arch= BEFORE prefix= (arch must be set before prefix creation)
        if let Some(arch) = cmd.strip_prefix("arch=") {
            // Set wine architecture (must be before prefix= command)
            let winearch = match arch {
                "32" | "win32" => "win32",
                "64" | "win64" => "win64",
                _ => {
                    eprintln!(
                        "Error: Invalid architecture '{}'. Use 32, 64, win32, or win64",
                        arch
                    );
                    std::process::exit(1);
                }
            };

            config.winearch = Some(winearch.to_string());
            std::env::set_var("WINEARCH", winearch);
            info!("Set WINEARCH={}", winearch);
            i += 1;
            continue;
        }

        // Process renderer= command (can be set anytime)
        if let Some(renderer_val) = cmd.strip_prefix("renderer=") {
            let renderer = match renderer_val.to_lowercase().as_str() {
                "opengl" | "gl" | "w" => "opengl",
                "vulkan" | "vk" | "v" => "vulkan",
                _ => {
                    eprintln!(
                        "Error: Invalid renderer '{}'. Use opengl or vulkan",
                        renderer_val
                    );
                    std::process::exit(1);
                }
            };

            config.renderer = Some(renderer.to_string());
            // Convert to Wine environment variable format
            // Wine uses WINE_D3D_CONFIG="renderer=<value>" format
            let wine_renderer = match renderer {
                "opengl" => "gl",     // Wine uses 'gl' for OpenGL
                "vulkan" => "vulkan", // Wine uses 'vulkan' for Vulkan
                _ => unreachable!(),
            };
            std::env::set_var("WINE_D3D_CONFIG", &format!("renderer={}", wine_renderer));

            // Also set in wineprefix registry for persistence
            if let Err(e) = config.set_renderer_in_registry(Some(renderer)) {
                warn!(
                    "Failed to set renderer in registry (will use environment variable): {}",
                    e
                );
            }

            info!(
                "Set WINE_D3D_CONFIG=renderer={} (renderer={})",
                wine_renderer, renderer
            );
            i += 1;
            continue;
        }

        // Process wayland= command (can be set anytime)
        if let Some(wayland_val) = cmd.strip_prefix("wayland=") {
            let wayland = match wayland_val.to_lowercase().as_str() {
                "wayland" => "wayland",
                "xwayland" | "x11" => "xwayland",
                "auto" => "auto",
                _ => {
                    eprintln!(
                        "Error: Invalid wayland value '{}'. Use wayland, xwayland, or auto",
                        wayland_val
                    );
                    std::process::exit(1);
                }
            };

            config.wayland = Some(wayland.to_string());

            // Set DISPLAY environment variable
            match wayland {
                "wayland" => {
                    std::env::remove_var("DISPLAY");
                }
                "xwayland" => {
                    let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
                    std::env::set_var("DISPLAY", &display);
                }
                _ => {} // Auto - don't modify
            }

            // Also set in wineprefix registry for persistence (or clear if auto)
            let wayland_for_registry = if wayland == "auto" {
                None
            } else {
                Some(wayland)
            };
            if let Err(e) = config.set_wayland_in_registry(wayland_for_registry) {
                warn!(
                    "Failed to set Graphics driver in registry (will use environment variable): {}",
                    e
                );
            }

            info!("Set wayland={}", wayland);
            i += 1;
            continue;
        }

        if let Some(prefix_name) = cmd.strip_prefix("prefix=") {
            let prefix_path = config.prefixes_root.join(prefix_name);
            config.wineprefix = Some(prefix_path.clone());
            std::env::set_var("WINEPREFIX", prefix_path.to_str().unwrap());

            // If WINEARCH is set and prefix doesn't exist, initialize it
            if let Some(ref arch) = config.winearch {
                if !prefix_path.exists() {
                    info!(
                        "Creating WINEPREFIX \"{}\" with WINEARCH={}",
                        prefix_path.display(),
                        arch
                    );
                    // Initialize prefix with wineboot
                    let wine = winetricks_lib::Wine::detect()?;
                    std::process::Command::new(&wine.wine_bin)
                        .arg("wineboot")
                        .env("WINEPREFIX", prefix_path.to_str().unwrap())
                        .env("WINEARCH", arch)
                        .status()?;
                    // Wait for wineserver to finish
                    std::process::Command::new(&wine.wineserver_bin)
                        .arg("-w")
                        .env("WINEPREFIX", prefix_path.to_str().unwrap())
                        .status()?;
                }
            }
            i += 1;
            continue;
        }

        // Handle special commands (check these BEFORE treating as verb)
        match cmd.as_str() {
            "reinstall" => {
                // reinstall command: reinstall VERB_NAME
                if i + 1 >= cli.commands.len() {
                    eprintln!("Error: reinstall requires a verb name");
                    eprintln!("Usage: winetricks reinstall <verb-name>");
                    std::process::exit(1);
                }

                let verb_name = &cli.commands[i + 1];
                config.force = true; // Enable force for reinstall

                match install_verb(&config, verb_name).await {
                    Ok(_) => {
                        println!("Successfully reinstalled {}", verb_name);
                    }
                    Err(e) => {
                        eprintln!("Error reinstalling {}: {}", verb_name, e);
                        std::process::exit(1);
                    }
                }
                i += 1; // Skip the verb name
                continue; // Continue to next iteration
            }
            "list" => {
                println!("Categories: apps benchmarks dlls fonts settings");
            }
            "list-all" => {
                let metadata_dir = config.metadata_dir();
                if metadata_dir.exists() {
                    let registry = VerbRegistry::load_from_dir(metadata_dir)?;
                    let categories = [
                        VerbCategory::Apps,
                        VerbCategory::Benchmarks,
                        VerbCategory::Dlls,
                        VerbCategory::Fonts,
                        VerbCategory::Settings,
                    ];
                    for category in categories {
                        println!("===== {} =====", category.as_str());
                        let verbs = registry.list_by_category(category);
                        for verb in verbs {
                            println!("{}", verb.name);
                        }
                    }
                }
            }
            "list-cached" => {
                let metadata_dir = config.metadata_dir();
                if !metadata_dir.exists() {
                    eprintln!(
                        "Error: metadata directory not found: {}",
                        metadata_dir.display()
                    );
                    return Ok(());
                }

                let registry = VerbRegistry::load_from_dir(metadata_dir)?;
                let mut cached_verbs = Vec::new();

                // Check each verb to see if its files are cached
                for category in [
                    VerbCategory::Apps,
                    VerbCategory::Benchmarks,
                    VerbCategory::Dlls,
                    VerbCategory::Fonts,
                    VerbCategory::Settings,
                ] {
                    for verb_metadata in registry.list_by_category(category) {
                        // A verb is cached if all its files exist in cache
                        let mut all_cached = true;
                        if !verb_metadata.files.is_empty() {
                            for file in &verb_metadata.files {
                                // Check if file is cached - original winetricks uses verb_name/filename structure
                                // For now, check both verb_name/filename and just filename
                                let cache_file = config
                                    .cache_dir
                                    .join(&verb_metadata.name)
                                    .join(&file.filename);
                                let cache_file_alt = config.cache_dir.join(&file.filename);

                                if !cache_file.exists() && !cache_file_alt.exists() {
                                    all_cached = false;
                                    break;
                                }
                            }
                        } else {
                            // Verb has no files to download, skip it
                            all_cached = false;
                        }

                        if all_cached {
                            cached_verbs.push(verb_metadata.name.clone());
                        }
                    }
                }

                cached_verbs.sort();
                for verb_name in cached_verbs {
                    println!("{}", verb_name);
                }
            }
            "list-download" => {
                let metadata_dir = config.metadata_dir();
                if !metadata_dir.exists() {
                    eprintln!(
                        "Error: metadata directory not found: {}",
                        metadata_dir.display()
                    );
                    return Ok(());
                }

                let registry = VerbRegistry::load_from_dir(metadata_dir)?;
                let mut download_verbs = Vec::new();

                // List verbs with media=download
                for category in [
                    VerbCategory::Apps,
                    VerbCategory::Benchmarks,
                    VerbCategory::Dlls,
                    VerbCategory::Fonts,
                    VerbCategory::Settings,
                ] {
                    for verb_metadata in registry.list_by_category(category) {
                        if verb_metadata.media == winetricks_lib::MediaType::Download {
                            download_verbs.push(verb_metadata.name.clone());
                        }
                    }
                }

                download_verbs.sort();
                for verb_name in download_verbs {
                    println!("{}", verb_name);
                }
            }
            "list-manual-download" => {
                let metadata_dir = config.metadata_dir();
                if !metadata_dir.exists() {
                    eprintln!(
                        "Error: metadata directory not found: {}",
                        metadata_dir.display()
                    );
                    return Ok(());
                }

                let registry = VerbRegistry::load_from_dir(metadata_dir)?;
                let mut manual_download_verbs = Vec::new();

                // List verbs with media=manual_download
                for category in [
                    VerbCategory::Apps,
                    VerbCategory::Benchmarks,
                    VerbCategory::Dlls,
                    VerbCategory::Fonts,
                    VerbCategory::Settings,
                ] {
                    for verb_metadata in registry.list_by_category(category) {
                        if verb_metadata.media == winetricks_lib::MediaType::ManualDownload {
                            manual_download_verbs.push(verb_metadata.name.clone());
                        }
                    }
                }

                manual_download_verbs.sort();
                for verb_name in manual_download_verbs {
                    println!("{}", verb_name);
                }
            }
            "list-installed" => {
                let wineprefix = config.wineprefix();
                let log_file = wineprefix.join("winetricks.log");
                if log_file.exists() {
                    let content = std::fs::read_to_string(&log_file)?;
                    let installed: Vec<&str> = content.lines()
                        .map(|l| l.trim())
                        .filter(|l| {
                            // Filter out empty lines, flags (starting with -), comments, and command keywords
                            !l.is_empty()
                            && !l.starts_with('-')  // Flags like -q, --force
                            && !l.starts_with('#')  // Comments
                            && !l.starts_with("//") // Comments
                            && !l.contains('=')     // Commands like prefix=, arch=
                            && l != &"list" && l != &"list-installed" && l != &"list-all" 
                            && l != &"list-cached" && l != &"list-download" && l != &"list-manual-download"
                            && l != &"apps" && l != &"dlls" && l != &"fonts" && l != &"settings" && l != &"benchmarks"
                            && l != &"annihilate" && l != &"folder" && l != &"winecfg" && l != &"regedit"
                            && l != &"taskmgr" && l != &"explorer" && l != &"uninstaller" && l != &"shell"
                            && l != &"winecmd" && l != &"help" && l != &"uninstall" && l != &"reinstall"
                        })
                        .collect();

                    if installed.is_empty() {
                        println!("No verbs installed in this wineprefix");
                    } else {
                        println!("Installed verbs ({}):", installed.len());
                        println!("{}", "=".repeat(50));

                        // Try to show metadata if available
                        let metadata_dir = config.metadata_dir();
                        let registry = if metadata_dir.exists() {
                            VerbRegistry::load_from_dir(metadata_dir).ok()
                        } else {
                            None
                        };

                        for verb_name in &installed {
                            if let Some(registry) = &registry {
                                if let Some(metadata) = registry.get(verb_name) {
                                    println!(
                                        "  {} - {} ({})",
                                        verb_name,
                                        metadata.title,
                                        metadata.category.as_str()
                                    );
                                } else {
                                    println!("  {}", verb_name);
                                }
                            } else {
                                println!("  {}", verb_name);
                            }
                        }
                    }
                } else {
                    println!("No installation log found at {:?}", log_file);
                    println!("No verbs have been installed in this wineprefix.");
                }
            }
            "uninstall" => {
                // uninstall command: uninstall VERB_NAME
                if i + 1 >= cli.commands.len() {
                    eprintln!("Error: uninstall requires a verb name");
                    eprintln!("Usage: winetricks uninstall <verb-name>");
                    eprintln!(
                        "       winetricks uninstall <verb-name> <verb-name> ...  (multiple verbs)"
                    );
                    std::process::exit(1);
                }

                // Process all verbs after "uninstall"
                let mut uninstalled = Vec::new();
                let mut failed = Vec::new();

                i += 1; // Move past "uninstall"
                while i < cli.commands.len() && !cli.commands[i].starts_with("-") {
                    let verb_name = &cli.commands[i];

                    // Skip if it's another command
                    if [
                        "list",
                        "reinstall",
                        "uninstall",
                        "apps",
                        "dlls",
                        "fonts",
                        "settings",
                        "benchmarks",
                        "prefix=",
                        "arch=",
                        "renderer=",
                        "wayland=",
                    ]
                    .iter()
                    .any(|&cmd| {
                        verb_name.starts_with(cmd) || verb_name == cmd.trim_end_matches('=')
                    }) {
                        break;
                    }

                    match uninstall_verb(&config, verb_name).await {
                        Ok(_) => {
                            uninstalled.push(verb_name.clone());
                        }
                        Err(e) => {
                            eprintln!("Error uninstalling {}: {}", verb_name, e);
                            failed.push(verb_name.clone());
                        }
                    }
                    i += 1;
                }

                if !uninstalled.is_empty() {
                    println!("\nSuccessfully uninstalled: {}", uninstalled.join(", "));
                }
                if !failed.is_empty() {
                    eprintln!("\nFailed to uninstall: {}", failed.join(", "));
                    std::process::exit(1);
                }

                continue; // Already incremented i
            }
            "apps" | "benchmarks" | "dlls" | "fonts" | "settings" => {
                // Check if next command is "list"
                if i + 1 < cli.commands.len() && cli.commands[i + 1] == "list" {
                    if let Ok(category) = VerbCategory::from_str(cmd) {
                        let metadata_dir = config.metadata_dir();
                        if metadata_dir.exists() {
                            let registry = VerbRegistry::load_from_dir(metadata_dir)?;
                            let verbs = registry.list_by_category(category);
                            for verb in verbs {
                                println!("{}", verb.name);
                            }
                        }
                    }
                    i += 1; // Skip "list"
                } else {
                    eprintln!("Category '{}' requires 'list' command: {} list", cmd, cmd);
                }
            }
            "help" => {
                // Open winetricks wiki in browser
                let url = "https://github.com/Winetricks/winetricks/wiki";
                let browsers = ["xdg-open", "sdtwebclient", "cygstart", "open", "firefox"];

                let mut opened = false;
                for browser in &browsers {
                    if std::process::Command::new(browser)
                        .arg(url)
                        .status()
                        .is_ok()
                    {
                        opened = true;
                        break;
                    }
                }

                if !opened {
                    eprintln!("Could not open browser. Please visit: {}", url);
                    std::process::exit(1);
                }
            }
            "annihilate" => {
                // DANGEROUS: Delete entire WINEPREFIX
                let wineprefix = config.wineprefix();

                // Ask for confirmation unless unattended
                if !config.unattended {
                    eprintln!("WARNING: This will DELETE ALL DATA AND APPLICATIONS inside:");
                    eprintln!("  {}", wineprefix.display());
                    eprintln!("This action cannot be undone!");
                    print!("Are you sure you want to continue? [y/N] ");
                    io::stdout().flush().unwrap();

                    let mut answer = String::new();
                    io::stdin().read_line(&mut answer).unwrap();

                    if !answer.trim().to_lowercase().starts_with('y') {
                        println!("Cancelled.");
                        return Ok(());
                    }
                } else {
                    eprintln!(
                        "WARNING: Unattended annihilate will delete: {}",
                        wineprefix.display()
                    );
                }

                // Delete wineprefix
                if wineprefix.exists() {
                    info!("Deleting wineprefix: {:?}", wineprefix);
                    std::fs::remove_dir_all(&wineprefix).map_err(|e| {
                        WinetricksError::Verb(format!("Failed to delete wineprefix: {}", e))
                    })?;
                } else {
                    eprintln!("Wineprefix does not exist: {}", wineprefix.display());
                    return Ok(());
                }

                // Clean up .desktop files in XDG_DATA_HOME/applications
                if let Ok(data_home) = std::env::var("XDG_DATA_HOME") {
                    let apps_dir = std::path::Path::new(&data_home).join("applications");
                    if apps_dir.exists() {
                        // Find and remove .desktop files referencing this wineprefix
                        if let Ok(entries) = std::fs::read_dir(&apps_dir) {
                            for entry in entries.flatten() {
                                if let Some(file_name) = entry.file_name().to_str() {
                                    if file_name.ends_with(".desktop") {
                                        if let Ok(content) = std::fs::read_to_string(entry.path()) {
                                            if content.contains(wineprefix.to_str().unwrap_or("")) {
                                                let _ = std::fs::remove_file(entry.path());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Clean up desktop items
                // Try XDG_DESKTOP_DIR env var first
                let desktop_path = if let Ok(desktop_dir) = std::env::var("XDG_DESKTOP_DIR") {
                    std::path::PathBuf::from(desktop_dir)
                } else {
                    // Try reading from user-dirs.dirs config file
                    let mut desktop_dir = None;
                    if let Ok(config_home) = std::env::var("XDG_CONFIG_HOME") {
                        let user_dirs = std::path::Path::new(&config_home).join("user-dirs.dirs");
                        if user_dirs.exists() {
                            if let Ok(content) = std::fs::read_to_string(&user_dirs) {
                                for line in content.lines() {
                                    if line.starts_with("XDG_DESKTOP_DIR=") {
                                        let value = line
                                            .trim_start_matches("XDG_DESKTOP_DIR=\"")
                                            .trim_end_matches("\"");
                                        // Expand $HOME if present
                                        if let Ok(home) = std::env::var("HOME") {
                                            desktop_dir = Some(value.replace("$HOME", &home));
                                        } else {
                                            desktop_dir = Some(value.to_string());
                                        }
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    // Fall back to ~/Desktop
                    desktop_dir
                        .unwrap_or_else(|| {
                            std::env::var("HOME")
                                .map(|h| format!("{}/Desktop", h))
                                .unwrap_or_else(|_| "~/Desktop".to_string())
                        })
                        .into()
                };

                if desktop_path.exists() {
                    if let Ok(entries) = std::fs::read_dir(&desktop_path) {
                        for entry in entries.flatten() {
                            if let Some(file_name) = entry.file_name().to_str() {
                                if file_name.ends_with(".desktop") {
                                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                                        if content.contains(wineprefix.to_str().unwrap_or("")) {
                                            let _ = std::fs::remove_file(entry.path());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                println!("Wineprefix deleted: {}", wineprefix.display());
                std::process::exit(0);
            }
            "folder" => {
                // Open wineprefix folder in file manager
                let wineprefix = config.wineprefix();
                let file_managers = ["xdg-open", "open", "cygstart"];

                let mut opened = false;
                for fm in &file_managers {
                    if process::Command::new(fm).arg(&wineprefix).status().is_ok() {
                        opened = true;
                        break;
                    }
                }

                if !opened {
                    eprintln!(
                        "Could not open file manager. Wineprefix location: {}",
                        wineprefix.display()
                    );
                    process::exit(1);
                }
            }
            "winecfg" => {
                // Run Wine configuration GUI
                let wine = winetricks_lib::Wine::detect()?;
                process::Command::new(&wine.wine_bin)
                    .arg("winecfg")
                    .env("WINEPREFIX", config.wineprefix())
                    .status()?;
            }
            "regedit" => {
                // Run Windows registry editor
                let wine = winetricks_lib::Wine::detect()?;
                let mut cmd = process::Command::new(&wine.wine_bin);
                cmd.arg("regedit").env("WINEPREFIX", config.wineprefix());

                // Add /S flag for silent mode in unattended mode
                if config.unattended {
                    cmd.arg("/S");
                }

                cmd.status()?;
            }
            "taskmgr" => {
                // Run Windows task manager (background)
                let wine = winetricks_lib::Wine::detect()?;
                process::Command::new(&wine.wine_bin)
                    .arg("taskmgr")
                    .env("WINEPREFIX", config.wineprefix())
                    .spawn()?;
                // Don't wait for completion (background process)
            }
            "explorer" => {
                // Run Windows explorer (background)
                let wine = winetricks_lib::Wine::detect()?;
                process::Command::new(&wine.wine_bin)
                    .arg("explorer")
                    .env("WINEPREFIX", config.wineprefix())
                    .spawn()?;
                // Don't wait for completion (background process)
            }
            "uninstaller" => {
                // Run Windows uninstaller
                let wine = winetricks_lib::Wine::detect()?;
                process::Command::new(&wine.wine_bin)
                    .arg("uninstaller")
                    .env("WINEPREFIX", config.wineprefix())
                    .status()?;
            }
            "shell" => {
                // Open interactive Wine shell
                let wine = winetricks_lib::Wine::detect()?;
                let wineprefix = config.wineprefix();

                // Try to find a terminal emulator
                let terminals = [
                    "gnome-terminal",
                    "konsole",
                    "Terminal",
                    "xterm",
                    "alacritty",
                    "kitty",
                ];
                let mut found_term = None;
                for term in &terminals {
                    if which::which(term).is_ok() {
                        found_term = Some(term);
                        break;
                    }
                }

                if let Some(term) = found_term {
                    // Launch terminal with shell
                    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
                    let wine_path = wine.wine_bin.to_string_lossy().to_string();
                    let prefix_path = wineprefix.to_string_lossy().to_string();

                    let args = match *term {
                        "gnome-terminal" => vec!["--".to_string(), shell],
                        _ => vec!["-e".to_string(), shell],
                    };

                    process::Command::new(*term)
                        .args(&args)
                        .env("WINEPREFIX", &prefix_path)
                        .env("WINE", &wine_path)
                        .env("WINEDEBUG", "-all")
                        .spawn()?;
                } else {
                    // Fall back to direct shell
                    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
                    let mut cmd = process::Command::new(&shell);
                    cmd.env("WINEPREFIX", &wineprefix)
                        .env("WINE", &wine.wine_bin)
                        .env("WINEDEBUG", "-all")
                        .spawn()?
                        .wait()?;
                }
            }
            "winecmd" => {
                // Open Wine command prompt
                let wine = winetricks_lib::Wine::detect()?;
                let wineprefix = config.wineprefix();

                // Try to find a terminal emulator
                let terminals = [
                    "gnome-terminal",
                    "konsole",
                    "Terminal",
                    "xterm",
                    "alacritty",
                    "kitty",
                ];
                let mut found_term = None;
                for term in &terminals {
                    if which::which(term).is_ok() {
                        found_term = Some(term);
                        break;
                    }
                }

                if let Some(term) = found_term {
                    let wine_path = wine.wine_bin.to_string_lossy().to_string();
                    let prefix_path = wineprefix.to_string_lossy().to_string();
                    let cmd_exe = "cmd.exe";

                    let args = match *term {
                        "gnome-terminal" => {
                            vec!["--".to_string(), wine_path.clone(), cmd_exe.to_string()]
                        }
                        _ => vec!["-e".to_string(), wine_path.clone(), cmd_exe.to_string()],
                    };

                    process::Command::new(*term)
                        .args(&args)
                        .env("WINEPREFIX", &prefix_path)
                        .env("WINE", &wine_path)
                        .env("WINEDEBUG", "-all")
                        .spawn()?;
                } else {
                    // Fall back to direct execution
                    process::Command::new(&wine.wine_bin)
                        .arg("cmd.exe")
                        .env("WINEPREFIX", &wineprefix)
                        .env("WINEDEBUG", "-all")
                        .status()?;
                }
            }
            _ => {
                // Try to parse as category list command (e.g., "apps list")
                let parts: Vec<&str> = cmd.split_whitespace().collect();
                if parts.len() == 2 && parts[1] == "list" {
                    if let Ok(category) = VerbCategory::from_str(parts[0]) {
                        let metadata_dir = config.metadata_dir();
                        if metadata_dir.exists() {
                            let registry = VerbRegistry::load_from_dir(metadata_dir)?;
                            let verbs = registry.list_by_category(category);
                            for verb in verbs {
                                println!("{}", verb.name);
                            }
                        }
                    }
                } else {
                    // Assume it's a verb name - try to install
                    let metadata_dir = config.metadata_dir();
                    if !metadata_dir.exists() {
                        eprintln!(
                            "Error: Verb metadata directory not found at {:?}",
                            metadata_dir
                        );
                        eprintln!("Winetricks-RS requires verb metadata in JSON format.");
                        eprintln!("You may need to convert verb definitions from the original winetricks script.");
                        std::process::exit(1);
                    }

                    match install_verb(&config, cmd).await {
                        Ok(_) => {
                            // Success - already printed by executor
                        }
                        Err(WinetricksError::VerbNotFound(_)) => {
                            eprintln!("Error: Verb '{}' not found", cmd);
                            eprintln!("Use 'winetricks list' to see available verbs.");
                            std::process::exit(1);
                        }
                        Err(e) => {
                            eprintln!("Error installing {}: {}", cmd, e);
                            std::process::exit(1);
                        }
                    }
                }
            }
        }
        i += 1;
    }

    // Handle self-update and rollback early (before other processing)
    if cli.self_update {
        return handle_self_update().await;
    }

    if cli.update_rollback {
        return handle_update_rollback().await;
    }

    Ok(())
}

async fn handle_self_update() -> Result<()> {
    use std::env;
    use std::process;

    // Check if we're in a dev environment
    let current_exe = env::current_exe().map_err(|e| {
        WinetricksError::Config(format!("Could not determine executable path: {}", e))
    })?;

    // Check if we're in a git repository (dev environment)
    let exe_dir = current_exe.parent().ok_or_else(|| {
        WinetricksError::Config("Could not determine executable directory".into())
    })?;

    if exe_dir.join("../.git").exists() || exe_dir.join("../../.git").exists() {
        eprintln!("Warning: You're running in a dev environment. Self-update is disabled.");
        eprintln!("Please update manually or build from source.");
        process::exit(1);
    }

    // Check permissions
    let exe_metadata = std::fs::metadata(&current_exe).map_err(|e| {
        WinetricksError::Config(format!("Could not read executable metadata: {}", e))
    })?;

    if exe_metadata.permissions().readonly() {
        eprintln!("Error: Executable is read-only. Cannot update.");
        eprintln!("Try running with sudo or as root.");
        process::exit(1);
    }

    // Check if parent directory is writable by trying to create a test file
    let test_file = exe_dir.join(".winetricks_update_test");
    if std::fs::write(&test_file, "test").is_ok() {
        let _ = std::fs::remove_file(&test_file);
    } else {
        eprintln!(
            "Error: Cannot write to executable directory: {}",
            exe_dir.display()
        );
        eprintln!("Try running with sudo or as root.");
        process::exit(1);
    }

    eprintln!("Self-update for Rust winetricks is not yet fully implemented.");
    eprintln!("For now, please update by:");
    eprintln!("  1. Pulling latest changes: git pull");
    eprintln!("  2. Rebuilding: cargo build --release");
    eprintln!("  3. Reinstalling the binary");
    eprintln!();
    eprintln!("Future versions will support downloading pre-built binaries from GitHub releases.");

    Ok(())
}

async fn handle_update_rollback() -> Result<()> {
    use std::env;
    use std::process;

    let current_exe = env::current_exe().map_err(|e| {
        WinetricksError::Config(format!("Could not determine executable path: {}", e))
    })?;

    let rollback_file = current_exe.with_extension("bak");

    if !rollback_file.exists() {
        eprintln!("No backup found. Nothing to rollback.");
        eprintln!("Backup file would be at: {}", rollback_file.display());
        process::exit(1);
    }

    eprintln!("Rollback for Rust winetricks is not yet fully implemented.");
    eprintln!("To rollback manually:");
    eprintln!("  1. Backup file exists at: {}", rollback_file.display());
    eprintln!("  2. Copy it to replace current executable");
    eprintln!();
    eprintln!("Example:");
    eprintln!(
        "  sudo cp {} {}",
        rollback_file.display(),
        current_exe.display()
    );
    eprintln!("  sudo chmod +x {}", current_exe.display());

    Ok(())
}
