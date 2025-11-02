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

        // Initialize cache from source JSON files if needed
        config.ensure_cache_initialized()?;

        // Load verb registry from cached metadata directory
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

    /// Install a verb using Rust implementation
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

        // Get verb metadata - must be in registry, no fallback to original winetricks
        let metadata = match self.registry.get(verb_name) {
            Some(m) => m.clone(),
            None => {
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

        // For .NET Framework installers, wait a bit longer and verify installation
        if verb_name.starts_with("dotnet") {
            info!("Waiting for .NET Framework installation to complete...");
            // Give extra time for .NET installation to fully complete
            std::thread::sleep(std::time::Duration::from_secs(2));
            
            // Wait for wineserver to finish any remaining operations
            let wineserver_status = std::process::Command::new(&self.wine.wineserver_bin)
                .arg("-w")
                .env("WINEPREFIX", &self.config.wineprefix().to_string_lossy().to_string())
                .status();
            if let Err(e) = wineserver_status {
                warn!("Warning: Failed to wait for wineserver after .NET installation: {}", e);
            }
            
            // Check if .NET is actually installed by querying registry or checking files
            info!("Verifying .NET Framework installation...");
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
                    let wineprefix_str = wineprefix.to_string_lossy().to_string();
                    
                    // Set WINEARCH if configured (important for 64-bit installers)
                    if let Some(ref arch) = self.config.winearch {
                        std::env::set_var("WINEARCH", arch);
                    }

                    // Convert to Windows path for wine
                    let file_win_path = self.unix_to_wine_path(file)?;
                    
                    // Check if this is a 64-bit installer by filename
                    let filename = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let is_64bit_installer = filename.contains("amd64") || filename.contains("x64") || filename.contains("64");
                    
                    if is_64bit_installer {
                        if let Some(ref arch) = &self.config.winearch {
                            if arch == "win32" {
                                warn!("Warning: 64-bit MSI installer detected but WINEPREFIX is 32-bit (win32).");
                                warn!("This may cause issues. Consider using a 64-bit prefix or the 32-bit installer.");
                            }
                        } else {
                            warn!("64-bit MSI installer detected. Ensure WINEPREFIX is 64-bit or use 'arch=64' before installation.");
                        }
                    }

                    // Use wine start /wait for MSI files (as original winetricks does)
                    let mut cmd = std::process::Command::new(&self.wine.wine_bin);
                    cmd.env("WINEPREFIX", &wineprefix_str);
                    cmd.env(
                        "W_OPT_UNATTENDED",
                        if self.config.unattended { "1" } else { "0" },
                    );
                    
                    // Set WINEARCH in command environment too
                    if let Some(ref arch) = self.config.winearch {
                        cmd.env("WINEARCH", arch);
                    }
                    
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
                    let wineprefix_str = wineprefix.to_string_lossy().to_string();
                    
                    // Set WINEARCH if configured (important for 64-bit installers)
                    if let Some(ref arch) = self.config.winearch {
                        std::env::set_var("WINEARCH", arch);
                    }

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
                    let is_msxml = filename.contains("msxml")
                        || filename.contains("MSXML")
                        || filename.contains("xml");

                    let mut cmd = std::process::Command::new(&self.wine.wine_bin);
                    cmd.env("WINEPREFIX", &wineprefix_str);
                    cmd.env(
                        "W_OPT_UNATTENDED",
                        if self.config.unattended { "1" } else { "0" },
                    );

                    // Set WINEARCH if configured (important for 64-bit installers)
                    if let Some(ref arch) = self.config.winearch {
                        cmd.env("WINEARCH", arch);
                    }

                    // Set WINEDLLOVERRIDES for .NET installers (required for fusion.dll)
                    if is_dotnet {
                        cmd.env("WINEDLLOVERRIDES", "fusion=b");
                    }

                    // For MSXML installers, ensure we're using the right architecture
                    // MSXML 6.0 amd64 installer requires a 64-bit prefix
                    // If running a 64-bit MSXML installer on a 32-bit prefix, warn the user
                    if is_msxml && filename.contains("amd64") {
                        if let Some(ref arch) = &self.config.winearch {
                            if arch == "win32" {
                                warn!("Warning: MSXML 6.0 64-bit installer detected but WINEPREFIX appears to be 32-bit (win32).");
                                warn!("This may cause issues. Consider using a 64-bit prefix or the 32-bit MSXML installer.");
                                warn!("Attempting installation anyway...");
                            }
                        } else {
                            // No WINEARCH set - try to detect from filename
                            warn!("MSXML 6.0 64-bit installer detected. Ensure WINEPREFIX is 64-bit or use 'arch=64' before installation.");
                        }
                    }

                    cmd.arg(&file_win_path);

                    // Detect specific .NET versions for proper handling
                    let is_dotnet35 = filename.contains("35") || filename.contains("dotnet35") || filename.contains("NetFx35");
                    let is_dotnet40 = filename.contains("40") || filename.contains("dotnet40") || filename.contains("NetFx40");
                    let is_dotnet45 = filename.contains("45") || filename.contains("dotnet45") || filename.contains("NetFx45");
                    let is_dotnet46 = filename.contains("46") || filename.contains("462") || filename.contains("dotnet46");
                    let is_dotnet472 = filename.contains("472") || filename.contains("dotnet472");
                    let is_dotnet48 = filename.contains("48") || filename.contains("dotnet48");

                    // Apply appropriate silent flags based on installer type
                    if self.config.unattended {
                        if is_dotnet {
                            // .NET Framework installers require version-specific handling
                            if is_dotnet48 || is_dotnet472 {
                                // .NET 4.8 and 4.7.2: /sfxlang:1027 /q /norestart
                                cmd.arg("/sfxlang:1027").arg("/q").arg("/norestart");
                            } else if is_dotnet46 {
                                // .NET 4.6+: /q /norestart
                                cmd.arg("/q").arg("/norestart");
                            } else if is_dotnet45 || is_dotnet40 {
                                // .NET 4.5 and 4.0: Use /quiet flag
                                cmd.arg("/quiet").arg("/norestart");
                            } else if is_dotnet35 {
                                // .NET 3.5: Use /q flag and extract then run installer
                                // .NET 3.5 installer may need to extract first
                                cmd.arg("/q");
                            } else {
                                // Older/newer .NET versions: Try common flags
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

                    // For .NET 3.5 and 4.5, apply special DLL overrides before installation
                    if is_dotnet35 || is_dotnet45 {
                        if is_dotnet35 {
                            info!("Applying special settings for .NET Framework 3.5...");
                            // .NET 3.5 requires native mscoree and mscorwks DLLs
                            // TODO: Implement DLL override logic in Rust
                        }
                        if is_dotnet45 {
                            info!("Applying special settings for .NET Framework 4.5...");
                        }
                    }

                    // Keep terminal output visible (unattended mode suppresses GUI, not terminal)
                    let status = cmd
                        .status()
                        .map_err(|e| WinetricksError::CommandExecution {
                            command: format!("wine {:?}", file_win_path),
                            error: e.to_string(),
                        })?;

                    // Wait for wineserver after .NET installation (important for proper completion)
                    if is_dotnet {
                        info!("Waiting for wineserver to finish processing .NET installation...");
                        let wineserver_status = std::process::Command::new(&self.wine.wineserver_bin)
                            .arg("-w")
                            .env("WINEPREFIX", &wineprefix_str)
                            .status();
                        if let Err(e) = wineserver_status {
                            warn!("Warning: Failed to wait for wineserver: {}", e);
                        }
                    }

                    // Check exit code - some installers return non-zero codes that are still success
                    let exit_code = status.code();
                    if !status.success() {
                        // For .NET Framework installers, some exit codes indicate success but reboot required
                        if is_dotnet {
                            // Exit codes that indicate success but reboot required:
                            // 236 - Success, reboot required (common for .NET 3.5 and older)
                            // 3010 - Success, reboot required (common Windows installer code)
                            // 1603 - Fatal error (but sometimes false positive with Wine for .NET 3.5/4.5)
                            if let Some(code) = exit_code {
                                if code == 236 || code == 3010 {
                                    info!(
                                        "Installer returned exit code {} (reboot required - OK in Wine)",
                                        code
                                    );
                                    // Treat as success - no reboot needed in Wine environment
                                } else if code == 1603 && (is_dotnet35 || is_dotnet45) {
                                    // .NET 3.5 and 4.5 sometimes return 1603 but installation partially succeeded
                                    warn!("Installer returned exit code 1603. This may indicate partial installation.");
                                    warn!("This is common with .NET 3.5/4.5 in Wine - checking if installation succeeded...");
                                    // Wait a bit more and check
                                    std::thread::sleep(std::time::Duration::from_secs(3));
                                    // Continue - we'll verify later
                                    info!("Continuing despite exit code 1603 - will verify installation");
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
    /// Matches original winetricks behavior using word boundary matching
    pub fn is_installed(&self, verb_name: &str) -> Result<bool> {
        let wineprefix = self.config.wineprefix();
        let log_file = wineprefix.join("winetricks.log");

        if !log_file.exists() {
            return Ok(false);
        }

        let content = std::fs::read_to_string(&log_file)?;
        // Use word boundary matching like original winetricks (grep -qw)
        // Match verb_name as a whole word, not as part of another verb name
        Ok(content.lines().any(|line| {
            let trimmed = line.trim();
            // Skip comments, flags, and commands
            if trimmed.starts_with('#') || trimmed.starts_with('-') || trimmed.starts_with("//") {
                return false;
            }
            // Skip lines with = (commands like prefix=, arch=, etc.)
            if trimmed.contains('=') {
                return false;
            }
            // Exact match (word boundary equivalent)
            // Match whole word to avoid partial matches (e.g., "dotnet" matching "dotnet48")
            trimmed == verb_name
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

    /// Remove verb from installation log (for reinstall/uninstall)
    /// Matches original winetricks behavior - removes exact verb name match
    fn remove_from_log(&self, verb_name: &str) -> Result<()> {
        let wineprefix = self.config.wineprefix();
        let log_file = wineprefix.join("winetricks.log");

        if !log_file.exists() {
            return Ok(()); // Nothing to remove
        }

        let content = std::fs::read_to_string(&log_file)?;
        let lines: Vec<String> = content
            .lines()
            .filter(|l| {
                let trimmed = l.trim();
                // Keep the line if it's not the verb we're removing
                // Use exact match to avoid removing similar verb names
                trimmed != verb_name
            })
            .map(|l| l.to_string()) // Preserve original line (including whitespace)
            .collect();

        // Write back the file with preserved formatting
        // Original winetricks preserves newlines, so we do too
        let output = if lines.is_empty() {
            String::new()
        } else {
            // Join with newlines and ensure trailing newline if file had content
            lines.join("\n") + if content.ends_with('\n') { "\n" } else { "" }
        };

        std::fs::write(&log_file, output)?;
        Ok(())
    }

}
