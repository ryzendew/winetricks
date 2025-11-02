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
    /// Stored Windows version (for restore after installation)
    stored_windows_version: Option<String>,
}

impl Executor {
    /// Create a new executor
    pub async fn new(config: Config) -> Result<Self> {
        let wine = Wine::detect()?;
        let downloader = DownloadManager::new(config.cache_dir.clone())?;

        // Initialize cache from source JSON files if needed (or download from GitHub)
        config.ensure_cache_initialized().await?;

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
            stored_windows_version: None,
        })
    }

    /// Internal installation method (recursive, for prerequisites)
    async fn install_verb_internal(&mut self, verb_name: &str) -> Result<()> {
        // Check if already installed
        if !self.config.force && self.is_installed(verb_name)? {
            info!("{} is already installed, skipping", verb_name);
            return Ok(());
        }

        // Get verb metadata
        let metadata = self
            .registry
            .get(verb_name)
            .ok_or_else(|| WinetricksError::VerbNotFound(verb_name.to_string()))?
            .clone();

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
        self.execute_verb_installation(&metadata, &cache_dir)
            .await?;

        // Log installation
        self.log_installation(verb_name)?;

        Ok(())
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

        // Handle .NET specific prerequisites (from original winetricks)
        if verb_name == "dotnet48" {
            info!("Preparing for .NET 4.8 installation...");
            
            // 1. Remove Mono (prevents conflicts)
            if let Err(e) = self.install_verb_internal("remove_mono").await {
                warn!("Warning: Failed to remove Mono (may not be critical): {}", e);
            }
            
            // 2. Install .NET 4.0 first (required prerequisite)
            if let Err(e) = self.install_verb_internal("dotnet40").await {
                warn!("Warning: Failed to install .NET 4.0 prerequisite: {}", e);
                // Continue anyway, but warn
            }
            
            // 3. Set Windows version to Windows 7 (required for .NET 4.8)
            self.set_windows_version("win7")?;
        } else if verb_name == "dotnet35" || verb_name == "dotnet35sp1" {
            info!("Preparing for .NET 3.5 installation...");
            
            // 1. Remove Mono (prevents conflicts)
            if let Err(e) = self.install_verb_internal("remove_mono").await {
                warn!("Warning: Failed to remove Mono (may not be critical): {}", e);
            }
            
            // 2. Store current Windows version (to restore later)
            self.store_windows_version()?;
            
            // 3. Set Windows version to Windows XP (required for .NET 3.5)
            self.set_windows_version("winxp")?;
            
            // 4. Override DLLs BEFORE installation (critical for dotnet35)
            self.set_dll_override("mscoree", "native")?;
            self.set_dll_override("mscorwks", "native")?;
            
            // 5. Wait for wineserver BEFORE installation (critical for dotnet35)
            info!("Waiting for wineserver before .NET 3.5 installation...");
            let wineprefix_str = self.config.wineprefix().to_string_lossy().to_string();
            let wineserver_status = std::process::Command::new(&self.wine.wineserver_bin)
                .arg("-w")
                .env("WINEPREFIX", &wineprefix_str)
                .status();
            if let Err(e) = wineserver_status {
                warn!("Warning: Failed to wait for wineserver: {}", e);
            }
        }

        // Execute verb installation
        self.execute_verb_installation(&metadata, &cache_dir)
            .await?;
        
        // Handle .NET post-installation steps
        if verb_name == "dotnet48" {
            // Override mscoree.dll to native (required for .NET 4.8) - AFTER installation
            self.set_dll_override("mscoree", "native")?;
            
            // Create marker file (as original winetricks does)
            let wineprefix = self.config.wineprefix();
            let marker_file = wineprefix.join("drive_c/windows/dotnet48.installed.workaround");
            if let Err(e) = std::fs::File::create(&marker_file) {
                warn!("Warning: Failed to create marker file: {}", e);
            }
        } else if verb_name == "dotnet35" || verb_name == "dotnet35sp1" {
            // For dotnet35, DLL overrides are done BEFORE installation (already done above)
            // Restore Windows version (original winetricks does w_restore_winver)
            self.restore_windows_version()?;
        }

        // Verify installation
        if let Some(ref installed_file) = metadata.installed_file {
            info!("Verifying installation: {}", installed_file);
            
            // For .NET Framework, do comprehensive verification
            if verb_name.starts_with("dotnet") {
                if !self.verify_dotnet_installation(verb_name, installed_file)? {
                    return Err(WinetricksError::Verb(format!(
                        "Installation verification failed for {}. The installer may have failed silently.",
                        verb_name
                    )));
                }
            } else {
                // For other verbs, check the installed_file path
                if !self.verify_file_exists(installed_file)? {
                    warn!("Warning: Installed file not found: {}. Installation may have failed.", installed_file);
                    // Don't fail for non-critical verbs, but warn
                }
            }
        }

        // For .NET Framework installers, wait a bit longer after verification
        if verb_name.starts_with("dotnet") {
            info!("Waiting for .NET Framework installation to fully complete...");
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

                    // For .NET installers, change to cache directory (like original winetricks does)
                    // This ensures the installer can extract files properly
                    let current_dir = if is_dotnet {
                        Some(std::env::current_dir()?)
                    } else {
                        None
                    };
                    
                    if is_dotnet {
                        // Change to cache directory (where installer is located)
                        // This is critical for .NET installers to extract properly
                        std::env::set_current_dir(file.parent().ok_or_else(|| {
                            WinetricksError::Config("Could not get parent directory of installer".into())
                        })?)?;
                        info!("Changed to cache directory: {:?}", std::env::current_dir()?);
                    }

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
                    // Note: Original winetricks sets this as environment variable, not via cmd.env
                    // But we need it in the command environment, so cmd.env is correct
                    if is_dotnet {
                        cmd.env("WINEDLLOVERRIDES", "fusion=b");
                    }
                    
                    // IMPORTANT: Original winetricks does NOT use "wine start /wait"
                    // It just calls "wine <installer>" directly via w_try_ms_installer -> w_try
                    // So we should NOT use "start /wait" - just call wine directly

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

                    // For .NET installers, we need to use just the filename since we changed directory
                    if is_dotnet {
                        let file_name = file.file_name()
                            .and_then(|n| n.to_str())
                            .ok_or_else(|| WinetricksError::Config("Invalid filename".into()))?;
                        cmd.arg(file_name);
                    } else {
                        cmd.arg(&file_win_path);
                    }

                    // Detect specific .NET versions for proper handling
                    let is_dotnet35 = filename.contains("35") || filename.contains("dotnet35") || filename.contains("NetFx35");
                    let is_dotnet40 = filename.contains("40") || filename.contains("dotnet40") || filename.contains("NetFx40");
                    let is_dotnet45 = filename.contains("45") || filename.contains("dotnet45") || filename.contains("NetFx45");
                    let is_dotnet46 = filename.contains("46") || filename.contains("462") || filename.contains("dotnet46");
                    let is_dotnet472 = filename.contains("472") || filename.contains("dotnet472");
                    let is_dotnet48 = filename.contains("48") || filename.contains("dotnet48");

                    // Apply appropriate silent flags based on installer type
                    // For .NET installers, always use silent flags (they work better)
                    if is_dotnet {
                        // .NET Framework installers require version-specific handling
                        if is_dotnet48 || is_dotnet472 {
                            // .NET 4.8 and 4.7.2: /sfxlang:1027 /q /norestart
                            // Only use these flags if unattended (matching original winetricks)
                            if self.config.unattended {
                                cmd.arg("/sfxlang:1027").arg("/q").arg("/norestart");
                            }
                        } else if is_dotnet46 {
                            // .NET 4.6+: /q /norestart
                            if self.config.unattended {
                                cmd.arg("/q").arg("/norestart");
                            }
                        } else if is_dotnet45 || is_dotnet40 {
                            // .NET 4.5 and 4.0: Use /quiet flag
                            if self.config.unattended {
                                cmd.arg("/quiet").arg("/norestart");
                            }
                        } else if is_dotnet35 {
                            // .NET 3.5: /lang:ENU /q (only /q if unattended)
                            // Original winetricks: w_try_ms_installer "${WINE}" "${file1}" /lang:ENU ${W_OPT_UNATTENDED:+/q}
                            cmd.arg("/lang:ENU");
                            if self.config.unattended {
                                cmd.arg("/q");
                            }
                        } else {
                            // Older/newer .NET versions: Try common flags
                            if self.config.unattended {
                                cmd.arg("/q").arg("/norestart");
                            }
                        }
                    } else if self.config.unattended {
                        if is_vcredist {
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
                            command: format!("wine {} {:?}", if is_dotnet { "start /wait" } else { "" }, file_win_path),
                            error: e.to_string(),
                        })?;

                    // Restore original directory if we changed it
                    if let Some(orig_dir) = current_dir {
                        if let Err(e) = std::env::set_current_dir(orig_dir) {
                            warn!("Warning: Failed to restore directory: {}", e);
                        }
                    }

                    // Wait for wineserver after .NET installation (important for proper completion)
                    if is_dotnet {
                        info!("Waiting for wineserver to finish processing .NET installation...");
                        std::thread::sleep(std::time::Duration::from_secs(1)); // Brief pause first
                        let wineserver_status = std::process::Command::new(&self.wine.wineserver_bin)
                            .arg("-w")
                            .env("WINEPREFIX", &wineprefix_str)
                            .status();
                        if let Err(e) = wineserver_status {
                            warn!("Warning: Failed to wait for wineserver: {}", e);
                        }
                    }

                    // Check exit code - .NET installers can return specific codes that indicate success
                    let exit_code = status.code();
                    
                    // .NET installers can return:
                    // 0 = success
                    // 3010 = success (reboot required)
                    // 236 = success (cancelled by user, but installer extracted files)
                    // 1603 = fatal error (but sometimes false positive for .NET 3.5/4.5)
                    // Other non-zero = usually failure
                    let is_success = if is_dotnet {
                        match exit_code {
                            Some(0) | Some(3010) | Some(236) => true,
                            Some(1603) => {
                                // .NET 3.5/4.5 can return 1603 even when partially successful
                                let is_dotnet35_or_45 = filename.contains("35") || filename.contains("45");
                                if is_dotnet35_or_45 {
                                    warn!("Warning: Installer returned exit code 1603 (fatal error). This may be a false positive for .NET 3.5/4.5.");
                                    warn!("Checking if installation actually succeeded...");
                                    true // We'll verify later
                                } else {
                                    false
                                }
                            },
                            _ => false,
                        }
                    } else {
                        status.success()
                    };
                    
                    if !is_success {
                        // For .NET, don't fail immediately - we'll verify installation files
                        if is_dotnet {
                            warn!("Installer returned non-success exit code: {:?}", exit_code);
                            warn!("Continuing to verify installation - some .NET installers report failure but still install files.");
                        } else {
                            return Err(WinetricksError::Verb(format!(
                                "Installer failed with exit code: {:?}",
                                exit_code
                            )));
                        }
                    }
                }
                "zip" => {
                    info!("Extracting ZIP: {:?}", file);
                    self.extract_zip(file, &cache_dir)?;
                }
                "cab" => {
                    info!("Extracting CAB: {:?}", file);
                    self.extract_cab(file, &cache_dir)?;
                }
                "7z" => {
                    info!("Extracting 7z: {:?}", file);
                    self.extract_7z(file, &cache_dir)?;
                }
                "rar" => {
                    info!("Extracting RAR: {:?}", file);
                    self.extract_rar(file, &cache_dir)?;
                }
                "reg" => {
                    info!("Importing registry file: {:?}", file);
                    self.import_registry_file(file)?;
                }
                _ => {
                    // Check if file might be an archive by magic bytes or try extraction
                    // Some files might not have extensions but are archives
                    let filename = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if filename.ends_with(".7z") || filename.contains(".7z.") {
                        self.extract_7z(file, &cache_dir)?;
                    } else if filename.ends_with(".rar") || filename.contains(".rar.") {
                        self.extract_rar(file, &cache_dir)?;
                    } else {
                        warn!("Unknown file type: {:?}", file);
                    }
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

    /// Verify that a file exists in the wineprefix (Windows path converted to Unix)
    fn verify_file_exists(&self, windows_path: &str) -> Result<bool> {
        let wineprefix = self.config.wineprefix();
        
        // Convert Windows path template (e.g., ${W_WINDIR_WIN}/file.dll) to actual path
        // For now, handle common templates
        let mut unix_path = windows_path.to_string();
        
        // Replace common Wine path variables
        if unix_path.contains("${W_WINDIR_WIN}") || unix_path.contains("$W_WINDIR_WIN") {
            unix_path = unix_path.replace("${W_WINDIR_WIN}", "");
            unix_path = unix_path.replace("$W_WINDIR_WIN", "");
            // Remove leading slash if present
            let windows_part = unix_path.trim_start_matches('/').replace('\\', "/");
            let full_path = wineprefix.join("drive_c/windows").join(&windows_part);
            return Ok(full_path.exists());
        }
        
        if unix_path.contains("${W_SYSTEM32") || unix_path.contains("$W_SYSTEM32") {
            unix_path = unix_path.replace("${W_SYSTEM32_DLLS_WIN}", "");
            unix_path = unix_path.replace("$W_SYSTEM32_DLLS_WIN", "");
            unix_path = unix_path.replace("${W_SYSTEM32_WIN}", "");
            unix_path = unix_path.replace("$W_SYSTEM32_WIN", "");
            let windows_part = unix_path.trim_start_matches('/').replace('\\', "/");
            let full_path = wineprefix.join("drive_c/windows/system32").join(&windows_part);
            return Ok(full_path.exists());
        }
        
        // Try using winepath to convert if it's a simple Windows path
        if unix_path.starts_with("C:\\") || unix_path.starts_with("c:\\") {
            let wine_path = self.windows_to_unix_path(windows_path)?;
            return Ok(wine_path.exists());
        }
        
        // Fallback: try direct conversion
        let windows_part = windows_path.replace('\\', "/").trim_start_matches('/').to_string();
        let full_path = wineprefix.join("drive_c").join(&windows_part);
        Ok(full_path.exists())
    }
    
    /// Convert Windows path to Unix path using winepath
    fn windows_to_unix_path(&self, windows_path: &str) -> Result<PathBuf> {
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Use winepath to convert Windows path to Unix
        let output = std::process::Command::new(&self.wine.wine_bin)
            .arg("winepath")
            .arg("-u")
            .arg(windows_path)
            .env("WINEPREFIX", &wineprefix_str)
            .output()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine winepath -u {:?}", windows_path),
                error: e.to_string(),
            })?;
        
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Ok(PathBuf::from(path))
        } else {
            // Fallback: manual conversion
            let windows_part = windows_path.replace('\\', "/").trim_start_matches(|c| c == 'C' || c == ':' || c == '/').to_string();
            Ok(wineprefix.join("drive_c").join(&windows_part))
        }
    }
    
    /// Verify .NET Framework installation by checking registry and files
    fn verify_dotnet_installation(&self, verb_name: &str, _installed_file: &str) -> Result<bool> {
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        info!("Verifying .NET Framework installation for {}...", verb_name);
        
        // Check registry for .NET version
        let registry_key = if verb_name == "dotnet48" {
            "HKLM\\Software\\Microsoft\\NET Framework Setup\\NDP\\v4\\Full"
        } else if verb_name == "dotnet35" || verb_name == "dotnet35sp1" {
            "HKLM\\Software\\Microsoft\\NET Framework Setup\\NDP\\v3.5"
        } else if verb_name.starts_with("dotnet4") {
            "HKLM\\Software\\Microsoft\\NET Framework Setup\\NDP\\v4\\Full"
        } else if verb_name.starts_with("dotnet3") {
            "HKLM\\Software\\Microsoft\\NET Framework Setup\\NDP\\v3.5"
        } else {
            // Fallback: check v4
            "HKLM\\Software\\Microsoft\\NET Framework Setup\\NDP\\v4\\Full"
        };
        
        // Check registry
        let registry_check = std::process::Command::new(&self.wine.wine_bin)
            .arg("reg")
            .arg("query")
            .arg(registry_key)
            .env("WINEPREFIX", &wineprefix_str)
            .output();
        
        let registry_found = registry_check
            .map(|output| output.status.success())
            .unwrap_or(false);
        
        if !registry_found {
            warn!("Registry key {} not found", registry_key);
        }
        
        // Check for actual .NET files in Framework directories
        let framework_dirs = vec![
            wineprefix.join("drive_c/windows/Microsoft.NET/Framework/v4.0.30319"),
            wineprefix.join("drive_c/windows/Microsoft.NET/Framework/v3.5"),
            wineprefix.join("drive_c/windows/Microsoft.NET/Framework64/v4.0.30319"),
        ];
        
        let mut files_found = false;
        for framework_dir in &framework_dirs {
            if framework_dir.exists() {
                // Check for key .NET DLLs
                let key_dlls = vec![
                    "mscoree.dll",
                    "mscorlib.dll",
                    "System.dll",
                    "Microsoft.NETFramework.dll",
                ];
                
                for dll in &key_dlls {
                    let dll_path = framework_dir.join(dll);
                    if dll_path.exists() {
                        files_found = true;
                        info!("Found .NET file: {:?}", dll_path);
                        break;
                    }
                }
                
                if files_found {
                    break;
                }
            }
        }
        
        // For .NET, require both registry AND files
        if registry_found && files_found {
            info!("✅ .NET Framework {} verified: registry and files found", verb_name);
            Ok(true)
        } else if registry_found && !files_found {
            warn!("⚠️  .NET Framework {} registry found but files missing - installation incomplete!", verb_name);
            Ok(false)
        } else if !registry_found && files_found {
            warn!("⚠️  .NET Framework {} files found but registry missing - may not be properly registered", verb_name);
            Ok(false)
        } else {
            warn!("❌ .NET Framework {} not properly installed: no registry or files found", verb_name);
            Ok(false)
        }
    }

    /// Set Windows version in Wine registry
    fn set_windows_version(&self, version: &str) -> Result<()> {
        use std::fs;
        use std::io::Write;
        use std::process::Command;

        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Map version names to Windows version numbers
        let version_num = match version.to_lowercase().as_str() {
            "win10" | "windows10" => "0xa00",
            "win81" | "windows81" => "0x0603",
            "win8" | "windows8" => "0x0602",
            "win7" | "windows7" => "0x0601",
            "winxp" | "windowsxp" => "0x0501",
            "win2k" | "windows2000" => "0x0500",
            _ => {
                warn!("Unknown Windows version: {}, defaulting to Windows 7", version);
                "0x0601"
            }
        };

        // Create temp directory for registry file
        let temp_dir = dirs::cache_dir()
            .ok_or_else(|| WinetricksError::Config("Could not determine cache directory".into()))?
            .join("winetricks");
        fs::create_dir_all(&temp_dir)?;

        let reg_file = temp_dir.join("set_wine_version.reg");

        // Create registry file
        let reg_content = format!(
            r#"REGEDIT4

[HKEY_CURRENT_USER\Software\Wine]
"Version"="{}"
"#,
            version_num
        );

        let mut file = fs::File::create(&reg_file)?;
        file.write_all(reg_content.as_bytes())?;
        file.sync_all()?;

        // Convert Unix path to Wine Windows path
        let reg_file_str = reg_file.to_string_lossy().to_string();
        
        // Get Windows path for the reg file
        let output = Command::new(&self.wine.wine_bin)
            .arg("winepath")
            .arg("-w")
            .arg(&reg_file_str)
            .env("WINEPREFIX", &wineprefix_str)
            .output()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine winepath -w {:?}", reg_file_str),
                error: e.to_string(),
            })?;

        let reg_file_win = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // Import registry file using wine regedit
        let status = Command::new(&self.wine.wine_bin)
            .arg("regedit")
            .arg("/S") // Silent mode
            .arg(&reg_file_win)
            .env("WINEPREFIX", &wineprefix_str)
            .status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine regedit /S {:?}", reg_file_win),
                error: e.to_string(),
            })?;

        // Clean up temp file
        let _ = fs::remove_file(&reg_file);

        if !status.success() {
            return Err(WinetricksError::Config(format!(
                "Failed to set Windows version to {}",
                version
            )));
        }

        info!("Set Windows version to {}", version);
        Ok(())
    }

    /// Store current Windows version (for restore later)
    fn store_windows_version(&mut self) -> Result<()> {
        use std::process::Command;
        
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();

        // Query current Windows version from registry
        let output = Command::new(&self.wine.wine_bin)
            .arg("reg")
            .arg("query")
            .arg("HKEY_CURRENT_USER\\Software\\Wine")
            .arg("/v")
            .arg("Version")
            .env("WINEPREFIX", &wineprefix_str)
            .output();

        if let Ok(output) = output {
            if output.status.success() {
                // Parse version number from output (e.g., "0x0601")
                let output_str = String::from_utf8_lossy(&output.stdout);
                for line in output_str.lines() {
                    if line.contains("REG_SZ") || line.contains("REG_DWORD") {
                        // Extract version value (hex number)
                        if let Some(version_val) = line.split_whitespace().last() {
                            // Map version number back to name
                            let version_name = match version_val {
                                "0xa00" => "win10",
                                "0x0603" => "win81",
                                "0x0602" => "win8",
                                "0x0601" => "win7",
                                "0x0501" => "winxp",
                                "0x0500" => "win2k",
                                _ => {
                                    warn!("Unknown Windows version number: {}, storing as win7", version_val);
                                    "win7"
                                }
                            };
                            self.stored_windows_version = Some(version_name.to_string());
                            info!("Stored Windows version: {}", version_name);
                            return Ok(());
                        }
                    }
                }
            }
        }

        // If we can't read current version, assume win7 (most common default)
        warn!("Could not read current Windows version, assuming win7");
        self.stored_windows_version = Some("win7".to_string());
        Ok(())
    }

    /// Restore previously stored Windows version
    fn restore_windows_version(&mut self) -> Result<()> {
        if let Some(ref version) = self.stored_windows_version {
            info!("Restoring Windows version to: {}", version);
            self.set_windows_version(version)?;
            self.stored_windows_version = None;
        } else {
            warn!("No stored Windows version to restore");
        }
        Ok(())
    }

    /// Set DLL override in Wine registry
    fn set_dll_override(&self, dll_name: &str, override_type: &str) -> Result<()> {
        use std::fs;
        use std::io::Write;
        use std::process::Command;

        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();

        // Create temp directory for registry file
        let temp_dir = dirs::cache_dir()
            .ok_or_else(|| WinetricksError::Config("Could not determine cache directory".into()))?
            .join("winetricks");
        fs::create_dir_all(&temp_dir)?;

        let reg_file = temp_dir.join("set_dll_override.reg");

        // Create registry file
        // DLL overrides go in HKEY_CURRENT_USER\Software\Wine\DllOverrides
        let reg_content = format!(
            r#"REGEDIT4

[HKEY_CURRENT_USER\Software\Wine\DllOverrides]
"{}"="{}"
"#,
            dll_name, override_type
        );

        let mut file = fs::File::create(&reg_file)?;
        file.write_all(reg_content.as_bytes())?;
        file.sync_all()?;

        // Convert Unix path to Wine Windows path
        let reg_file_str = reg_file.to_string_lossy().to_string();
        
        // Get Windows path for the reg file
        let output = Command::new(&self.wine.wine_bin)
            .arg("winepath")
            .arg("-w")
            .arg(&reg_file_str)
            .env("WINEPREFIX", &wineprefix_str)
            .output()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine winepath -w {:?}", reg_file_str),
                error: e.to_string(),
            })?;

        let reg_file_win = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // Import registry file using wine regedit
        let status = Command::new(&self.wine.wine_bin)
            .arg("regedit")
            .arg("/S") // Silent mode
            .arg(&reg_file_win)
            .env("WINEPREFIX", &wineprefix_str)
            .status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine regedit /S {:?}", reg_file_win),
                error: e.to_string(),
            })?;

        // Clean up temp file
        let _ = fs::remove_file(&reg_file);

        if !status.success() {
            return Err(WinetricksError::Config(format!(
                "Failed to set DLL override for {} to {}",
                dll_name, override_type
            )));
        }

        info!("Set DLL override: {} = {}", dll_name, override_type);
        Ok(())
    }

    /// Extract ZIP archive (matching w_try_unzip behavior)
    fn extract_zip(&self, zip_file: &Path, dest_dir: &Path) -> Result<()> {
        use std::process::Command;
        use which::which;

        // Try unzip first (matches original winetricks)
        if which("unzip").is_ok() {
            info!("Using unzip to extract: {:?}", zip_file);
            let status = Command::new("unzip")
                .arg("-o") // Overwrite files without prompting
                .arg("-q") // Quiet mode
                .arg("-d") // Destination directory
                .arg(dest_dir)
                .arg(zip_file)
                .status()
                .map_err(|e| WinetricksError::CommandExecution {
                    command: format!("unzip -o -q -d {:?} {:?}", dest_dir, zip_file),
                    error: e.to_string(),
                })?;

            if status.success() {
                return Ok(());
            }
            warn!("unzip failed, falling back to 7z");
        }

        // Fallback to 7z (or Windows 7-Zip via Wine)
        self.extract_7z(zip_file, dest_dir)
    }

    /// Extract CAB archive using cabextract (matching w_try_cabextract behavior)
    fn extract_cab(&self, cab_file: &Path, dest_dir: &Path) -> Result<()> {
        use std::process::Command;
        use which::which;

        // cabextract is required (original winetricks dies if not found)
        let cabextract = which("cabextract")
            .map_err(|_| WinetricksError::Config(
                "cabextract not found. Please install it (e.g. 'sudo apt install cabextract' or 'sudo yum install cabextract')".into()
            ))?;

        info!("Using cabextract to extract: {:?}", cab_file);
        
        // cabextract extracts to current directory, so we need to change directory
        let original_dir = std::env::current_dir()?;
        std::env::set_current_dir(dest_dir)
            .map_err(|e| WinetricksError::Config(format!("Failed to change to dest directory: {}", e)))?;

        let status = Command::new(&cabextract)
            .arg("-q") // Quiet mode
            .arg(cab_file)
            .status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("cabextract -q {:?}", cab_file),
                error: e.to_string(),
            })?;

        // Restore directory
        std::env::set_current_dir(original_dir)?;

        if !status.success() {
            return Err(WinetricksError::Verb(format!(
                "cabextract failed with exit code: {:?}",
                status.code()
            )));
        }

        Ok(())
    }

    /// Extract 7z archive (matching w_try_7z behavior)
    fn extract_7z(&self, archive: &Path, dest_dir: &Path) -> Result<()> {
        use std::process::Command;
        use which::which;

        // Try 7z first (matches original winetricks)
        if which("7z").is_ok() {
            info!("Using 7z to extract: {:?}", archive);
            let status = Command::new("7z")
                .arg("x") // Extract with full paths
                .arg(archive)
                .arg("-o") // Output directory (no space after -o)
                .arg(dest_dir)
                .arg("-y") // Assume yes on all queries
                .status()
                .map_err(|e| WinetricksError::CommandExecution {
                    command: format!("7z x {:?} -o{:?}", archive, dest_dir),
                    error: e.to_string(),
                })?;

            if status.success() {
                return Ok(());
            }
            warn!("7z failed, falling back to Windows 7-Zip via Wine");
        }

        // Fallback to Windows 7-Zip via Wine (original winetricks does this)
        // First check if 7zip is installed in the wineprefix
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        let sevenzip_exe = wineprefix.join("drive_c/Program Files/7-Zip/7z.exe");

        if sevenzip_exe.exists() {
            info!("Using Windows 7-Zip via Wine to extract: {:?}", archive);
            let archive_win_path = self.unix_to_wine_path(archive)?;
            let dest_win_path = self.unix_to_wine_path(dest_dir)?;

            let status = std::process::Command::new(&self.wine.wine_bin)
                .arg(&sevenzip_exe)
                .arg("x")
                .arg(&archive_win_path)
                .arg("-o")
                .arg(&dest_win_path)
                .arg("-y")
                .env("WINEPREFIX", &wineprefix_str)
                .status()
                .map_err(|e| WinetricksError::CommandExecution {
                    command: format!("wine 7z.exe x {:?} -o{:?}", archive_win_path, dest_win_path),
                    error: e.to_string(),
                })?;

            if status.success() {
                return Ok(());
            }
        }

        // If we get here, we need to install 7zip first
        warn!("7z not available and Windows 7-Zip not found. Attempting to install 7zip...");
        // TODO: Implement automatic 7zip installation
        Err(WinetricksError::Config(
            "Cannot extract archive: 7z not found and Windows 7-Zip not available. Please install 7z (e.g. 'sudo apt install 7zip')".into()
        ))
    }

    /// Extract RAR archive (matching w_try_unrar behavior)
    fn extract_rar(&self, rar_file: &Path, dest_dir: &Path) -> Result<()> {
        use std::process::Command;
        use which::which;

        // Try unrar first (matches original winetricks)
        if which("unrar").is_ok() {
            info!("Using unrar to extract: {:?}", rar_file);
            // Change to dest directory (unrar extracts to current directory)
            let original_dir = std::env::current_dir()?;
            std::env::set_current_dir(dest_dir)
                .map_err(|e| WinetricksError::Config(format!("Failed to change to dest directory: {}", e)))?;

            let status = Command::new("unrar")
                .arg("x") // Extract with full paths
                .arg(rar_file)
                .status()
                .map_err(|e| WinetricksError::CommandExecution {
                    command: format!("unrar x {:?}", rar_file),
                    error: e.to_string(),
                })?;

            // Restore directory
            std::env::set_current_dir(original_dir)?;

            if status.success() {
                return Ok(());
            }
            warn!("unrar failed, falling back to 7z");
        }

        // Fallback to 7z (or Windows 7-Zip)
        self.extract_7z(rar_file, dest_dir)
    }

    /// Import registry file using regedit (matching w_try_regedit behavior)
    fn import_registry_file(&self, reg_file: &Path) -> Result<()> {
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Convert Unix path to Wine Windows path
        let reg_file_win_path = self.unix_to_wine_path(reg_file)?;

        // Use regedit to import (silent mode if unattended)
        let mut cmd = std::process::Command::new(&self.wine.wine_bin);
        cmd.env("WINEPREFIX", &wineprefix_str);
        
        // Add /S flag for silent mode if unattended (matches original winetricks)
        if self.config.unattended {
            cmd.arg("regedit").arg("/S");
        } else {
            cmd.arg("regedit");
        }
        cmd.arg(&reg_file_win_path);

        let status = cmd.status().map_err(|e| WinetricksError::CommandExecution {
            command: format!("wine regedit {:?}", reg_file_win_path),
            error: e.to_string(),
        })?;

        if !status.success() {
            return Err(WinetricksError::Verb(format!(
                "Registry import failed with exit code: {:?}",
                status.code()
            )));
        }

        Ok(())
    }

    /// Register DLL using regsvr32 (matching w_try_regsvr32 behavior)
    pub fn register_dll(&self, dll_name: &str, dll_path: &Path) -> Result<()> {
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Convert DLL path to Wine Windows path
        let dll_win_path = self.unix_to_wine_path(dll_path)?;

        // Use regsvr32 to register DLL
        let status = std::process::Command::new(&self.wine.wine_bin)
            .arg("regsvr32")
            .arg("/s") // Silent mode
            .arg(&dll_win_path)
            .env("WINEPREFIX", &wineprefix_str)
            .status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine regsvr32 /s {:?}", dll_win_path),
                error: e.to_string(),
            })?;

        if !status.success() {
            warn!("regsvr32 returned non-zero exit code for {}: {:?}", dll_name, status.code());
            // Don't fail - DLL registration can sometimes fail but DLL might still work
        }

        info!("Registered DLL: {}", dll_name);
        Ok(())
    }

    /// Register DLL using regsvr64 for 64-bit (matching w_try_regsvr64 behavior)
    pub fn register_dll_64(&self, dll_name: &str, dll_path: &Path) -> Result<()> {
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Find wine64 binary
        let wine64_bin = if let Some(wine_dir) = self.wine.wine_bin.parent() {
            wine_dir.join("wine64")
        } else {
            which::which("wine64")
                .map_err(|_| WinetricksError::Config("wine64 not found".into()))?
        };

        // Convert DLL path to Wine Windows path
        let dll_win_path = self.unix_to_wine_path(dll_path)?;

        // Use regsvr32 via wine64 to register 64-bit DLL
        let status = std::process::Command::new(&wine64_bin)
            .arg("regsvr32")
            .arg("/s") // Silent mode
            .arg(&dll_win_path)
            .env("WINEPREFIX", &wineprefix_str)
            .status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine64 regsvr32 /s {:?}", dll_win_path),
                error: e.to_string(),
            })?;

        if !status.success() {
            warn!("regsvr64 returned non-zero exit code for {}: {:?}", dll_name, status.code());
        }

        info!("Registered 64-bit DLL: {}", dll_name);
        Ok(())
    }
}
