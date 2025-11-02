//! Verb execution engine

use crate::config::Config;
use crate::download::DownloadManager;
use crate::error::{Result, WinetricksError};
use crate::verb::{VerbCategory, VerbMetadata, VerbRegistry};
use crate::wine::Wine;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::{info, warn};

/// Verb executor
pub struct Executor {
    config: Config,
    wine: Wine,
    downloader: DownloadManager,
    registry: VerbRegistry,
}

impl Executor {
    /// Create a new executor
    pub async fn new(config: Config) -> Result<Self> {
        let wine = Wine::detect()?;
        let downloader = DownloadManager::new(config.cache_dir.clone())?;

        // Load verb registry if metadata directory exists
        let registry = if config.metadata_dir().exists() {
            VerbRegistry::load_from_dir(config.metadata_dir())?
        } else {
            VerbRegistry::new()
        };

        Ok(Self {
            config,
            wine,
            downloader,
            registry,
        })
    }

    /// Install a verb (hybrid mode: tries Rust implementation, falls back to original script)
    pub async fn install_verb(&mut self, verb_name: &str) -> Result<()> {
        let start_time = Instant::now();
        info!("Installing verb: {}", verb_name);

        // Set W_OPT_UNATTENDED environment variable for compatibility
        std::env::set_var(
            "W_OPT_UNATTENDED",
            if self.config.unattended { "1" } else { "0" },
        );

        // Set WINE_D3D_CONFIG if configured
        // Wine uses WINE_D3D_CONFIG="renderer=<value>" format
        if let Some(ref renderer) = self.config.renderer {
            let wine_renderer = match renderer.to_lowercase().as_str() {
                "opengl" | "gl" | "w" => "gl",
                "vulkan" | "vk" | "v" => "vulkan",
                "gdi" => "gdi",
                "no3d" => "no3d",
                _ => renderer.as_str(),
            };
            std::env::set_var("WINE_D3D_CONFIG", &format!("renderer={}", wine_renderer));
        }

        // Try to find original winetricks script for fallback
        let original_script = find_original_winetricks();

        // Get verb metadata
        let metadata = match self.registry.get(verb_name) {
            Some(m) => m.clone(),
            None => {
                // If metadata not found but original script exists, delegate to it
                if let Some(ref script) = original_script {
                    warn!(
                        "Verb '{}' not in Rust metadata, delegating to original winetricks",
                        verb_name
                    );
                    return self
                        .delegate_to_original_winetricks(script, verb_name)
                        .await;
                }
                return Err(WinetricksError::VerbNotFound(verb_name.to_string()));
            }
        };

        // Check if already installed
        if !self.config.force && self.is_installed(verb_name)? {
            println!("{} already installed, skipping", verb_name);
            println!("Use --force to reinstall");
            return Ok(());
        }

        // If force is enabled and verb is installed, remove from log first
        if self.config.force && self.is_installed(verb_name)? {
            info!("Force reinstall requested for {}", verb_name);
            self.remove_from_log(verb_name)?;
        }

        // Check conflicts
        if !self.config.force && !metadata.conflicts.is_empty() {
            for conflict in &metadata.conflicts {
                if self.is_installed(conflict)? {
                    return Err(WinetricksError::VerbConflict {
                        verb: verb_name.to_string(),
                        conflicting: conflict.clone(),
                    });
                }
            }
        }

        // Download files if needed
        let cache_dir = self.config.cache_dir.join(verb_name);
        std::fs::create_dir_all(&cache_dir)?;

        for file in &metadata.files {
            if let Some(ref url) = file.url {
                info!("Downloading {} from {}", file.filename, url);
                let _downloaded = self
                    .downloader
                    .download(
                        url,
                        &cache_dir.join(&file.filename),
                        file.sha256.as_deref(),
                        true,
                    )
                    .await?;
            }
        }

        // Execute verb installation
        // TODO: For now, we'll try to run common installation patterns
        // Eventually we need to parse verb definitions from the original script
        // or create a new verb definition format

        self.execute_verb_installation(&metadata, &cache_dir)
            .await?;

        // Verify installation
        if let Some(ref installed_file) = metadata.installed_file {
            // Check if file exists in wineprefix
            // TODO: Convert Windows path to Unix path and check
            info!("Verifying installation: {}", installed_file);
        }

        // Log installation
        self.log_installation(verb_name)?;

        // Calculate and display installation time
        let duration = start_time.elapsed();
        let duration_secs = duration.as_secs();
        let duration_millis = duration.subsec_millis();

        if duration_secs >= 60 {
            let minutes = duration_secs / 60;
            let seconds = duration_secs % 60;
            println!(
                "Successfully installed {} in {}m {}.{:03}s",
                verb_name, minutes, seconds, duration_millis
            );
        } else {
            println!(
                "Successfully installed {} in {}.{:03}s",
                verb_name, duration_secs, duration_millis
            );
        }

        Ok(())
    }

    /// Execute verb installation logic
    async fn execute_verb_installation(
        &self,
        metadata: &VerbMetadata,
        cache_dir: &Path,
    ) -> Result<()> {
        // This is a simplified version - real winetricks has per-verb logic
        // For now, try to detect installer type and run it

        let files: Vec<PathBuf> = metadata
            .files
            .iter()
            .map(|f| cache_dir.join(&f.filename))
            .collect();

        for file in &files {
            if !file.exists() {
                continue;
            }

            // Detect installer type by extension
            let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");

            match ext {
                "msi" => {
                    info!("Running MSI installer: {:?}", file);

                    // Run MSI installer using wine start /wait with msiexec
                    let wineprefix = self.config.wineprefix();
                    std::env::set_var("WINEPREFIX", &wineprefix);
                    std::env::set_var(
                        "W_OPT_UNATTENDED",
                        if self.config.unattended { "1" } else { "0" },
                    );

                    // Convert to Windows path for wine
                    let file_win_path = self.unix_to_wine_path(file)?;

                    // Use wine start /wait for MSI files (as original winetricks does)
                    let mut cmd = std::process::Command::new(&self.wine.wine_bin);
                    cmd.arg("start")
                        .arg("/wait")
                        .arg("msiexec.exe")
                        .arg("/i")
                        .arg(&file_win_path);

                    if self.config.unattended {
                        cmd.arg("/q"); // Quiet mode for MSI (suppresses GUI, not terminal output)
                    }

                    // Keep terminal output visible
                    let status = cmd
                        .status()
                        .map_err(|e| WinetricksError::CommandExecution {
                            command: format!("wine start /wait msiexec.exe /i {:?}", file_win_path),
                            error: e.to_string(),
                        })?;

                    if !status.success() {
                        return Err(WinetricksError::Verb(format!(
                            "MSI installer failed with exit code: {:?}",
                            status.code()
                        )));
                    }
                }
                "exe" => {
                    info!("Running EXE installer: {:?}", file);

                    // Run EXE installer in wine
                    let wineprefix = self.config.wineprefix();
                    std::env::set_var("WINEPREFIX", &wineprefix);
                    std::env::set_var(
                        "W_OPT_UNATTENDED",
                        if self.config.unattended { "1" } else { "0" },
                    );

                    // Convert to Windows path for wine
                    let file_win_path = self.unix_to_wine_path(file)?;

                    // Detect installer type and use appropriate silent flags
                    let filename = file.file_name().and_then(|n| n.to_str()).unwrap_or("");

                    let is_dotnet = filename.contains("dotnet")
                        || filename.contains("ndp")
                        || filename.starts_with("NDP");
                    let is_vcredist =
                        filename.contains("vcredist") || filename.contains("vc_redist");
                    let is_ie = filename.contains("IE")
                        || filename.contains("ie")
                        || filename.contains("internetexplorer");

                    let mut cmd = std::process::Command::new(&self.wine.wine_bin);

                    // Set WINEDLLOVERRIDES for .NET installers (required for fusion.dll)
                    if is_dotnet {
                        cmd.env("WINEDLLOVERRIDES", "fusion=b");
                    }

                    cmd.arg(&file_win_path);

                    // Apply appropriate silent flags based on installer type
                    if self.config.unattended {
                        if is_dotnet {
                            // .NET Framework installers: /sfxlang:1027 /q /norestart (for 4.7.2+)
                            // Older versions use: /q /c:"install.exe /q"
                            if filename.contains("48")
                                || filename.contains("472")
                                || filename.contains("46")
                                || filename.contains("462")
                            {
                                cmd.arg("/sfxlang:1027").arg("/q").arg("/norestart");
                            } else {
                                cmd.arg("/q").arg("/c:\"install.exe /q\"");
                            }
                        } else if is_vcredist {
                            // Visual C++ Redistributables
                            cmd.arg("/q");
                        } else if is_ie {
                            // Internet Explorer installers
                            cmd.arg("/quiet").arg("/forcerestart");
                        } else {
                            // Generic EXE installers - try common flags
                            // /S is common for NSIS installers, /q for others
                            // Try /q first as it's more universal for Windows installers
                            cmd.arg("/q");
                        }
                    }

                    // Keep terminal output visible (unattended mode suppresses GUI, not terminal)
                    let status = cmd
                        .status()
                        .map_err(|e| WinetricksError::CommandExecution {
                            command: format!("wine {:?}", file_win_path),
                            error: e.to_string(),
                        })?;

                    // Check exit code - some installers return non-zero codes that are still success
                    let exit_code = status.code();
                    if !status.success() {
                        // For .NET Framework installers, some exit codes indicate success but reboot required
                        if is_dotnet {
                            // Exit codes that indicate success but reboot required:
                            // 236 - Success, reboot required (common for .NET 3.5)
                            // 3010 - Success, reboot required (common Windows installer code)
                            if let Some(code) = exit_code {
                                if code == 236 || code == 3010 {
                                    info!(
                                        "Installer returned exit code {} (reboot required - OK in Wine)",
                                        code
                                    );
                                    // Treat as success - no reboot needed in Wine environment
                                } else {
                                    // Other non-zero codes are still failures
                                    return Err(WinetricksError::Verb(format!(
                                        "EXE installer failed with exit code: {:?}",
                                        exit_code
                                    )));
                                }
                            } else {
                                // No exit code available - treat as failure
                                return Err(WinetricksError::Verb(
                                    "EXE installer failed (no exit code available)".into(),
                                ));
                            }
                        } else {
                            // For non-.NET installers, all non-zero codes are failures
                            return Err(WinetricksError::Verb(format!(
                                "EXE installer failed with exit code: {:?}",
                                exit_code
                            )));
                        }
                    }
                }
                "zip" => {
                    info!("Extracting zip: {:?}", file);
                    // TODO: Extract zip to wineprefix
                    return Err(WinetricksError::Verb(
                        "ZIP extraction not yet implemented".into(),
                    ));
                }
                "cab" => {
                    info!("Extracting cab: {:?}", file);
                    // TODO: Extract cab to wineprefix using cabextract
                    return Err(WinetricksError::Verb(
                        "CAB extraction not yet implemented".into(),
                    ));
                }
                _ => {
                    warn!("Unknown file type: {:?}", file);
                }
            }
        }

        Ok(())
    }

    /// Convert Unix path to Wine Windows path
    fn unix_to_wine_path(&self, unix_path: &Path) -> Result<String> {
        // Use winepath to convert Unix path to Windows path
        let wineprefix = self.config.wineprefix();
        std::env::set_var("WINEPREFIX", &wineprefix);

        let output = std::process::Command::new(&self.wine.wine_bin)
            .arg("winepath")
            .arg("-w")
            .arg(unix_path)
            .output()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine winepath -w {:?}", unix_path),
                error: e.to_string(),
            })?;

        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout);
            Ok(path.trim().to_string())
        } else {
            // Fallback: assume Z: drive mapping
            Ok(format!(
                "Z:\\{}",
                unix_path.to_string_lossy().replace('/', "\\")
            ))
        }
    }

    /// Check if verb is installed
    pub fn is_installed(&self, verb_name: &str) -> Result<bool> {
        let wineprefix = self.config.wineprefix();
        let log_file = wineprefix.join("winetricks.log");

        if !log_file.exists() {
            return Ok(false);
        }

        let content = std::fs::read_to_string(&log_file)?;
        Ok(content.lines().any(|line| {
            let trimmed = line.trim();
            // Only match actual verb names, not flags or commands
            trimmed == verb_name
            && !trimmed.starts_with('-')  // Not a flag
            && !trimmed.starts_with('#')  // Not a comment
            && !trimmed.contains('=') // Not a command like prefix=
        }))
    }

    /// Log installation to winetricks.log
    fn log_installation(&self, verb_name: &str) -> Result<()> {
        let wineprefix = self.config.wineprefix();
        let log_file = wineprefix.join("winetricks.log");

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)?;

        writeln!(file, "{}", verb_name)?;
        Ok(())
    }

    /// Uninstall a verb (removes from log, attempts cleanup)
    pub async fn uninstall_verb(&mut self, verb_name: &str) -> Result<()> {
        if !self.config.unattended {
            info!("Uninstalling verb: {}", verb_name);
        }

        // Check if installed
        if !self.is_installed(verb_name)? {
            if !self.config.unattended {
                println!("{} is not installed", verb_name);
            }
            return Ok(());
        }

        // Try to get metadata to see what type of verb it is
        if let Some(metadata) = self.registry.get(verb_name) {
            match metadata.category {
                VerbCategory::Apps => {
                    // For apps, try to find and run uninstaller
                    if !self.config.unattended {
                        println!("Attempting to uninstall application: {}", verb_name);
                    }
                    // TODO: Look for uninstaller in common locations
                    // For now, just remove from log
                    self.remove_from_log(verb_name)?;
                    if !self.config.unattended {
                        println!("Removed {} from installation log", verb_name);
                        println!("Note: Application files may still be present. Use Windows uninstaller if needed.");
                    }
                }
                VerbCategory::Dlls | VerbCategory::Fonts => {
                    // DLLs and fonts are harder to fully remove
                    if !self.config.unattended {
                        println!("Removing {} from installation log", verb_name);
                    }
                    self.remove_from_log(verb_name)?;
                    if !self.config.unattended {
                        println!("Note: DLL/Font files may still be present in wineprefix.");
                        println!("To fully remove, you may need to manually delete files or reset wineprefix.");
                    }
                }
                VerbCategory::Settings => {
                    // Settings can't really be "uninstalled" - just logged
                    if !self.config.unattended {
                        println!("Removing {} from installation log", verb_name);
                    }
                    self.remove_from_log(verb_name)?;
                    if !self.config.unattended {
                        println!("Note: Settings changes persist. Reset wineprefix to undo.");
                    }
                }
                _ => {
                    self.remove_from_log(verb_name)?;
                    if !self.config.unattended {
                        println!("Removed {} from installation log", verb_name);
                    }
                }
            }
        } else {
            // No metadata - just remove from log
            self.remove_from_log(verb_name)?;
            if !self.config.unattended {
                println!("Removed {} from installation log", verb_name);
            }
        }

        Ok(())
    }

    /// Remove verb from installation log (for reinstall)
    fn remove_from_log(&self, verb_name: &str) -> Result<()> {
        let wineprefix = self.config.wineprefix();
        let log_file = wineprefix.join("winetricks.log");

        if !log_file.exists() {
            return Ok(()); // Nothing to remove
        }

        let content = std::fs::read_to_string(&log_file)?;
        let lines: Vec<String> = content
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| l != verb_name && !l.is_empty())
            .collect();

        // Ensure newline at end if there are any lines
        let output = if lines.is_empty() {
            String::new()
        } else {
            lines.join("\n") + "\n"
        };

        std::fs::write(&log_file, output)?;
        Ok(())
    }

    /// Delegate verb installation to original winetricks script
    async fn delegate_to_original_winetricks(
        &self,
        script_path: &Path,
        verb_name: &str,
    ) -> Result<()> {
        let start_time = Instant::now();
        info!("Delegating {} to original winetricks script", verb_name);

        let wineprefix = self.config.wineprefix();
        std::env::set_var("WINEPREFIX", &wineprefix);
        std::env::set_var(
            "W_OPT_UNATTENDED",
            if self.config.unattended { "1" } else { "0" },
        );

        // Set WINE_D3D_CONFIG if configured
        // Wine uses WINE_D3D_CONFIG="renderer=<value>" format
        if let Some(ref renderer) = self.config.renderer {
            let wine_renderer = match renderer.to_lowercase().as_str() {
                "opengl" | "gl" | "w" => "gl",
                "vulkan" | "vk" | "v" => "vulkan",
                "gdi" => "gdi",
                "no3d" => "no3d",
                _ => renderer.as_str(),
            };
            std::env::set_var("WINE_D3D_CONFIG", &format!("renderer={}", wine_renderer));
        }

        // Set DISPLAY for Wayland/XWayland if configured
        if let Some(ref wayland) = self.config.wayland {
            match wayland.to_lowercase().as_str() {
                "wayland" => {
                    std::env::remove_var("DISPLAY");
                }
                "xwayland" | "x11" => {
                    let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
                    std::env::set_var("DISPLAY", &display);
                }
                _ => {} // Auto - don't modify
            }
        }

        let mut cmd = std::process::Command::new("sh");
        cmd.arg(script_path);

        if self.config.force {
            cmd.arg("--force");
        }
        if self.config.unattended {
            cmd.arg("--unattended");
        }
        if self.config.torify {
            cmd.arg("--torify");
        }
        if self.config.isolate {
            cmd.arg("--isolate");
        }
        if self.config.no_clean {
            cmd.arg("--no-clean");
        }

        cmd.arg(verb_name);

        // Keep output visible (unattended mode suppresses GUI, not terminal output)
        let status = cmd
            .status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("sh {:?} {}", script_path, verb_name),
                error: e.to_string(),
            })?;

        if status.success() {
            // Log installation
            self.log_installation(verb_name)?;

            // Calculate and display installation time
            let duration = start_time.elapsed();
            let duration_secs = duration.as_secs();
            let duration_millis = duration.subsec_millis();

            if duration_secs >= 60 {
                let minutes = duration_secs / 60;
                let seconds = duration_secs % 60;
                println!(
                    "Successfully installed {} (via original winetricks) in {}m {}.{:03}s",
                    verb_name, minutes, seconds, duration_millis
                );
            } else {
                println!(
                    "Successfully installed {} (via original winetricks) in {}.{:03}s",
                    verb_name, duration_secs, duration_millis
                );
            }
            Ok(())
        } else {
            Err(WinetricksError::Verb(format!(
                "Original winetricks failed with exit code: {:?}",
                status.code()
            )))
        }
    }
}

/// Find the original winetricks script (for hybrid mode fallback)
fn find_original_winetricks() -> Option<PathBuf> {
    // Try standard system locations first
    let candidates = [
        "/usr/bin/winetricks",
        "/usr/local/bin/winetricks",
        "/opt/local/bin/winetricks", // macOS
    ];

    for candidate in &candidates {
        let path = PathBuf::from(candidate);
        if path.exists() && path.is_file() {
            return Some(path);
        }
    }

    // Try to find it in PATH
    if let Ok(output) = std::process::Command::new("which")
        .arg("winetricks")
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout);
            let path = path.trim();
            if !path.is_empty() {
                return Some(PathBuf::from(path));
            }
        }
    }

    None
}
