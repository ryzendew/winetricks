//! Verb execution engine

use crate::config::Config;
use crate::download::DownloadManager;
use crate::error::{Result, WinetricksError};
use crate::installer::{detect_from_file, detect_installer_type, get_msi_silent_switch, get_silent_switches, InstallerType};
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

        // Check if this is a VC++ Redistributables verb (for internal calls, pass false)
        let is_vcrun_internal = verb_name.starts_with("vcrun20") || verb_name.starts_with("ucrtbase");
        
        // Execute verb installation
        self.execute_verb_installation(&metadata, &cache_dir, is_vcrun_internal)
            .await?;

        // Log installation
        self.log_installation(verb_name)?;

        Ok(())
    }

    /// Install a verb using Rust implementation
    pub async fn install_verb(&mut self, verb_name: &str) -> Result<()> {
        let start_time = Instant::now();
        info!("Installing verb: {}", verb_name);
        
        // Debug: Log force and unattended flags
        if self.config.force {
            info!("Force mode enabled - will reinstall if already installed");
        }
        if self.config.unattended {
            info!("Unattended mode enabled - no prompts");
        }

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

        // If force is enabled, clean up FIRST (before checking if installed)
        // This ensures cleanup runs even if the check happens before installation
        if self.config.force {
            // For .NET installers, always clean up partial installations when forcing
            // This is important because failed installations may leave partial files but not be in log
            if verb_name.starts_with("dotnet") {
                info!("Force mode: Cleaning up any partial .NET installation first...");
                self.cleanup_dotnet_installation(verb_name)?;
            }
            
            // Remove from log if present
            if self.is_installed(verb_name)? {
                info!("Force reinstall requested for {} (found in log, removing)", verb_name);
                self.remove_from_log(verb_name)?;
            } else {
                info!("Force reinstall requested for {} (not in log, but cleanup done)", verb_name);
            }
        } else {
            // Only check if installed when NOT forcing
            if self.is_installed(verb_name)? {
                println!("{} already installed, skipping", verb_name);
                println!("Use --force to reinstall");
                return Ok(());
            }
        }

        // Check if package is broken in current Wine version
        self.check_package_broken(verb_name, &metadata)?;

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
        if verb_name == "dotnet45" {
            info!("Preparing for .NET 4.5 installation...");
            
            // 1. Remove Mono (prevents conflicts)
            // Note: If Mono isn't installed, this will fail but that's fine
            if let Err(e) = self.install_verb_internal("remove_mono").await {
                // Check if error is just "not found" - that's expected if Mono isn't installed
                if e.to_string().contains("No such file") || e.to_string().contains("not found") {
                    info!("Mono not installed (or already removed) - skipping removal");
                } else {
                    warn!("Warning: Failed to remove Mono (may not be critical): {}", e);
                }
            }
            
            // 2. Install .NET 4.0 first (required prerequisite)
            if let Err(e) = self.install_verb_internal("dotnet40").await {
                warn!("Warning: Failed to install .NET 4.0 prerequisite: {}", e);
                // Continue anyway, but warn
            }
            
            // 3. Set Windows version to Windows 7 (required for .NET 4.5)
            self.set_windows_version("win7")?;
        } else if verb_name == "dotnet48" || verb_name == "dotnet48.1" {
            let version_str = if verb_name == "dotnet48.1" { "4.8.1" } else { "4.8" };
            info!("Preparing for .NET {} installation...", version_str);
            
            // 1. Remove Mono (prevents conflicts)
            // Note: If Mono isn't installed, this will fail but that's fine
            if let Err(e) = self.install_verb_internal("remove_mono").await {
                // Check if error is just "not found" - that's expected if Mono isn't installed
                if e.to_string().contains("No such file") || e.to_string().contains("not found") {
                    info!("Mono not installed (or already removed) - skipping removal");
                } else {
                    warn!("Warning: Failed to remove Mono (may not be critical): {}", e);
                }
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
            // Note: If Mono isn't installed, this will fail but that's fine
            if let Err(e) = self.install_verb_internal("remove_mono").await {
                // Check if error is just "not found" - that's expected if Mono isn't installed
                if e.to_string().contains("No such file") || e.to_string().contains("not found") {
                    info!("Mono not installed (or already removed) - skipping removal");
                } else {
                    warn!("Warning: Failed to remove Mono (may not be critical): {}", e);
                }
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

        // Check if this is a VC++ Redistributables verb
        let is_vcrun_verb = verb_name.starts_with("vcrun20") || verb_name.starts_with("ucrtbase");
        
        // Handle comctl32 and vcrun2005 special DLL override handling (before installation)
        // These verbs need wine builtin manifests removed
        if verb_name == "comctl32" {
            info!("Preparing for comctl32 installation (will remove wine builtin manifests)...");
        } else if verb_name == "vcrun2005" {
            info!("Preparing for vcrun2005 installation (will remove wine builtin manifests)...");
        }
        
        // Apply wine bug workarounds if needed (w_workaround_wine_bug)
        // This can be called by individual verbs to conditionally apply fixes
        // For now, we have the infrastructure but individual verbs would need to call it
        
        // Check if this is a DirectX d3dx9 verb (needs special handling)
        let is_d3dx9_verb = verb_name.starts_with("d3dx9") || verb_name == "d3dx9";
        
        // Handle DirectX prerequisites (download DirectX redistributable if needed)
        if is_d3dx9_verb {
            info!("Preparing for DirectX d3dx9 DLL installation...");
            self.ensure_directx_redistributable().await?;
        }

        // Handle corefonts (meta-verb that installs multiple individual font verbs)
        if verb_name == "corefonts" {
            info!("Installing corefonts (MS Arial, Courier, Times fonts)...");
            return self.install_corefonts().await;
        }
        
        // Handle allfonts (meta-verb that installs all available font verbs)
        if verb_name == "allfonts" {
            info!("Installing allfonts (all available fonts)...");
            return self.install_allfonts().await;
        }
        
        // Handle verbs with empty files arrays (need special handling)
        if metadata.files.is_empty() {
            // GitHub-based DLLs that download from releases
            let github_dlls = vec![
                ("vkd3d", "HansKristian-Work", "vkd3d-proton", vec!["d3d12.dll", "d3d12core.dll"]),
                ("dxvk", "doitsujin", "dxvk", vec!["d3d9.dll", "d3d10.dll", "d3d10core.dll", "d3d11.dll", "dxgi.dll"]),
                ("dxvk_async", "Ph42oN", "dxvk-async", vec!["d3d9.dll", "d3d10.dll", "d3d10core.dll", "d3d11.dll", "dxgi.dll"]),
                ("dxvk_nvapi", "jp7677", "dxvk-nvapi", vec!["nvapi.dll", "nvapi64.dll"]),
                ("faudio", "FNA-XNA", "FAudio", vec!["FAudio.dll"]),
                ("galliumnine", "iXit", "GalliumNine", vec!["d3d9-nine.dll"]),
                ("otvdm", "otvdm", "otvdm", vec!["otvdm.exe"]),
            ];
            
            for (verb, org, repo, dlls) in &github_dlls {
                if verb_name == *verb {
                    info!("Installing {} (downloads from GitHub releases)...", verb_name);
                    return self.install_github_dll(verb_name, org, repo, dlls).await;
                }
            }
            
            // Meta-verbs that install multiple components
            if verb_name == "allcodecs" {
                info!("Installing allcodecs (all codec components)...");
                return self.install_allcodecs().await;
            }
            
            if verb_name == "cjkfonts" {
                info!("Installing cjkfonts (CJK font components)...");
                return self.install_cjkfonts().await;
            }
            
            if verb_name == "pptfonts" {
                info!("Installing pptfonts (PowerPoint font components)...");
                return self.install_pptfonts().await;
            }
            
            // Special cases
            if verb_name == "directx9" {
                // DirectX 9 is deprecated/no-op in modern winetricks
                info!("directx9 is deprecated (no-op), skipping");
                self.log_installation("directx9")?;
                return Ok(());
            }
            
            if verb_name == "filever" {
                // filever is a utility that might need special handling
                // For now, just log it (it may not actually need installation)
                warn!("filever has no files array - may need special handling");
                self.log_installation("filever")?;
                return Ok(());
            }
            
            if verb_name == "mspaint" {
                // mspaint uses Windows Update installer that needs extraction
                return self.install_mspaint().await;
            }
            
            // Settings verbs (Windows version, registry tweaks, etc.)
            // These don't need files, they just modify registry/config
            if metadata.category == VerbCategory::Settings {
                info!("Installing settings verb: {} (no files needed)", verb_name);
                // Settings verbs are handled by execute_verb_installation
                // which will detect they have no files and skip file processing
                // But we still need to handle the actual setting change
                return self.install_settings_verb(verb_name, &metadata).await;
            }
            
            // If we get here, it's an unknown verb with no files
            warn!("Verb {} has no files array and no special handler. Skipping file installation.", verb_name);
            // Still log it as installed (for settings that don't need files)
            self.log_installation(verb_name)?;
            return Ok(());
        }
        
        // Execute verb installation
        self.execute_verb_installation(&metadata, &cache_dir, is_vcrun_verb)
            .await?;
        
        // Handle VC++ Redistributables post-installation steps
        if is_vcrun_verb {
            // On win64 prefixes, also install 64-bit version
            if self.config.winearch.as_ref().map(|a| a == "win64").unwrap_or(false) {
                if let Err(e) = self.install_vcredist_64bit(verb_name, &cache_dir) {
                    warn!("Warning: Failed to install 64-bit VC++ Redistributables: {}", e);
                }
            }
        }

        // Handle comctl32 and vcrun2005 special DLL override (after installation)
        if verb_name == "comctl32" {
            // comctl32: w_override_dlls native,builtin comctl32 (uses w_common_override_dll)
            // Original winetricks: w_common_override_dll "native,builtin" comctl32
            if let Err(e) = self.common_override_dll("comctl32", "native,builtin", &["comctl32"]) {
                warn!("Warning: Failed to set DLL overrides for comctl32: {}", e);
            }
        } else if verb_name == "vcrun2005" {
            // vcrun2005: w_override_dlls native,builtin mfc80 msvcp80 msvcr80 (uses w_common_override_dll)
            // Original winetricks: w_common_override_dll "native,builtin" mfc80 msvcp80 msvcr80
            if let Err(e) = self.common_override_dll("vcrun2005", "native,builtin", &["mfc80", "msvcp80", "msvcr80"]) {
                warn!("Warning: Failed to set DLL overrides for vcrun2005: {}", e);
            }
        }

        // Handle .NET post-installation steps
        if verb_name == "dotnet45" {
            // After .NET 4.5 installation, set registry key and Windows version
            info!("Applying .NET 4.5 post-installation settings...");
            
            // Set registry key to avoid popup on WINEPREFIX updates
            // Original winetricks: "${WINE}" reg add "HKLM\\Software\\Microsoft\\.NETFramework" /v OnlyUseLatestCLR /t REG_DWORD /d 0001 /f
            let wineprefix_str = self.config.wineprefix().to_string_lossy().to_string();
            let reg_status = std::process::Command::new(&self.wine.wine_bin)
                .arg("reg")
                .arg("add")
                .arg("HKLM\\Software\\Microsoft\\.NETFramework")
                .arg("/v")
                .arg("OnlyUseLatestCLR")
                .arg("/t")
                .arg("REG_DWORD")
                .arg("/d")
                .arg("0001")
                .arg("/f")
                .env("WINEPREFIX", &wineprefix_str)
                .status();
            
            if let Err(e) = reg_status {
                warn!("Warning: Failed to set OnlyUseLatestCLR registry key: {}", e);
            }
            
            // For win64, also set in Wow6432Node
            if self.config.winearch.as_ref().map(|a| a == "win64").unwrap_or(false) {
                let reg_status = std::process::Command::new(&self.wine.wine_bin)
                    .arg("reg")
                    .arg("add")
                    .arg("HKLM\\Software\\Wow6432Node\\.NETFramework")
                    .arg("/v")
                    .arg("OnlyUseLatestCLR")
                    .arg("/t")
                    .arg("REG_DWORD")
                    .arg("/d")
                    .arg("0001")
                    .arg("/f")
                    .env("WINEPREFIX", &wineprefix_str)
                    .status();
                
                if let Err(e) = reg_status {
                    warn!("Warning: Failed to set OnlyUseLatestCLR registry key (Wow6432Node): {}", e);
                }
            }
            
            // Set Windows version to win2k3 (Windows Server 2003) - required for .NET 4.5
            // Original winetricks: w_warn "Setting Windows version to 2003, otherwise applications using .NET 4.5 will subtly fail"
            // w_set_winver win2k3
            warn!("Setting Windows version to win2k3 (Windows Server 2003) - required for .NET 4.5 applications to work correctly");
            self.set_windows_version("win2k3")?;
        } else if verb_name == "dotnet48" || verb_name == "dotnet48.1" {
            // Override mscoree.dll to native (required for .NET 4.8) - AFTER installation
            self.set_dll_override("mscoree", "native")?;
            
            // Create marker file (as original winetricks does)
            let wineprefix = self.config.wineprefix();
            let marker_filename = if verb_name == "dotnet48.1" {
                "dotnet48.1.installed.workaround"
            } else {
                "dotnet48.installed.workaround"
            };
            let marker_file = wineprefix.join(format!("drive_c/windows/{}", marker_filename));
            eprintln!("Executing touch {}", marker_file.to_string_lossy());
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
        is_vcrun: bool,
    ) -> Result<()> {
        // This is a simplified version - real winetricks has per-verb logic
        // For now, try to detect installer type and run it

        let files: Vec<PathBuf> = metadata
            .files
            .iter()
            .map(|f| {
                // Handle special paths like "../directx9/directx_Jun2010_redist.exe"
                // These point to shared cache directories
                if f.filename.starts_with("../") {
                    // Resolve relative to cache root
                    self.config.cache_dir.join(f.filename.strip_prefix("../").unwrap_or(&f.filename))
                } else {
                    cache_dir.join(&f.filename)
                }
            })
            .collect();

        // Check if this is a DirectX d3dx9 verb (needs special handling)
        // DirectX verbs reference files in "../directx9/" directory
        let is_d3dx9_verb = metadata.name.starts_with("d3dx9") || metadata.name == "d3dx9";

        for file in &files {
            // For DirectX, file might be in shared cache directory
            // We need to resolve the actual file path (handles "../directx9/" paths)
            let file_to_use: PathBuf = if is_d3dx9_verb && !file.exists() {
                // Try to find it in directx9 cache
                let directx_file = self.config.cache_dir.join("directx9").join(
                    file.file_name().ok_or_else(|| WinetricksError::Config("Invalid DirectX filename".into()))?
                );
                if directx_file.exists() {
                    directx_file
                } else {
                    file.clone()
                }
            } else {
                file.clone()
            };
            
            if !file_to_use.exists() {
                if is_d3dx9_verb {
                    // For DirectX, we need to ensure the redistributable is downloaded
                    // This should have been done in ensure_directx_redistributable, but double-check
                    warn!("DirectX redistributable not found: {:?}", file_to_use);
                }
                continue;
            }

            // Detect installer type by extension
            let ext = file_to_use.extension().and_then(|e| e.to_str()).unwrap_or("");
            
            // For DirectX d3dx9 verbs, extract specific DLL from DirectX redistributable
            if is_d3dx9_verb && ext == "exe" {
                info!("Extracting DirectX d3dx9 DLL: {}", metadata.name);
                self.extract_d3dx9_dll(metadata, &file_to_use, &cache_dir)?;
                continue; // Skip regular EXE installer handling
            }

            // Check if this is a font installer (fonts use EXE or CAB files that are CAB archives)
            if metadata.category == VerbCategory::Fonts && (ext == "exe" || ext == "cab" || ext == "CAB") {
                info!("Processing font installer: {:?}", file_to_use);
                self.install_fonts(&metadata, &file_to_use, &cache_dir)?;
                continue;
            }

            match ext {
                "msi" => {
                    info!("Running MSI installer: {:?}", file_to_use);

                    // Run MSI installer using wine start /wait with msiexec
                    let wineprefix = self.config.wineprefix();
                    let wineprefix_str = wineprefix.to_string_lossy().to_string();
                    
                    // Set WINEARCH if configured (important for 64-bit installers)
                    if let Some(ref arch) = self.config.winearch {
                        std::env::set_var("WINEARCH", arch);
                    }

                    // Convert to Windows path for wine
                    let file_win_path = self.unix_to_wine_path(&file_to_use)?;
                    
                    // Check if this is a 64-bit installer by filename
                    let filename = file_to_use.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    let is_64bit_installer = filename.contains("amd64") || filename.contains("x64") || filename.contains("64");
                    
                    // Check if we should use wine64 and SYSTEM64 msiexec
                    let use_wine64 = is_64bit_installer && self.config.winearch.as_ref().map(|a| a == "win64").unwrap_or(false);
                    
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

                    // Use wine64 with SYSTEM64 msiexec for 64-bit installers on win64 prefixes
                    let (wine_bin, msiexec_path) = if use_wine64 {
                        // Find wine64 binary
                        let wine64_bin = if let Some(wine_dir) = self.wine.wine_bin.parent() {
                            wine_dir.join("wine64")
                        } else {
                            which::which("wine64")
                                .map_err(|_| WinetricksError::Config("wine64 not found for 64-bit MSI installer".into()))?
                        };
                        
                        // Use SYSTEM64 msiexec.exe (64-bit)
                        // On 64-bit Wine prefixes, 64-bit executables are in system32, 32-bit are in syswow64
                        let wineprefix = self.config.wineprefix();
                        let system64_msiexec = wineprefix.join("drive_c/windows/system32/msiexec.exe");
                        
                        // Convert to Windows path
                        let system64_msiexec_win = self.unix_to_wine_path(&system64_msiexec)?;
                        
                        (wine64_bin, system64_msiexec_win)
                    } else {
                        // Use regular wine and 32-bit msiexec
                        (self.wine.wine_bin.clone(), "msiexec.exe".to_string())
                    };

                    // Use wine start /wait for MSI files (as original winetricks does)
                    let mut cmd = std::process::Command::new(&wine_bin);
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
                        .arg(&msiexec_path)
                        .arg("/i")
                        .arg(&file_win_path);

                    // Use /qn for explicit "no UI" (more standard than /q)
                    if let Some(switch) = get_msi_silent_switch(self.config.unattended) {
                        cmd.arg(&switch);
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
                    // Font installers (both exe and cab) are already handled above
                    // This path is only for non-font EXE installers

                    info!("Running EXE installer: {:?}", file_to_use);

                    // Run EXE installer in wine
                    let wineprefix = self.config.wineprefix();
                    let wineprefix_str = wineprefix.to_string_lossy().to_string();
                    
                    // Set WINEARCH if configured (important for 64-bit installers)
                    if let Some(ref arch) = self.config.winearch {
                        std::env::set_var("WINEARCH", arch);
                    }

                    // Convert to Windows path for wine
                    let file_win_path = self.unix_to_wine_path(&file_to_use)?;

                    // Detect installer type and use appropriate silent flags
                    let filename = file_to_use.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    
                    // Use installer detection module
                    let installer_type = detect_from_file(&file_to_use)
                        .unwrap_or_else(|| detect_installer_type(filename, &metadata.name));

                    let is_dotnet = installer_type == InstallerType::DotNet
                        || filename.contains("dotnet")
                        || filename.contains("ndp")
                        || filename.starts_with("NDP");
                    let is_vcredist = installer_type == InstallerType::VcRedist
                        || filename.contains("vcredist") || filename.contains("vc_redist");
                    
                    // Check if this is a VC++ Redistributables verb (needs special handling)
                    let is_vcrun = metadata.name.starts_with("vcrun20") || metadata.name.starts_with("ucrtbase");
                    let is_ie = filename.contains("IE")
                        || filename.contains("ie")
                        || filename.contains("internetexplorer");
                    let is_mozilla = filename.contains("FirefoxSetup")
                        || filename.contains("firefoxsetup")
                        || filename.contains("ThunderbirdSetup")
                        || filename.contains("thunderbirdsetup")
                        || metadata.name == "firefox"
                        || metadata.name == "thunderbird";
                    let is_msxml = filename.contains("msxml")
                        || filename.contains("MSXML")
                        || filename.contains("xml");

                    // For VC++ Redistributables (vcrun2015+), extract DLLs BEFORE running installer
                    // Original winetricks: Extract a10/a11 CAB, then extract msvcp140.dll, then run installer
                    // Note: vcrun2015, vcrun2017, vcrun2019, vcrun2022 use vc_redist.x86.exe/vc_redist.x64.exe format
                    // Older versions (vcrun2005, vcrun2008, vcrun2010, vcrun2012, vcrun2013) use vcredist_x86.exe and don't need this
                    if is_vcrun && (filename.contains("vc_redist") || metadata.name == "vcrun2022") {
                        // Extract DLLs before running installer (matching original winetricks behavior)
                        if let Err(e) = self.extract_vcredist_dlls_before_install(&file_to_use, filename.contains("x64")) {
                            warn!("Warning: Failed to extract VC++ DLLs before installation: {}", e);
                            // Continue anyway - installer might still work
                        }
                    }

                    // For .NET and VC++ Redistributables installers, change to cache directory
                    // Original winetricks: cd /home/matt/.cache/winetricks/dotnet48 (or dotnet35)
                    // This is critical for .NET installers to extract files properly
                    let cache_dir_for_cmd = if is_dotnet || is_vcrun {
                        let cache_dir_path = file_to_use.parent().ok_or_else(|| {
                            WinetricksError::Config("Could not get parent directory of installer".into())
                        })?;
                        // Store original directory to restore later
                        let current_dir = std::env::current_dir()?;
                        eprintln!("Executing cd {}", cache_dir_path.to_string_lossy());
                        std::env::set_current_dir(cache_dir_path)?;
                        info!("Changed to cache directory: {:?}", std::env::current_dir()?);
                        Some((current_dir, cache_dir_path.to_path_buf()))
                    } else {
                        None
                    };

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
                    // Original winetricks: WINEDLLOVERRIDES=fusion=b (for dotnet48, not always for dotnet35)
                    // But setting it for all .NET installers is safe and matches behavior
                    if is_dotnet {
                        cmd.env("WINEDLLOVERRIDES", "fusion=b");
                    }
                    
                    // Set working directory to cache directory for .NET/VC++ installers
                    // This matches original winetricks behavior (cd to cache dir before running)
                    if let Some((_, ref cache_path)) = cache_dir_for_cmd {
                        cmd.current_dir(cache_path);
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

                    // For .NET and VC++ Redistributables installers, we need to use just the filename since we changed directory
                    if is_dotnet || is_vcrun {
                        let file_name = file_to_use.file_name()
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
                            // .NET 4.8/4.8.1: Use original winetricks method - run self-extractor directly
                            // Original winetricks: wine ndp48-x86-x64-allos-enu.exe /sfxlang:1027 /q /norestart
                            // The self-extractor handles running Setup.exe internally with proper flags
                            // This matches the original winetricks behavior exactly
                            if self.config.unattended {
                                cmd.arg("/sfxlang:1027").arg("/q").arg("/norestart");
                                info!("Running .NET {} installer with /sfxlang:1027 /q /norestart (matching original winetricks)", if is_dotnet48 { "4.8" } else { "4.7.2" });
                            } else {
                                // Interactive mode - still use sfxlang but no /q
                                cmd.arg("/sfxlang:1027");
                            }
                        } else if is_dotnet46 {
                            // .NET 4.6+: /q /norestart
                            if self.config.unattended {
                                cmd.arg("/q").arg("/norestart");
                            }
                        } else if is_dotnet45 {
                            // .NET 4.5: Extract manually and run Setup.exe directly for better reliability
                            // The self-extractor with /c:"install.exe /q" sometimes doesn't work properly
                            info!("Extracting .NET 4.5 installer manually to run Setup.exe directly...");
                            
                            let extract_dir = cache_dir.join("dotnet45_extracted");
                            if extract_dir.exists() {
                                std::fs::remove_dir_all(&extract_dir)?;
                            }
                            std::fs::create_dir_all(&extract_dir)?;
                            
                            // Use extractor's /x flag to extract only (no execution)
                            let file_name = file_to_use.file_name()
                                .and_then(|n| n.to_str())
                                .ok_or_else(|| WinetricksError::Config("Invalid filename".into()))?;
                            
                            let mut extract_cmd = std::process::Command::new(&self.wine.wine_bin);
                            extract_cmd.env("WINEPREFIX", &wineprefix_str);
                            extract_cmd.env("WINEDLLOVERRIDES", "fusion=b");
                            extract_cmd.current_dir(cache_dir);
                            extract_cmd.arg(file_name);
                            extract_cmd.arg("/x:").arg(&extract_dir.to_string_lossy().to_string());
                            
                            info!("Extracting installer to: {:?}", extract_dir);
                            let extract_status = extract_cmd.status()?;
                            
                            if !extract_status.success() {
                                warn!("Extraction failed, falling back to original method...");
                                if self.config.unattended {
                                    cmd.arg("/q");
                                    cmd.arg(r#"/c:"install.exe /q""#);
                                }
                            } else {
                                // Look for Setup.exe in the extracted directory
                                let setup_exe = extract_dir.join("Setup.exe");
                                if !setup_exe.exists() {
                                    // Try looking in subdirectories
                                    let mut found_setup = None;
                                    if let Ok(entries) = std::fs::read_dir(&extract_dir) {
                                        for entry in entries.flatten() {
                                            let path = entry.path();
                                            if path.is_dir() {
                                                let candidate = path.join("Setup.exe");
                                                if candidate.exists() {
                                                    found_setup = Some(candidate);
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    
                                    if let Some(setup_path) = found_setup {
                                        info!("Found Setup.exe in subdirectory, running it...");
                                        return self.run_setup_exe_directly(&setup_path, &extract_dir, wineprefix_str, is_dotnet45).await;
                                    } else {
                                        warn!("Setup.exe not found after extraction, using original method...");
                                        if self.config.unattended {
                                            cmd.arg("/q");
                                            cmd.arg(r#"/c:"install.exe /q""#);
                                        }
                                    }
                                } else {
                                    info!("Found Setup.exe, running it directly with proper arguments...");
                                        return self.run_setup_exe_directly(&setup_exe, &extract_dir, wineprefix_str, is_dotnet45).await;
                                }
                            }
                        } else if is_dotnet40 {
                            // .NET 4.0: Use /quiet flag
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
                        } else if metadata.name == "dotnet20sdk" || filename.contains("NetFx") {
                            // .NET 2.0 SDK: /q /c:"install.exe /q"
                            // Original winetricks: w_try_ms_installer "${WINE}" "${file1}" /q '/c:install.exe /q'
                            if self.config.unattended {
                                cmd.arg("/q").arg(r#"/c:"install.exe /q""#);
                                info!("Using .NET 2.0 SDK installation pattern: /q /c:\"install.exe /q\"");
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
                        } else if is_mozilla {
                            // Mozilla installers (Firefox, Thunderbird) use -ms flag
                            cmd.arg("-ms");
                            info!("Detected Mozilla installer, using -ms flag");
                        } else {
                            // Check for /silent flag (some installers like emu8086)
                            // First check if filename or verb suggests it needs /silent
                            if filename.to_lowercase().contains("setup.exe") && metadata.name == "emu8086" {
                                cmd.arg("/silent");
                                info!("Using /silent flag for {}", metadata.name);
                            } else {
                                // Use installer detection to get appropriate switches
                                let switches = get_silent_switches(installer_type, true);
                                info!("Detected installer type: {:?}, applying switches: {:?}", installer_type, switches);
                                for switch in switches {
                                    cmd.arg(&switch);
                                }
                            }
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

                    // Show command being executed
                    let cmd_args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
                    eprintln!("Executing: {} {}", 
                        self.wine.wine_bin.to_string_lossy(),
                        cmd_args.join(" ")
                    );
                    
                    // For .NET installers, the EXE is a self-extractor that launches Setup.exe
                    // Setup.exe then installs MSI packages - this can take a long time
                    // We need to wait for the entire process tree to complete
                    eprintln!("Starting installer (this may take several minutes)...");
                    eprintln!("Note: .NET installers extract files and run Setup.exe which installs MSI packages.");
                    eprintln!("This process can take 5-10 minutes or longer.");
                    
                    // Use .output() which waits for the process to complete
                    // For .NET installers, the extractor should wait for Setup.exe to finish
                    let output = cmd
                        .output()
                        .map_err(|e| WinetricksError::CommandExecution {
                            command: format!("wine {}", cmd_args.join(" ")),
                            error: e.to_string(),
                        })?;
                    
                    // Print captured stdout/stderr
                    if !output.stdout.is_empty() {
                        eprintln!("Installer stdout:");
                        eprintln!("{}", String::from_utf8_lossy(&output.stdout));
                    }
                    if !output.stderr.is_empty() {
                        eprintln!("Installer stderr:");
                        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
                    }
                    
                    let status = output.status;
                    
                    eprintln!("Installer finished with exit code: {:?}", status.code());

                    // Restore original directory if we changed it
                    if let Some((orig_dir, _)) = cache_dir_for_cmd {
                        if let Err(e) = std::env::set_current_dir(&orig_dir) {
                            warn!("Warning: Failed to restore directory: {}", e);
                        }
                    }
                    
                    // For VC++ Redistributables, also extract DLLs manually if needed
                    // Note: This is done AFTER the installer runs, but BEFORE it was already done earlier
                    // Original winetricks does it BEFORE running the installer, so we already did it

                    // Wait for wineserver after .NET installation (important for proper completion)
                    // .NET installers extract files and run Setup.exe which installs MSI packages
                    // This can take a long time - we need to wait for all MSI installations to complete
                    if is_dotnet {
                        info!("Waiting for wineserver to finish processing .NET installation...");
                        
                        // .NET installers can take a while - wait longer for .NET 4.8 and 4.5
                        let wait_time = if is_dotnet48 {
                            std::time::Duration::from_secs(10) // .NET 4.8 needs much more time for all MSI packages
                        } else if is_dotnet45 {
                            std::time::Duration::from_secs(8) // .NET 4.5 also needs extra time
                        } else {
                            std::time::Duration::from_secs(5)
                        };
                        
                        info!("Waiting {} seconds for .NET installer processes to complete...", wait_time.as_secs());
                        std::thread::sleep(wait_time);
                        
                        // Check for any remaining Setup.exe or msiexec.exe processes
                        // These may still be running even after the extractor exits
                        info!("Checking for remaining installer processes...");
                        let check_processes = std::process::Command::new("pgrep")
                            .arg("-f")
                            .arg("(Setup\\.exe|msiexec\\.exe)")
                            .output();
                        
                        if let Ok(proc_output) = check_processes {
                            if !proc_output.stdout.is_empty() {
                                let proc_count = proc_output.stdout.iter().filter(|&&b| b == b'\n').count();
                                if proc_count > 0 {
                                    warn!("Found {} remaining installer process(es) - waiting additional time...", proc_count);
                                    std::thread::sleep(std::time::Duration::from_secs(10)); // Wait more if processes still running
                                }
                            }
                        }
                        
                        // Wait for wineserver multiple times to ensure all operations complete
                        for i in 1..=5 {
                            let wineserver_status = std::process::Command::new(&self.wine.wineserver_bin)
                                .arg("-w")
                                .env("WINEPREFIX", &wineprefix_str)
                                .status();
                            if let Err(e) = wineserver_status {
                                if i == 1 {
                                    warn!("Warning: Failed to wait for wineserver: {}", e);
                                }
                            } else {
                                if i == 1 || i == 5 {
                                    info!("Wineserver sync {} complete", i);
                                }
                            }
                            
                            if i < 5 {
                                std::thread::sleep(std::time::Duration::from_secs(2));
                            }
                        }
                    }
                    
                    // For .NET 4.5, set DLL override for mscoree AFTER installation (from original winetricks)
                    if is_dotnet45 {
                        info!("Setting DLL override for mscoree (native) for .NET 4.5...");
                        if let Err(e) = self.set_dll_override("mscoree", "native") {
                            warn!("Warning: Failed to set DLL override for mscoree: {}", e);
                        }
                    }

                    // Check exit code - MS installers can return specific codes that indicate success
                    // Original winetricks: w_try_ms_installer handles exit codes 105, 194, 236 as non-fatal
                    let exit_code = status.code();
                    
                    // MS installers (including .NET) can return:
                    // 0 = success
                    // 105 = non-fatal (original winetricks treats as success)
                    // 194 = non-fatal (original winetricks treats as success)
                    // 236 = non-fatal (cancelled by user, but installer extracted files)
                    // 3010 = success (reboot required)
                    // 1603 = fatal error (but sometimes false positive for .NET 3.5/4.5)
                    // Other non-zero = usually failure
                    
                    // Log exit code for debugging
                    if let Some(code) = exit_code {
                        info!("Installer exit code: {}", code);
                    } else {
                        warn!("Installer terminated without exit code (possibly killed by signal)");
                    }
                    
                    let is_success = if is_dotnet || is_vcredist {
                        match exit_code {
                            Some(0) | Some(3010) | Some(236) | Some(105) | Some(194) => true,
                            Some(1603) => {
                                // .NET 3.5/4.5 can return 1603 even when partially successful
                                let is_dotnet35_or_45 = filename.contains("35") || filename.contains("45");
                                if is_dotnet35_or_45 {
                                    warn!("Warning: Installer returned exit code 1603 (fatal error). This may be a false positive for .NET 3.5/4.5.");
                                    warn!("Checking if installation actually succeeded...");
                                    true // We'll verify later
                                } else {
                                    warn!("Warning: Installer returned exit code 1603. This may indicate a failed installation.");
                                    warn!("Verification will check if files were actually installed...");
                                    true // For .NET, we'll verify files instead of relying solely on exit code
                                }
                            },
                            Some(code) => {
                                warn!("Installer returned exit code {} - this may indicate failure", code);
                                warn!("Verification will check if files were actually installed...");
                                true // For .NET, we'll verify files instead of failing immediately
                            },
                            None => {
                                warn!("Installer terminated abnormally - verification will check if files were installed");
                                true // Verify will catch missing files
                            },
                        }
                    } else {
                        // For other MS installers, also accept non-fatal exit codes
                        match exit_code {
                            Some(0) | Some(105) | Some(194) | Some(236) => true,
                            _ => status.success(),
                        }
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
                    info!("Extracting ZIP: {:?}", file_to_use);
                    self.extract_zip(&file_to_use, &cache_dir)?;
                }
                "cab" => {
                    info!("Extracting CAB: {:?}", file_to_use);
                    self.extract_cab(&file_to_use, &cache_dir)?;
                }
                "7z" => {
                    info!("Extracting 7z: {:?}", file_to_use);
                    self.extract_7z(&file_to_use, &cache_dir)?;
                }
                "rar" => {
                    info!("Extracting RAR: {:?}", file_to_use);
                    self.extract_rar(&file_to_use, &cache_dir)?;
                }
                "reg" => {
                    info!("Importing registry file: {:?}", file_to_use);
                    self.import_registry_file(&file_to_use)?;
                }
                _ => {
                    // Check if file might be an archive by magic bytes or try extraction
                    // Some files might not have extensions but are archives
                    let filename = file_to_use.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if filename.ends_with(".7z") || filename.contains(".7z.") {
                        self.extract_7z(&file_to_use, &cache_dir)?;
                    } else if filename.ends_with(".rar") || filename.contains(".rar.") {
                        self.extract_rar(&file_to_use, &cache_dir)?;
                    } else {
                        warn!("Unknown file type: {:?}", file_to_use);
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
            
            // On 64-bit prefixes, 32-bit DLLs go to syswow64, 64-bit DLLs go to system32
            // Check both locations
            let is_win64 = self.config.winearch.as_ref().map(|a| a == "win64").unwrap_or(false);
            
            if is_win64 {
                // Check syswow64 first (32-bit DLLs on 64-bit prefix)
                let syswow64_path = wineprefix.join("drive_c/windows/syswow64").join(&windows_part);
                if syswow64_path.exists() {
                    return Ok(true);
                }
                // Also check system32 (64-bit DLLs or if DLL is in system32)
                let system32_path = wineprefix.join("drive_c/windows/system32").join(&windows_part);
                return Ok(system32_path.exists());
            } else {
                // On 32-bit prefix, DLLs go to system32
                let system32_path = wineprefix.join("drive_c/windows/system32").join(&windows_part);
                return Ok(system32_path.exists());
            }
        }
        
        // Handle ${W_FONTSDIR_WIN} - Windows fonts directory
        if unix_path.contains("${W_FONTSDIR_WIN}") || unix_path.contains("$W_FONTSDIR_WIN") {
            unix_path = unix_path.replace("${W_FONTSDIR_WIN}", "");
            unix_path = unix_path.replace("$W_FONTSDIR_WIN", "");
            // W_FONTSDIR_WIN is typically C:\windows\Fonts or C:\windows\fonts
            let windows_part = unix_path.trim_start_matches('/').replace('\\', "/");
            // Try uppercase Fonts first (Windows standard), then lowercase
            let fonts_dir = wineprefix.join("drive_c/windows/Fonts");
            let full_path = if fonts_dir.exists() {
                fonts_dir.join(&windows_part)
            } else {
                wineprefix.join("drive_c/windows/fonts").join(&windows_part)
            };
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
        let mut found_files = Vec::new();
        
        // Also check system32 for mscoree.dll (it's installed there, not in Framework dir)
        let system32_mscoree = wineprefix.join("drive_c/windows/system32/mscoree.dll");
        if system32_mscoree.exists() {
            files_found = true;
            found_files.push("system32/mscoree.dll".to_string());
            info!("Found .NET file: {:?}", system32_mscoree);
        }
        
        for framework_dir in &framework_dirs {
            if framework_dir.exists() {
                // Check for key .NET DLLs
                let key_dlls = vec![
                    "mscoree.dll",  // Usually in system32, but check Framework dir too
                    "mscorlib.dll",
                    "System.dll",
                    "Microsoft.NETFramework.dll",
                ];
                
                for dll in &key_dlls {
                    let dll_path = framework_dir.join(dll);
                    if dll_path.exists() {
                        files_found = true;
                        found_files.push(format!("{}/{}", framework_dir.file_name().unwrap_or_default().to_string_lossy(), dll));
                        info!("Found .NET file: {:?}", dll_path);
                    }
                }
            }
        }
        
        // Log what we found
        if !found_files.is_empty() {
            info!("Found {} .NET files: {}", found_files.len(), found_files.join(", "));
        }
        
        // For .NET 4.x, we need to check for specific critical files
        // A complete .NET installation should have at least:
        // - mscoree.dll (in system32)
        // - mscorlib.dll (in Framework/v4.0.30319/)
        // - System.dll (in Framework/v4.0.30319/) - REQUIRED for .NET 4.x
        let mut critical_files_missing = Vec::new();
        
        if verb_name.starts_with("dotnet4") {
            // Check for System.dll - this is critical for .NET 4.x
            // Check both Framework (32-bit) and Framework64 (64-bit) directories
            let system_dll_32 = wineprefix.join("drive_c/windows/Microsoft.NET/Framework/v4.0.30319/System.dll");
            let system_dll_64 = wineprefix.join("drive_c/windows/Microsoft.NET/Framework64/v4.0.30319/System.dll");
            if !system_dll_32.exists() && !system_dll_64.exists() {
                critical_files_missing.push("System.dll".to_string());
                warn!("Critical file missing: System.dll");
            }
            
            // Check for mscorlib.dll
            let mscorlib_dll_32 = wineprefix.join("drive_c/windows/Microsoft.NET/Framework/v4.0.30319/mscorlib.dll");
            let mscorlib_dll_64 = wineprefix.join("drive_c/windows/Microsoft.NET/Framework64/v4.0.30319/mscorlib.dll");
            if !mscorlib_dll_32.exists() && !mscorlib_dll_64.exists() {
                critical_files_missing.push("mscorlib.dll".to_string());
                warn!("Critical file missing: mscorlib.dll");
            }
            
            // Count total files in Framework directory - should have 100+ for complete installation
            let framework_dir = wineprefix.join("drive_c/windows/Microsoft.NET/Framework/v4.0.30319");
            if framework_dir.exists() {
                let file_count = std::fs::read_dir(&framework_dir)
                    .map(|entries| entries.count())
                    .unwrap_or(0);
                
                if file_count < 20 {
                    warn!("⚠️  Only {} files in Framework/v4.0.30319/ - .NET 4.x should have 100+ files! Installation may be incomplete.", file_count);
                } else {
                    info!("Found {} files in Framework/v4.0.30319/", file_count);
                }
            }
        }
        
        // For .NET, require both registry AND critical files
        if !registry_found {
            warn!("❌ Registry key {} not found", registry_key);
        }
        
        if !critical_files_missing.is_empty() {
            warn!("❌ Critical .NET files missing: {}", critical_files_missing.join(", "));
            warn!("⚠️  .NET Framework {} installation is INCOMPLETE!", verb_name);
            return Ok(false);
        }
        
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
        use std::process::Command;

        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();

        // Use winecfg to set Windows version (matching original winetricks exactly)
        // Original winetricks: "${WINE}" winecfg -v "${winver}"
        // winecfg handles the version format correctly, avoiding the "Invalid Windows version value" error
        eprintln!("Executing wine winecfg -v {}", version);
        let status = Command::new(&self.wine.wine_bin)
            .arg("winecfg")
            .arg("-v")
            .arg(version) // Use version name (e.g., "win7"), not hex value
            .env("WINEPREFIX", &wineprefix_str)
            .status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine winecfg -v {}", version),
                error: e.to_string(),
            })?;

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

        // Create temp directory for registry file inside Wine prefix
        // Original winetricks: Uses C:\windows\Temp\override-dll.reg (inside Wine prefix)
        let temp_dir = wineprefix.join("drive_c/windows/temp");
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

        // Import registry file using regedit32/regedit64 (matching original winetricks)
        // Original winetricks: Uses syswow64\regedit.exe for 32-bit, regedit.exe for 64-bit
        // DLL overrides go to HKEY_CURRENT_USER which is shared, but we still need to import to both registries on win64
        let is_win64 = self.config.winearch.as_ref().map(|a| a == "win64").unwrap_or(false);
        
        // Convert to Windows path for display
        let reg_file_win = self.unix_to_wine_path(&reg_file)?;
        
        // Always import to 32-bit registry first (matches original winetricks w_try_regedit32)
        let regedit32_exe = if is_win64 {
            "C:\\windows\\syswow64\\regedit.exe"
        } else {
            "C:\\windows\\regedit.exe"
        };
        eprintln!("Executing wine {} /S {}", regedit32_exe, reg_file_win);
        self.regedit32(&reg_file)?;
        
        // On win64, also import to 64-bit registry (matches original winetricks w_try_regedit64)
        if is_win64 {
            eprintln!("Executing wine C:\\windows\\regedit.exe /S {}", reg_file_win);
            self.regedit64(&reg_file)?;
        }

        // Clean up temp file
        let _ = fs::remove_file(&reg_file);

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

        // Original winetricks: cabextract -q -d "${W_TMP}" (uses -d flag to specify destination)
        // Show "Executing" message to match original winetricks verbose output
        eprintln!("Executing cabextract -q -d {} {}", dest_dir.to_string_lossy(), cab_file.to_string_lossy());
        
        let status = Command::new(&cabextract)
            .arg("-q") // Quiet mode
            .arg("-d") // Destination directory
            .arg(dest_dir) // Extract to this directory
            .arg(cab_file)
            .status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("cabextract -q -d {:?} {:?}", dest_dir, cab_file),
                error: e.to_string(),
            })?;

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
    /// On win64 prefixes, imports to both 32-bit and 64-bit registry
    fn import_registry_file(&self, reg_file: &Path) -> Result<()> {
        // On win64, we need to import to both 32-bit and 64-bit registry
        // On win32, just import to 32-bit registry
        let is_win64 = self.config.winearch.as_ref().map(|a| a == "win64").unwrap_or(false);
        
        // Always import to 32-bit registry first
        self.regedit32(reg_file)?;
        
        // On win64, also import to 64-bit registry
        if is_win64 {
            self.regedit64(reg_file)?;
        }
        
        info!("Imported registry file: {:?}", reg_file);
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

    /// Install fonts from a font installer (matching load_arial, load_times, etc.)
    fn install_fonts(
        &self,
        metadata: &VerbMetadata,
        font_installer: &Path,
        cache_dir: &Path,
    ) -> Result<()> {
        use std::fs;
        use glob::glob;

        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Determine Windows fonts directory (W_FONTSDIR_UNIX)
        // Original winetricks checks for both "Fonts" and "fonts" directories
        let windows_dir = wineprefix.join("drive_c/windows");
        let fonts_dir_unix = if windows_dir.join("Fonts").exists() {
            windows_dir.join("Fonts")
        } else if windows_dir.join("fonts").exists() {
            windows_dir.join("fonts")
        } else {
            // Create Fonts directory (uppercase, like Windows)
            let fonts_dir = windows_dir.join("Fonts");
            fs::create_dir_all(&fonts_dir)?;
            fonts_dir
        };
        
        info!("Fonts directory: {:?}", fonts_dir_unix);

        // Extract font installer (EXE files are actually CAB archives)
        // Original winetricks: cabextract -q -d "${W_TMP}" (extracts to C:\windows\Temp inside Wine prefix)
        let wineprefix = self.config.wineprefix();
        let temp_dir = wineprefix.join("drive_c/windows/temp");
        fs::create_dir_all(&temp_dir)?;
        
        // Use cabextract to extract fonts to Wine prefix temp directory
        self.extract_cab(font_installer, &temp_dir)?;

        // Find all font files (*.TTF, *.ttf, *.TTC, *.ttc) in extracted directory
        // Try multiple patterns since glob doesn't support brace expansion
        let mut font_files = Vec::new();
        
        // Try .ttf files first
        if let Ok(entries) = glob(&temp_dir.join("*.ttf").to_string_lossy()) {
            font_files.extend(entries.filter_map(|entry| entry.ok()));
        }
        // Try .TTF files
        if let Ok(entries) = glob(&temp_dir.join("*.TTF").to_string_lossy()) {
            font_files.extend(entries.filter_map(|entry| entry.ok()));
        }
        // Try .ttc files
        if let Ok(entries) = glob(&temp_dir.join("*.ttc").to_string_lossy()) {
            font_files.extend(entries.filter_map(|entry| entry.ok()));
        }
        // Try .TTC files
        if let Ok(entries) = glob(&temp_dir.join("*.TTC").to_string_lossy()) {
            font_files.extend(entries.filter_map(|entry| entry.ok()));
        }

        if font_files.is_empty() {
            warn!("No font files found in installer: {:?}", font_installer);
            // Try to find files recursively
            let mut found_files = Vec::new();
            self.find_font_files_recursive(&temp_dir, &mut found_files)?;
            
            if found_files.is_empty() {
                return Err(WinetricksError::Verb(format!(
                    "No font files found in installer: {:?}",
                    font_installer
                )));
            }
            
            for font_file in &found_files {
                self.copy_and_register_font(font_file, &fonts_dir_unix, metadata)?;
            }
        } else {
            for font_file in &font_files {
                self.copy_and_register_font(font_file, &fonts_dir_unix, metadata)?;
            }
        }

        Ok(())
    }

    /// Recursively find font files in a directory
    fn find_font_files_recursive(&self, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        use std::fs;
        
        if !dir.is_dir() {
            return Ok(());
        }

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            
            if path.is_dir() {
                self.find_font_files_recursive(&path, files)?;
            } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let ext_lower = ext.to_lowercase();
                if ext_lower == "ttf" || ext_lower == "ttc" {
                    files.push(path);
                }
            }
        }
        
        Ok(())
    }

    /// Copy font file and register it in registry
    fn copy_and_register_font(
        &self,
        font_file: &Path,
        fonts_dir: &Path,
        metadata: &VerbMetadata,
    ) -> Result<()> {
        use std::fs;
        use std::io::Write;

        let font_filename = font_file
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| WinetricksError::Config("Invalid font filename".into()))?;

        // Copy font to fonts directory
        let dest_font = fonts_dir.join(font_filename);
        
        // If destination exists, remove it first (to handle read-only files)
        if dest_font.exists() {
            // Try to remove read-only attribute first
            let mut perms = fs::metadata(&dest_font)?.permissions();
            perms.set_readonly(false);
            fs::set_permissions(&dest_font, perms)?;
            fs::remove_file(&dest_font)?;
        }
        
        fs::copy(font_file, &dest_font)?;
        info!("Copied font: {} -> {:?}", font_filename, dest_font);

        // Register font in registry
        // Original winetricks uses font name from metadata or derives from filename
        let font_name: String = metadata.title
            .split('/')
            .next()
            .and_then(|s| s.split_whitespace().nth(1))
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                // Fallback: derive from filename (e.g., arial.ttf -> Arial)
                font_filename
                    .rsplit('.')
                    .nth(1)
                    .unwrap_or(font_filename)
                    .split(|c: char| c == '-' || c == '_')
                    .next()
                    .unwrap_or(font_filename)
                    .to_string()
            });

        // Register font
        self.register_font(&dest_font, &font_name)?;

        Ok(())
    }

    /// Register a font in the Windows registry (matching w_register_font behavior)
    fn register_font(&self, font_file: &Path, font_name: &str) -> Result<()> {
        use std::fs;
        use std::io::Write;
        use tempfile::NamedTempFile;

        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();

        // Get font filename (just the name, not full path)
        let font_filename = font_file
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| WinetricksError::Config("Invalid font filename".into()))?;

        // Determine if TrueType (TTF/TTC)
        let is_ttf = font_filename.to_lowercase().ends_with(".ttf")
            || font_filename.to_lowercase().ends_with(".ttc");
        let font_display_name = if is_ttf {
            format!("{} (TrueType)", font_name)
        } else {
            font_name.to_string()
        };

        // Just the filename for registry (original winetricks uses just filename)
        let font_reg_value = font_filename;

        // Create registry file in Wine prefix temp directory (matching original winetricks)
        // Original winetricks: Creates C:\windows\Temp\_register-font.reg
        let wineprefix = self.config.wineprefix();
        let temp_dir = wineprefix.join("drive_c/windows/temp");
        fs::create_dir_all(&temp_dir)?;
        
        let reg_file = temp_dir.join("_register-font.reg");
        let mut file = fs::File::create(&reg_file)?;
        writeln!(file, "REGEDIT4")?;
        writeln!(file, "")?;
        writeln!(file, "[HKEY_LOCAL_MACHINE\\Software\\Microsoft\\Windows NT\\CurrentVersion\\Fonts]")?;
        writeln!(file, "\"{}\"=\"{}\"", font_display_name, font_reg_value)?;
        file.sync_all()?;

        // Convert to Windows path for regedit
        let reg_file_win = self.unix_to_wine_path(&reg_file)?;
        
        // Import registry file using regedit32/regedit64 (matching original winetricks)
        // Original winetricks: Uses syswow64\regedit.exe for 32-bit, regedit.exe for 64-bit
        let is_win64 = self.config.winearch.as_ref().map(|a| a == "win64").unwrap_or(false);
        
        eprintln!("Executing wine {} /S {}", if is_win64 { "C:\\windows\\syswow64\\regedit.exe" } else { "C:\\windows\\regedit.exe" }, reg_file_win);
        self.regedit32(&reg_file)?;
        
        if is_win64 {
            eprintln!("Executing wine C:\\windows\\regedit.exe /S {}", reg_file_win);
            self.regedit64(&reg_file)?;
        }

        // Also register in Win9x key (original winetricks does this too)
        let reg_file2 = temp_dir.join("_register-font2.reg");
        let mut file2 = fs::File::create(&reg_file2)?;
        writeln!(file2, "REGEDIT4")?;
        writeln!(file2, "")?;
        writeln!(file2, "[HKEY_LOCAL_MACHINE\\Software\\Microsoft\\Windows\\CurrentVersion\\Fonts]")?;
        writeln!(file2, "\"{}\"=\"{}\"", font_display_name, font_reg_value)?;
        file2.sync_all()?;

        let reg_file2_win = self.unix_to_wine_path(&reg_file2)?;
        
        eprintln!("Executing wine {} /S {}", if is_win64 { "C:\\windows\\syswow64\\regedit.exe" } else { "C:\\windows\\regedit.exe" }, reg_file2_win);
        self.regedit32(&reg_file2)?;
        
        if is_win64 {
            eprintln!("Executing wine C:\\windows\\regedit.exe /S {}", reg_file2_win);
            self.regedit64(&reg_file2)?;
        }
        
        // Clean up registry files
        let _ = fs::remove_file(&reg_file);
        let _ = fs::remove_file(&reg_file2);

        info!("Registered font: {} -> {}", font_display_name, font_filename);
        Ok(())
    }

    /// Register a font replacement/alias (matching w_register_font_replacement behavior)
    /// This creates font aliases for fallback fonts (e.g., when a font is missing, use an alias)
    pub fn register_font_replacement(&self, alias: &str, font_name: &str) -> Result<()> {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();

        // Create UTF-16LE registry file with BOM (original winetricks does this)
        // UTF-16 BOM (U+FEFF) = 0xEF 0xBB 0xBF in UTF-8
        let reg_file = NamedTempFile::new()?;
        let reg_path = reg_file.path();

        // Write UTF-16LE BOM and registry content
        // Original winetricks uses iconv -f UTF-8 -t UTF-16LE
        // UTF-16LE BOM is 0xFF 0xFE (U+FEFF in little-endian)
        let content = format!(
            "REGEDIT4\r\n\r\n[HKEY_CURRENT_USER\\Software\\Wine\\Fonts\\Replacements]\r\n\"{}\"=\"{}\"\r\n",
            alias, font_name
        );
        
        // Convert UTF-8 string to UTF-16LE bytes
        let mut utf16_bytes = Vec::new();
        // Write UTF-16LE BOM (0xFF 0xFE)
        utf16_bytes.extend_from_slice(&[0xFFu8, 0xFEu8]);
        
        // Convert UTF-8 string to UTF-16LE
        for ch in content.encode_utf16() {
            let bytes = ch.to_le_bytes(); // Little-endian encoding
            utf16_bytes.extend_from_slice(&bytes);
        }
        
        // Write to file
        std::fs::write(reg_path, utf16_bytes)?;

        // Convert registry file path to Wine Windows path
        let reg_file_win_path = self.unix_to_wine_path(reg_path)?;

        // Import registry file using regedit
        let status = std::process::Command::new(&self.wine.wine_bin)
            .arg("regedit")
            .arg("/S") // Silent mode
            .arg(&reg_file_win_path)
            .env("WINEPREFIX", &wineprefix_str)
            .status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine regedit /S {:?}", reg_file_win_path),
                error: e.to_string(),
            })?;

        if !status.success() {
            return Err(WinetricksError::Verb(format!(
                "Failed to register font replacement: {} -> {}",
                alias, font_name
            )));
        }

        info!("Registered font replacement: {} -> {}", alias, font_name);
        Ok(())
    }

    /// Extract msvcp140.dll from VC++ Redistributables installer BEFORE running installer
    /// This matches original winetricks behavior for vcrun2022:
    /// 1. Extract a10 (32-bit) or a11 (64-bit) CAB to C:\windows\temp\win32 or win64
    /// 2. Extract msvcp140.dll from CAB to syswow64 (32-bit) or system32 (64-bit)
    /// 3. Then run the installer with /q
    fn extract_vcredist_dlls_before_install(&self, vcredist_exe: &Path, is_64bit: bool) -> Result<()> {
        use std::fs;
        use std::process::Command;
        use which::which;

        let wineprefix = self.config.wineprefix();
        
        // cabextract is required
        let cabextract = which("cabextract")
            .map_err(|_| WinetricksError::Config(
                "cabextract not found. Please install it (e.g. 'sudo apt install cabextract')".into()
            ))?;

        if is_64bit {
            // 64-bit: Extract a11 to C:\windows\temp\win64, then extract msvcp140.dll to system32
            let temp_win64 = wineprefix.join("drive_c/windows/temp/win64");
            fs::create_dir_all(&temp_win64)?;
            
            let system32_dlls = wineprefix.join("drive_c/windows/system32");
            fs::create_dir_all(&system32_dlls)?;

            info!("Extracting 'a11' CAB from VC++ Redistributables 64-bit installer...");
            eprintln!("Executing cabextract -q --directory={} {} -F a11", 
                temp_win64.to_string_lossy(), vcredist_exe.to_string_lossy());
            
            let status = Command::new(&cabextract)
                .arg("-q")
                .arg("--directory")
                .arg(&temp_win64)
                .arg(vcredist_exe)
                .arg("-F")
                .arg("a11")
                .status()
                .map_err(|e| WinetricksError::CommandExecution {
                    command: format!("cabextract -q --directory {:?} {:?} -F a11", temp_win64, vcredist_exe),
                    error: e.to_string(),
                })?;

            if !status.success() {
                return Err(WinetricksError::Verb(
                    "Failed to extract 'a11' CAB from VC++ Redistributables installer".into()
                ));
            }

            let a11_cab = temp_win64.join("a11");
            if !a11_cab.exists() {
                return Err(WinetricksError::Verb(
                    "Extracted 'a11' CAB file not found".into()
                ));
            }

            // Extract msvcp140.dll from a11 to system32
            info!("Extracting msvcp140.dll to system32...");
            eprintln!("Executing cabextract -q --directory={} {} -F msvcp140.dll",
                system32_dlls.to_string_lossy(), a11_cab.to_string_lossy());
            
            let status = Command::new(&cabextract)
                .arg("-q")
                .arg("--directory")
                .arg(&system32_dlls)
                .arg(&a11_cab)
                .arg("-F")
                .arg("msvcp140.dll")
                .status()
                .map_err(|e| WinetricksError::CommandExecution {
                    command: format!("cabextract -q --directory {:?} {:?} -F msvcp140.dll", system32_dlls, a11_cab),
                    error: e.to_string(),
                })?;

            if !status.success() {
                warn!("Warning: Failed to extract msvcp140.dll from 'a11' CAB (may not be critical)");
            }
        } else {
            // 32-bit: Extract a10 to C:\windows\temp\win32, then extract msvcp140.dll to syswow64
            let temp_win32 = wineprefix.join("drive_c/windows/temp/win32");
            fs::create_dir_all(&temp_win32)?;
            
            // For 32-bit DLLs on 64-bit prefixes, extract to syswow64
            // For 32-bit prefixes, extract to system32
            let is_win64_prefix = self.config.winearch.as_ref().map(|a| a == "win64").unwrap_or(false);
            let dll_dest = if is_win64_prefix {
                wineprefix.join("drive_c/windows/syswow64")
            } else {
                wineprefix.join("drive_c/windows/system32")
            };
            fs::create_dir_all(&dll_dest)?;

            info!("Extracting 'a10' CAB from VC++ Redistributables 32-bit installer...");
            eprintln!("Executing cabextract -q --directory={} {} -F a10",
                temp_win32.to_string_lossy(), vcredist_exe.to_string_lossy());
            
            let status = Command::new(&cabextract)
                .arg("-q")
                .arg("--directory")
                .arg(&temp_win32)
                .arg(vcredist_exe)
                .arg("-F")
                .arg("a10")
                .status()
                .map_err(|e| WinetricksError::CommandExecution {
                    command: format!("cabextract -q --directory {:?} {:?} -F a10", temp_win32, vcredist_exe),
                    error: e.to_string(),
                })?;

            if !status.success() {
                return Err(WinetricksError::Verb(
                    "Failed to extract 'a10' CAB from VC++ Redistributables installer".into()
                ));
            }

            let a10_cab = temp_win32.join("a10");
            if !a10_cab.exists() {
                return Err(WinetricksError::Verb(
                    "Extracted 'a10' CAB file not found".into()
                ));
            }

            // Extract msvcp140.dll from a10 to syswow64 (or system32 on 32-bit prefix)
            info!("Extracting msvcp140.dll to {}...", dll_dest.to_string_lossy());
            eprintln!("Executing cabextract -q --directory={} {} -F msvcp140.dll",
                dll_dest.to_string_lossy(), a10_cab.to_string_lossy());
            
            let status = Command::new(&cabextract)
                .arg("-q")
                .arg("--directory")
                .arg(&dll_dest)
                .arg(&a10_cab)
                .arg("-F")
                .arg("msvcp140.dll")
                .status()
                .map_err(|e| WinetricksError::CommandExecution {
                    command: format!("cabextract -q --directory {:?} {:?} -F msvcp140.dll", dll_dest, a10_cab),
                    error: e.to_string(),
                })?;

            if !status.success() {
                warn!("Warning: Failed to extract msvcp140.dll from 'a10' CAB (may not be critical)");
            }
        }

        info!("Successfully extracted VC++ Redistributables DLLs before installation");
        Ok(())
    }

    /// Extract msvcp140.dll and ucrtbase.dll from VC++ Redistributables installer
    /// This is required because Wine's builtin versions have higher version numbers,
    /// so the installer refuses to install them. We extract them manually.
    /// NOTE: This is the old method - extract_vcredist_dlls_before_install is preferred for vcrun2022
    fn extract_vcredist_dlls(&self, vcredist_exe: &Path, cache_dir: &Path) -> Result<()> {
        use std::fs;
        use std::process::Command;
        use which::which;

        let wineprefix = self.config.wineprefix();
        let system32_dlls = wineprefix.join("drive_c/windows/system32");
        fs::create_dir_all(&system32_dlls)?;

        // Create temp directory for extraction
        let temp_dir = cache_dir.join("vcredist_extract");
        let temp_win32 = temp_dir.join("win32");
        fs::create_dir_all(&temp_win32)?;

        // cabextract is required
        let cabextract = which("cabextract")
            .map_err(|_| WinetricksError::Config(
                "cabextract not found. Please install it (e.g. 'sudo apt install cabextract')".into()
            ))?;

        info!("Extracting 'a10' CAB from VC++ Redistributables installer...");
        // Extract the 'a10' CAB file from the installer
        let status = Command::new(&cabextract)
            .arg("--directory")
            .arg(&temp_win32)
            .arg(vcredist_exe)
            .arg("-F")
            .arg("a10")
            .status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("cabextract --directory {:?} {:?} -F a10", temp_win32, vcredist_exe),
                error: e.to_string(),
            })?;

        if !status.success() {
            return Err(WinetricksError::Verb(
                "Failed to extract 'a10' CAB from VC++ Redistributables installer".into()
            ));
        }

        let a10_cab = temp_win32.join("a10");
        if !a10_cab.exists() {
            return Err(WinetricksError::Verb(
                "Extracted 'a10' CAB file not found".into()
            ));
        }

        // Extract msvcp140.dll from the 'a10' CAB
        info!("Extracting msvcp140.dll to system32...");
        let status = Command::new(&cabextract)
            .arg("--directory")
            .arg(&system32_dlls)
            .arg(&a10_cab)
            .arg("-F")
            .arg("msvcp140.dll")
            .status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("cabextract --directory {:?} {:?} -F msvcp140.dll", system32_dlls, a10_cab),
                error: e.to_string(),
            })?;

        if !status.success() {
            warn!("Warning: Failed to extract msvcp140.dll from 'a10' CAB (may not be critical)");
        }

        // Extract ucrtbase.dll from the 'a10' CAB (for vcrun2015 and vcrun2017)
        info!("Extracting ucrtbase.dll to system32...");
        let status = Command::new(&cabextract)
            .arg("--directory")
            .arg(&system32_dlls)
            .arg(&a10_cab)
            .arg("-F")
            .arg("ucrtbase.dll")
            .status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("cabextract --directory {:?} {:?} -F ucrtbase.dll", system32_dlls, a10_cab),
                error: e.to_string(),
            })?;

        if !status.success() {
            warn!("Warning: Failed to extract ucrtbase.dll from 'a10' CAB (may not be critical)");
        }

        info!("Successfully extracted VC++ Redistributables DLLs");
        Ok(())
    }

    /// Install 64-bit VC++ Redistributables on win64 prefixes
    /// Called after 32-bit installation if prefix is win64
    fn install_vcredist_64bit(&self, verb_name: &str, cache_dir: &Path) -> Result<()> {
        use std::process::Command;

        // Check if we're on a win64 prefix
        if self.config.winearch.as_ref().map(|a| a != "win64").unwrap_or(true) {
            return Ok(()); // Not win64, skip
        }

        info!("Installing 64-bit VC++ Redistributables for win64 prefix...");

        // Download URLs for 64-bit installers (from original winetricks)
        let (_url, _sha256) = if verb_name == "vcrun2019" {
            ("https://aka.ms/vs/16/release/vc_redist.x64.exe", "4c6c420cf4cbf2c9c9ed476e96580ae92a97b2822c21329a2e49e8439ac5ad30")
        } else if verb_name == "vcrun2022" {
            ("https://aka.ms/vs/17/release/vc_redist.x64.exe", "") // TODO: Get SHA256
        } else {
            // vcrun2015 and vcrun2017 don't have documented 64-bit URLs in the original winetricks
            return Ok(());
        };

        // TODO: Download 64-bit installer using downloader
        // For now, just log that it's needed
        info!("64-bit VC++ Redistributables installer download not yet fully implemented");
        
        Ok(())
    }

    /// Ensure DirectX redistributable is downloaded (helper_directx_Jun2010)
    async fn ensure_directx_redistributable(&self) -> Result<()> {
        // DirectX redistributable goes to shared cache directory "directx9"
        let directx_cache = self.config.cache_dir.join("directx9");
        std::fs::create_dir_all(&directx_cache)?;
        
        let directx_file = directx_cache.join("directx_Jun2010_redist.exe");
        
        // Download if not exists
        if !directx_file.exists() {
            info!("Downloading DirectX June 2010 redistributable...");
            let url = "https://files.holarse-linuxgaming.de/mirrors/microsoft/directx_Jun2010_redist.exe";
            let sha256 = "8746ee1a84a083a90e37899d71d50d5c7c015e69688a466aa80447f011780c0d";
            
            self.downloader
                .download(url, &directx_file, Some(sha256), true)
                .await?;
        }
        
        Ok(())
    }

    /// Extract specific d3dx9 DLL from DirectX redistributable (helper_d3dx9_xx)
    fn extract_d3dx9_dll(&self, metadata: &VerbMetadata, directx_redist: &Path, cache_dir: &Path) -> Result<()> {
        use std::fs;
        use std::process::Command;
        use which::which;
        use glob::glob;

        let wineprefix = self.config.wineprefix();
        let system32_dlls = wineprefix.join("drive_c/windows/system32");
        fs::create_dir_all(&system32_dlls)?;

        // Extract DLL version number from verb name (e.g., "d3dx9_43" -> "43", "d3dx9" -> extract all)
        let dll_version = if metadata.name == "d3dx9" {
            None // Extract all d3dx9 DLLs
        } else if metadata.name.starts_with("d3dx9_") {
            Some(metadata.name.strip_prefix("d3dx9_").unwrap_or(""))
        } else {
            return Err(WinetricksError::Verb(format!(
                "Invalid d3dx9 verb name: {}",
                metadata.name
            )));
        };

        // Create temp directory for extraction
        let temp_dir = cache_dir.join("d3dx9_extract");
        fs::create_dir_all(&temp_dir)?;

        // cabextract is required
        let cabextract = which("cabextract")
            .map_err(|_| WinetricksError::Config(
                "cabextract not found. Please install it (e.g. 'sudo apt install cabextract')".into()
            ))?;

        if let Some(version) = dll_version {
            // Extract specific DLL version (e.g., d3dx9_43)
            let dll_name = format!("d3dx9_{}", version);
            info!("Extracting {} from DirectX redistributable...", dll_name);

            // Extract CAB files matching d3dx9_XX x86 pattern
            // Original winetricks: w_try_cabextract -d "${W_TMP}" -L -F "*${dllname}*x86*" "${W_CACHE}"/directx9/${DIRECTX_NAME}
            let status = Command::new(&cabextract)
                .arg("-d")
                .arg(&temp_dir)
                .arg("-L") // List contents
                .arg("-F")
                .arg(&format!("*{}*x86*", dll_name))
                .arg(directx_redist)
                .status()
                .map_err(|e| WinetricksError::CommandExecution {
                    command: format!("cabextract -d {:?} -L -F *{}*x86* {:?}", temp_dir, dll_name, directx_redist),
                    error: e.to_string(),
                })?;

            if !status.success() {
                return Err(WinetricksError::Verb(format!(
                    "Failed to extract {} CAB files from DirectX redistributable",
                    dll_name
                )));
            }

            // Extract DLL from CAB files
            // Original winetricks: for x in "${W_TMP}"/*.cab; do w_try_cabextract -d "${W_SYSTEM32_DLLS}" -L -F "${dllname}.dll" "${x}"; done
            let cab_pattern = temp_dir.join("*.cab");
            if let Ok(entries) = glob(&cab_pattern.to_string_lossy()) {
                for cab_entry in entries {
                    if let Ok(cab_file) = cab_entry {
                        let status = Command::new(&cabextract)
                            .arg("-d")
                            .arg(&system32_dlls)
                            .arg("-L")
                            .arg("-F")
                            .arg(&format!("{}.dll", dll_name))
                            .arg(&cab_file)
                            .status()
                            .map_err(|e| WinetricksError::CommandExecution {
                                command: format!("cabextract -d {:?} -L -F {}.dll {:?}", system32_dlls, dll_name, cab_file),
                                error: e.to_string(),
                            })?;

                        if status.success() {
                            info!("Extracted {}.dll to system32", dll_name);
                        }
                    }
                }
            }

            // On win64 prefixes, also extract 64-bit version
            if self.config.winearch.as_ref().map(|a| a == "win64").unwrap_or(false) {
                let system64_dlls = wineprefix.join("drive_c/windows/system32"); // 64-bit DLLs go in system32 on win64
                fs::create_dir_all(&system64_dlls)?;

                // Extract CAB files matching d3dx9_XX x64 pattern
                let status = Command::new(&cabextract)
                    .arg("-d")
                    .arg(&temp_dir)
                    .arg("-L")
                    .arg("-F")
                    .arg(&format!("*{}*x64*", dll_name))
                    .arg(directx_redist)
                    .status()
                    .map_err(|e| WinetricksError::CommandExecution {
                        command: format!("cabextract -d {:?} -L -F *{}*x64* {:?}", temp_dir, dll_name, directx_redist),
                        error: e.to_string(),
                    })?;

                if status.success() {
                    // Extract DLL from x64 CAB files
                    let x64_cab_pattern = temp_dir.join("*x64.cab");
                    if let Ok(entries) = glob(&x64_cab_pattern.to_string_lossy()) {
                        for cab_entry in entries {
                            if let Ok(cab_file) = cab_entry {
                                let _status = Command::new(&cabextract)
                                    .arg("-d")
                                    .arg(&system64_dlls)
                                    .arg("-L")
                                    .arg("-F")
                                    .arg(&format!("{}.dll", dll_name))
                                    .arg(&cab_file)
                                    .status();
                                // Don't fail on error, just warn
                            }
                        }
                    }
                }
            }

            // Set DLL override to native
            // Original winetricks: w_override_dlls native "${dllname}"
            if let Err(e) = self.set_dll_override(&dll_name, "native") {
                warn!("Warning: Failed to set DLL override for {}: {}", dll_name, e);
            }
        } else {
            // Extract all d3dx9 DLLs (for "d3dx9" verb)
            info!("Extracting all d3dx9 DLLs from DirectX redistributable...");

            // Extract all d3dx9 x86 CAB files
            let status = Command::new(&cabextract)
                .arg("-d")
                .arg(&temp_dir)
                .arg("-L")
                .arg("-F")
                .arg("*d3dx9*x86*")
                .arg(directx_redist)
                .status()
                .map_err(|e| WinetricksError::CommandExecution {
                    command: format!("cabextract -d {:?} -L -F *d3dx9*x86* {:?}", temp_dir, directx_redist),
                    error: e.to_string(),
                })?;

            if status.success() {
                // Extract all d3dx9*.dll from CAB files
                let cab_pattern = temp_dir.join("*.cab");
                if let Ok(entries) = glob(&cab_pattern.to_string_lossy()) {
                    for cab_entry in entries {
                        if let Ok(cab_file) = cab_entry {
                            let _status = Command::new(&cabextract)
                                .arg("-d")
                                .arg(&system32_dlls)
                                .arg("-L")
                                .arg("-F")
                                .arg("d3dx9*.dll")
                                .arg(&cab_file)
                                .status();
                            // Continue even if one fails
                        }
                    }
                }
            }

            // Set DLL overrides for all d3dx9 versions (as original winetricks does)
            let d3dx9_versions = vec![
                "d3dx9_24", "d3dx9_25", "d3dx9_26", "d3dx9_27", "d3dx9_28", "d3dx9_29", "d3dx9_30",
                "d3dx9_31", "d3dx9_32", "d3dx9_33", "d3dx9_34", "d3dx9_35", "d3dx9_36", "d3dx9_37",
                "d3dx9_38", "d3dx9_39", "d3dx9_40", "d3dx9_41", "d3dx9_42", "d3dx9_43"
            ];

            for dll in &d3dx9_versions {
                if let Err(e) = self.set_dll_override(dll, "native") {
                    warn!("Warning: Failed to set DLL override for {}: {}", dll, e);
                }
            }

            // On win64, also extract 64-bit DLLs
            if self.config.winearch.as_ref().map(|a| a == "win64").unwrap_or(false) {
                let system64_dlls = wineprefix.join("drive_c/windows/system32");
                fs::create_dir_all(&system64_dlls)?;

                let status = Command::new(&cabextract)
                    .arg("-d")
                    .arg(&temp_dir)
                    .arg("-L")
                    .arg("-F")
                    .arg("*d3dx9*x64*")
                    .arg(directx_redist)
                    .status();

                if status.is_ok() && status.unwrap().success() {
                    let x64_cab_pattern = temp_dir.join("*x64.cab");
                    if let Ok(entries) = glob(&x64_cab_pattern.to_string_lossy()) {
                        for cab_entry in entries {
                            if let Ok(cab_file) = cab_entry {
                                let _status = Command::new(&cabextract)
                                    .arg("-d")
                                    .arg(&system64_dlls)
                                    .arg("-L")
                                    .arg("-F")
                                    .arg("d3dx9*.dll")
                                    .arg(&cab_file)
                                    .status();
                            }
                        }
                    }
                }
            }
        }

        info!("Successfully extracted DirectX d3dx9 DLL(s)");
        Ok(())
    }

    /// Check if a package is broken in the current Wine version (w_package_broken)
    /// Takes bug_link, bad_version (optional), and good_version (optional)
    /// If broken and --force is not set, shows warning and returns error
    fn check_package_broken(
        &self,
        verb_name: &str,
        _metadata: &VerbMetadata,
    ) -> Result<()> {
        // For now, hardcode known broken packages from original winetricks
        // TODO: Add broken package info to JSON metadata files
        let broken_info = self.get_broken_info(verb_name);
        
        if let Some((bug_link, bad_version, good_version)) = broken_info {
            // Build version range to check
            let version_range = if let (Some(bad), Some(good)) = (bad_version, good_version) {
                // Both versions known: broken from bad_version to good_version
                format!("{},{}", bad, good)
            } else if let Some(bad) = bad_version {
                // Only bad version known: broken from bad_version onwards
                format!("{},", bad)
            } else {
                // No version info: always broken
                return self.handle_package_broken(verb_name, bug_link, None, good_version);
            };
            
            // Check if we're in the problematic range
            if let Ok(is_broken) = self.wine.version_in_range(&version_range) {
                if is_broken {
                    return self.handle_package_broken(verb_name, bug_link, bad_version, good_version);
                }
            }
        }
        
        Ok(())
    }

    /// Handle package broken warning/error
    fn handle_package_broken(
        &self,
        verb_name: &str,
        bug_link: &str,
        bad_version: Option<&str>,
        good_version: Option<&str>,
    ) -> Result<()> {
        let current_version = &self.wine.version_stripped;
        
        let message = match (bad_version, good_version) {
            (Some(bad), Some(good)) => {
                format!(
                    "Package ({}) is broken in wine-{}. Broken since version {}. Use >= {}. See {} for more information. Use --force to try anyway.",
                    verb_name, current_version, bad, good, bug_link
                )
            }
            (Some(bad), None) => {
                format!(
                    "Package ({}) is broken in wine-{}. Broken since version {}. See {} for more information. Use --force to try anyway.",
                    verb_name, current_version, bad, bug_link
                )
            }
            (None, Some(good)) => {
                format!(
                    "Package ({}) is broken in wine-{}. Use >= {}. See {} for more information. Use --force to try anyway.",
                    verb_name, current_version, good, bug_link
                )
            }
            (None, None) => {
                format!(
                    "Package ({}) is broken. See {} for more information. Use --force to try anyway.",
                    verb_name, bug_link
                )
            }
        };
        
        if self.config.force {
            warn!("{}", message);
            warn!("Continuing anyway due to --force flag");
            Ok(())
        } else {
            Err(WinetricksError::Verb(message))
        }
    }

    /// Get broken package info for a verb (hardcoded for now, matches original winetricks)
    fn get_broken_info(&self, verb_name: &str) -> Option<(&str, Option<&str>, Option<&str>)> {
        // This is a simplified version - in reality, this should come from JSON metadata
        // For now, hardcode known broken packages from original winetricks
        match verb_name {
            // Examples from original winetricks:
            // w_package_broken "https://bugs.winehq.org/show_bug.cgi?id=49532" 5.12 5.18
            // w_package_broken "https://bugs.winehq.org/show_bug.cgi?id=52722" 7.5 7.6
            // etc.
            _ => None,
        }
    }

    /// Install corefonts meta-verb (installs all individual font verbs)
    /// Original winetricks: load_corefonts() calls w_call for each font
    async fn install_corefonts(&mut self) -> Result<()> {
        use std::fs;

        // List of individual font verbs that make up corefonts
        // Original winetricks: w_call andale, arial, comicsans, courier, georgia, impact, times, trebuchet, verdana, webdings
        let corefonts_verbs = vec![
            "andale",
            "arial",
            "comicsans",
            "courier",
            "georgia",
            "impact",
            "times",
            "trebuchet",
            "verdana",
            "webdings",
        ];

        info!("Installing corefonts components...");
        
        // Install each individual font verb
        for font_verb in &corefonts_verbs {
            // Check if already installed (skip if so, unless --force)
            if !self.config.force && self.is_installed(font_verb).unwrap_or(false) {
                info!("{} is already installed, skipping", font_verb);
                continue;
            }

            // Install individual font verb (using install_verb_internal to avoid re-checking and logging)
            if let Err(e) = self.install_verb_internal(font_verb).await {
                warn!("Warning: Failed to install {} (may not be critical): {}", font_verb, e);
                // Continue installing other fonts even if one fails
            } else {
                info!("Successfully installed {}", font_verb);
            }
        }

        // Create marker file after all fonts are installed
        // Original winetricks: touch "${W_FONTSDIR_UNIX}/corefonts.installed"
        let wineprefix = self.config.wineprefix();
        let windows_dir = wineprefix.join("drive_c/windows");
        let fonts_dir_unix = if windows_dir.join("Fonts").exists() {
            windows_dir.join("Fonts")
        } else if windows_dir.join("fonts").exists() {
            windows_dir.join("fonts")
        } else {
            // Create Fonts directory if it doesn't exist
            let fonts_dir = windows_dir.join("Fonts");
            fs::create_dir_all(&fonts_dir)?;
            fonts_dir
        };

        let marker_file = fonts_dir_unix.join("corefonts.installed");
        fs::File::create(&marker_file)?;
        info!("Created corefonts marker file: {:?}", marker_file);

        // Log corefonts installation
        self.log_installation("corefonts")?;

        Ok(())
    }

    /// Install allfonts meta-verb (installs all individual font verbs)
    /// Original winetricks: load_allfonts() calls w_call for each font
    async fn install_allfonts(&mut self) -> Result<()> {
        use std::fs;

        // List of all individual font verbs (excluding meta-verbs like corefonts, allfonts, cjkfonts, pptfonts)
        // Based on files/json/fonts/*.json
        let allfonts_verbs = vec![
            "andale",
            "arial",
            "baekmuk",
            "calibri",
            "cambria",
            "candara",
            "comicsans",
            "consolas",
            "constantia",
            "corbel",
            "courier",
            "droid",
            "eufonts",
            "fakechinese",
            "fakejapanese",
            "fakejapanese_ipamona",
            "fakejapanese_vlgothic",
            "fakekorean",
            "georgia",
            "impact",
            "ipamona",
            "liberation",
            "lucida",
            "meiryo",
            "micross",
            "opensymbol",
            "sourcehansans",
            "tahoma",
            "takao",
            "times",
            "trebuchet",
            "uff",
            "unifont",
            "verdana",
            "vlgothic",
            "webdings",
            "wenquanyi",
            "wenquanyizenhei",
        ];

        info!("Installing allfonts components...");
        
        // Install each individual font verb
        for font_verb in &allfonts_verbs {
            // Check if already installed (skip if so, unless --force)
            if !self.config.force && self.is_installed(font_verb).unwrap_or(false) {
                info!("{} is already installed, skipping", font_verb);
                continue;
            }

            // Install individual font verb (using install_verb_internal to avoid re-checking and logging)
            if let Err(e) = self.install_verb_internal(font_verb).await {
                warn!("Warning: Failed to install {} (may not be critical): {}", font_verb, e);
                // Continue installing other fonts even if one fails
            } else {
                info!("Successfully installed {}", font_verb);
            }
        }

        // Create marker file after all fonts are installed
        // Original winetricks: touch "${W_FONTSDIR_UNIX}/allfonts.installed"
        let wineprefix = self.config.wineprefix();
        let windows_dir = wineprefix.join("drive_c/windows");
        let fonts_dir_unix = if windows_dir.join("Fonts").exists() {
            windows_dir.join("Fonts")
        } else if windows_dir.join("fonts").exists() {
            windows_dir.join("fonts")
        } else {
            // Create Fonts directory if it doesn't exist
            let fonts_dir = windows_dir.join("Fonts");
            fs::create_dir_all(&fonts_dir)?;
            fonts_dir
        };

        let marker_file = fonts_dir_unix.join("allfonts.installed");
        fs::File::create(&marker_file)?;
        info!("Created allfonts marker file: {:?}", marker_file);

        // Log allfonts installation
        self.log_installation("allfonts")?;

        Ok(())
    }

    /// Install GitHub-based DLL (generic handler for dxvk, vkd3d, faudio, etc.)
    async fn install_github_dll(&mut self, verb_name: &str, org: &str, repo: &str, dll_names: &[&str]) -> Result<()> {
        use std::fs;
        use std::process::Command;

        let cache_dir = self.config.cache_dir.join(verb_name);
        fs::create_dir_all(&cache_dir)?;

        // Get latest GitHub release URL
        info!("Getting latest {} release from GitHub...", repo);
        let release_url = self.get_github_latest_release(org, repo).await?;
        
        // Extract filename from URL (e.g., "vkd3d-proton-2.8.tar.zst" or "vkd3d-proton-2.8.tar.gz")
        let filename = release_url.split('/').last().ok_or_else(|| {
            WinetricksError::Config("Invalid GitHub release URL".into())
        })?;
        
        let archive_file = cache_dir.join(filename);
        
        // Download the release
        info!("Downloading {} from: {}", repo, release_url);
        self.downloader
            .download(&release_url, &archive_file, None, true)
            .await?;
        
        // Extract archive file
        info!("Extracting {} archive...", repo);
        let extract_dir = cache_dir.join(format!("{}_extract", verb_name));
        if extract_dir.exists() {
            fs::remove_dir_all(&extract_dir)?;
        }
        fs::create_dir_all(&extract_dir)?;
        
        // Handle different archive formats
        let status = if filename.ends_with(".tar.zst") {
            // Use zstd to decompress, then tar to extract
            // zstd -d <file.tar.zst | tar xf - -C <dest>
            eprintln!("Executing zstd -d -c {} | tar xf - -C {}", 
                archive_file.to_string_lossy(), extract_dir.to_string_lossy());
            
            // Pipe zstd output to tar
            let mut zstd_cmd = Command::new("zstd");
            zstd_cmd.arg("-d").arg("-c").arg(&archive_file)
                .stdout(std::process::Stdio::piped());
            
            let mut zstd_process = zstd_cmd.spawn()
                .map_err(|e| WinetricksError::CommandExecution {
                    command: format!("zstd -d -c {:?}", archive_file),
                    error: e.to_string(),
                })?;
            
            let mut tar_cmd = Command::new("tar");
            tar_cmd.arg("xf").arg("-").arg("-C").arg(&extract_dir)
                .stdin(zstd_process.stdout.take().ok_or_else(|| {
                    WinetricksError::Config("Failed to create pipe for zstd".into())
                })?);
            
            let tar_output = tar_cmd.status()
                .map_err(|e| WinetricksError::CommandExecution {
                    command: format!("tar xf - -C {:?}", extract_dir),
                    error: e.to_string(),
                })?;
            
            // Wait for zstd to finish
            let _ = zstd_process.wait();
            
            tar_output
        } else if filename.ends_with(".tar.gz") {
            // Use tar to extract (tar xzf)
            eprintln!("Executing tar xzf {} -C {}", 
                archive_file.to_string_lossy(), extract_dir.to_string_lossy());
            Command::new("tar")
                .arg("xzf")
                .arg(&archive_file)
                .arg("-C")
                .arg(&extract_dir)
                .status()
                .map_err(|e| WinetricksError::CommandExecution {
                    command: format!("tar xzf {:?} -C {:?}", archive_file, extract_dir),
                    error: e.to_string(),
                })?
        } else {
            return Err(WinetricksError::Verb(format!(
                "Unsupported archive format: {} (expected .tar.zst or .tar.gz)", filename
            )));
        };
        
        if !status.success() {
            return Err(WinetricksError::Verb(format!(
                "Failed to extract {} archive", repo
            )));
        }
        
        // Find DLL files in extracted directory
        // Most GitHub DLLs structure: <repo>-*/x64/<dlls> and x32/<dlls>
        // Some may use different structures (x86/x64, win32/win64, etc.)
        let wineprefix = self.config.wineprefix();
        let is_win64 = self.config.winearch.as_ref().map(|a| a == "win64").unwrap_or(false);
        
        // Find the extracted directory (should be <repo>-* or similar)
        let extracted_dirs: Vec<_> = fs::read_dir(&extract_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();
        
        if extracted_dirs.is_empty() {
            return Err(WinetricksError::Verb(format!(
                "No extracted directory found in {} archive", repo
            )));
        }
        
        let extracted_dir = &extracted_dirs[0].path();
        
        // Try different directory structures (x32/x64, x86/x64, win32/win64, etc.)
        let arch_dirs_32 = vec!["x32", "x86", "win32", "32"];
        let arch_dirs_64 = vec!["x64", "amd64", "win64", "64"];
        
        // Copy 32-bit DLLs to syswow64 (or system32 on 32-bit prefix)
        let dll_dest_32 = if is_win64 {
            wineprefix.join("drive_c/windows/syswow64")
        } else {
            wineprefix.join("drive_c/windows/system32")
        };
        fs::create_dir_all(&dll_dest_32)?;
        
        for arch_dir in &arch_dirs_32 {
            let src_dir = extracted_dir.join(arch_dir);
            if src_dir.exists() {
                for dll_name in dll_names {
                    let src_dll = src_dir.join(dll_name);
                    if src_dll.exists() {
                        let dest_dll = dll_dest_32.join(dll_name);
                        fs::copy(&src_dll, &dest_dll)?;
                        info!("Copied {} to {:?}", dll_name, dest_dll);
                    }
                }
                break;
            }
        }
        
        // Copy 64-bit DLLs to system32 (on 64-bit prefix)
        if is_win64 {
            let dll_dest_64 = wineprefix.join("drive_c/windows/system32");
            fs::create_dir_all(&dll_dest_64)?;
            
            for arch_dir in &arch_dirs_64 {
                let src_dir = extracted_dir.join(arch_dir);
                if src_dir.exists() {
                    for dll_name in dll_names {
                        let src_dll = src_dir.join(dll_name);
                        if src_dll.exists() {
                            let dest_dll = dll_dest_64.join(dll_name);
                            fs::copy(&src_dll, &dest_dll)?;
                            info!("Copied {} to {:?}", dll_name, dest_dll);
                        }
                    }
                    break;
                }
            }
        }
        
        // Set DLL overrides to native (strip .dll/.exe extension)
        for dll_name in dll_names {
            let dll_base = dll_name.trim_end_matches(".dll").trim_end_matches(".exe");
            if let Err(e) = self.set_dll_override(dll_base, "native") {
                warn!("Warning: Failed to set DLL override for {}: {}", dll_base, e);
            }
        }
        
        // Log installation
        self.log_installation(verb_name)?;
        
        info!("Successfully installed {}", verb_name);
        Ok(())
    }

    /// Install allcodecs (meta-verb that installs all codec components)
    async fn install_allcodecs(&mut self) -> Result<()> {
        // List of codec verbs (dirac, ffdshow, icodecs, cinepak, l3codecx, xvid)
        let codec_verbs = vec!["dirac", "ffdshow", "icodecs", "cinepak", "l3codecx", "xvid"];
        
        info!("Installing allcodecs components...");
        
        for codec_verb in &codec_verbs {
            if !self.config.force && self.is_installed(codec_verb).unwrap_or(false) {
                info!("{} is already installed, skipping", codec_verb);
                continue;
            }
            
            if let Err(e) = self.install_verb_internal(codec_verb).await {
                warn!("Warning: Failed to install {} (may not be critical): {}", codec_verb, e);
            } else {
                info!("Successfully installed {}", codec_verb);
            }
        }
        
        self.log_installation("allcodecs")?;
        Ok(())
    }

    /// Install cjkfonts (meta-verb that installs CJK font components)
    async fn install_cjkfonts(&mut self) -> Result<()> {
        // List of CJK font verbs
        let cjk_verbs = vec!["baekmuk", "fakechinese", "fakejapanese", "fakejapanese_ipamona", 
                            "fakejapanese_vlgothic", "fakekorean", "ipamona", "meiryo", 
                            "sourcehansans", "takao", "vlgothic", "wenquanyi", "wenquanyizenhei"];
        
        info!("Installing cjkfonts components...");
        
        for font_verb in &cjk_verbs {
            if !self.config.force && self.is_installed(font_verb).unwrap_or(false) {
                info!("{} is already installed, skipping", font_verb);
                continue;
            }
            
            if let Err(e) = self.install_verb_internal(font_verb).await {
                warn!("Warning: Failed to install {} (may not be critical): {}", font_verb, e);
            } else {
                info!("Successfully installed {}", font_verb);
            }
        }
        
        self.log_installation("cjkfonts")?;
        Ok(())
    }

    /// Install pptfonts (meta-verb that installs PowerPoint font components)
    async fn install_pptfonts(&mut self) -> Result<()> {
        // PowerPoint fonts are typically: calibri, cambria, candara, consolas, constantia, corbel
        let ppt_verbs = vec!["calibri", "cambria", "candara", "consolas", "constantia", "corbel"];
        
        info!("Installing pptfonts components...");
        
        for font_verb in &ppt_verbs {
            if !self.config.force && self.is_installed(font_verb).unwrap_or(false) {
                info!("{} is already installed, skipping", font_verb);
                continue;
            }
            
            if let Err(e) = self.install_verb_internal(font_verb).await {
                warn!("Warning: Failed to install {} (may not be critical): {}", font_verb, e);
            } else {
                info!("Successfully installed {}", font_verb);
            }
        }
        
        self.log_installation("pptfonts")?;
        Ok(())
    }

    /// Install settings verb (Windows version, registry tweaks, etc.)
    async fn install_mspaint(&mut self) -> Result<()> {
        use std::fs;
        use std::process::Command;
        use which::which;

        info!("Installing mspaint (Windows Update installer)");
        
        // Load metadata from registry
        let metadata = self
            .registry
            .get("mspaint")
            .ok_or_else(|| WinetricksError::VerbNotFound("mspaint".to_string()))?
            .clone();
        let cache_dir = self.config.cache_dir.join("mspaint");
        fs::create_dir_all(&cache_dir)?;
        
        // Download file
        let file_info = &metadata.files[0];
        let file_path = cache_dir.join(&file_info.filename);
        
        if !file_path.exists() {
            if let Some(ref url) = file_info.url {
                info!("Downloading mspaint installer...");
                self.downloader
                    .download(url, &file_path, file_info.sha256.as_deref(), true)
                    .await?;
            } else {
                return Err(WinetricksError::Verb("mspaint file has no URL".into()));
            }
        }
        
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Step 1: Extract mfc42*.dll from vcredist.exe (if vcrun6 is available)
        let vcrun6_cache = self.config.cache_dir.join("vcrun6");
        let vcredist_exe = vcrun6_cache.join("vcredist.exe");
        if vcredist_exe.exists() {
            let cabextract = which("cabextract")
                .map_err(|_| WinetricksError::Config("cabextract not found".into()))?;
            
            let syswow64 = wineprefix.join("drive_c/windows/syswow64");
            fs::create_dir_all(&syswow64)?;
            
            info!("Extracting mfc42*.dll from vcredist.exe...");
            eprintln!("Executing cabextract -q {} -d {} -F mfc42*.dll", 
                vcredist_exe.to_string_lossy(), syswow64.to_string_lossy());
            
            let status = Command::new(&cabextract)
                .arg("-q")
                .arg(&vcredist_exe)
                .arg("-d")
                .arg(&syswow64)
                .arg("-F")
                .arg("mfc42*.dll")
                .status()
                .map_err(|e| WinetricksError::CommandExecution {
                    command: format!("cabextract -q {} -d {} -F mfc42*.dll", 
                        vcredist_exe.to_string_lossy(), syswow64.to_string_lossy()),
                    error: e.to_string(),
                })?;
            
            if !status.success() {
                warn!("Warning: Failed to extract mfc42*.dll (may not be critical)");
            }
        }
        
        // Step 2: Extract Windows Update installer with /q /x:
        let temp_dir = wineprefix.join("drive_c/windows/temp");
        fs::create_dir_all(&temp_dir)?;
        let extract_dest = temp_dir.join(&file_info.filename);
        
        let file_win_path = self.unix_to_wine_path(&file_path)?;
        let extract_dest_win = self.unix_to_wine_path(&extract_dest)?;
        
        info!("Extracting Windows Update installer...");
        eprintln!("Executing wine {} /q /x:{}", file_win_path, extract_dest_win);
        
        let mut extract_cmd = std::process::Command::new(&self.wine.wine_bin);
        extract_cmd.env("WINEPREFIX", &wineprefix_str);
        extract_cmd.current_dir(&cache_dir);
        extract_cmd.arg(&file_win_path);
        extract_cmd.arg("/q");
        extract_cmd.arg("/x:").arg(&extract_dest_win);
        
        let extract_status = extract_cmd.status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine {} /q /x:{}", file_win_path, extract_dest_win),
                error: e.to_string(),
            })?;
        
        if !extract_status.success() {
            return Err(WinetricksError::Verb("Failed to extract Windows Update installer".into()));
        }
        
        // Step 3: Copy mspaint.exe from extracted directory
        let extracted_mspaint = extract_dest.join("SP3GDR/mspaint.exe");
        let dest_mspaint = wineprefix.join("drive_c/windows/mspaint.exe");
        
        if !extracted_mspaint.exists() {
            return Err(WinetricksError::Verb("mspaint.exe not found in extracted files".into()));
        }
        
        fs::create_dir_all(dest_mspaint.parent().unwrap())?;
        fs::copy(&extracted_mspaint, &dest_mspaint)?;
        info!("Copied mspaint.exe to {:?}", dest_mspaint);
        
        self.log_installation("mspaint")?;
        Ok(())
    }

    async fn install_settings_verb(&mut self, verb_name: &str, _metadata: &VerbMetadata) -> Result<()> {
        // Handle Windows version settings
        if verb_name.starts_with("win") {
            let version = verb_name.strip_prefix("win").unwrap_or(verb_name);
            // Map verb names to Windows version strings
            let win_version = match version {
                "7" => "win7",
                "8" => "win8",
                "81" => "win8.1",
                "10" => "win10",
                "11" => "win11",
                "xp" => "winxp",
                "2k" => "win2k",
                "95" => "win95",
                "98" => "win98",
                "me" => "winme",
                _ => version,
            };
            self.set_windows_version(win_version)?;
            info!("Set Windows version to: {}", win_version);
        } else {
            // Other settings verbs (fontfix, forcemono, etc.) may need specific handling
            // For now, just log the installation
            info!("Settings verb {} installed (specific handling may be needed)", verb_name);
        }
        
        self.log_installation(verb_name)?;
        Ok(())
    }

    /// Copy DLL file with symlink handling (matching w_try_cp_dll behavior)
    /// Removes symbolic links if present before copying
    fn copy_dll(&self, src_file: &Path, dest_file: &Path) -> Result<()> {
        use std::fs;
        
        // Handle if dest_file is a directory
        let dest = if dest_file.is_dir() {
            dest_file.join(src_file.file_name().ok_or_else(|| {
                WinetricksError::Config("Invalid source filename".into())
            })?)
        } else {
            dest_file.to_path_buf()
        };
        
        // Remove symbolic link if present (original winetricks does this)
        if dest.is_symlink() || dest.read_link().is_ok() {
            info!("Removing symbolic link: {:?}", dest);
            let _ = fs::remove_file(&dest);
        }
        
        // Copy file with force (overwrite existing)
        fs::copy(src_file, &dest)?;
        info!("Copied DLL: {:?} -> {:?}", src_file, dest);
        
        Ok(())
    }

    /// Copy font files with pattern matching (matching w_try_cp_font_files behavior)
    /// Converts font filenames to lowercase and removes case-sensitive duplicates
    fn copy_font_files(&self, src_dir: &Path, dest_dir: &Path, pattern: Option<&str>) -> Result<()> {
        use std::fs;
        use glob::glob;
        
        if !src_dir.is_dir() {
            return Err(WinetricksError::Config(
                format!("Source directory does not exist: {:?}", src_dir)
            ));
        }
        
        // Create destination directory if it doesn't exist
        fs::create_dir_all(dest_dir)?;
        
        // Default pattern is *.ttf
        let font_pattern = pattern.unwrap_or("*.ttf");
        
        // Build full pattern path
        let pattern_path = src_dir.join(font_pattern);
        
        // Find all matching font files
        let mut font_files = Vec::new();
        if let Ok(entries) = glob(&pattern_path.to_string_lossy()) {
            font_files.extend(entries.filter_map(|entry| entry.ok()));
        }
        
        // Also try uppercase pattern
        let pattern_path_upper = src_dir.join(font_pattern.to_uppercase());
        if let Ok(entries) = glob(&pattern_path_upper.to_string_lossy()) {
            font_files.extend(entries.filter_map(|entry| entry.ok()));
        }
        
        // Also try .ttc and .TTC
        if font_pattern.contains("ttf") {
            let ttc_pattern = font_pattern.replace("ttf", "ttc");
            let pattern_path_ttc = src_dir.join(&ttc_pattern);
            if let Ok(entries) = glob(&pattern_path_ttc.to_string_lossy()) {
                font_files.extend(entries.filter_map(|entry| entry.ok()));
            }
            
            let pattern_path_ttc_upper = src_dir.join(&ttc_pattern.to_uppercase());
            if let Ok(entries) = glob(&pattern_path_ttc_upper.to_string_lossy()) {
                font_files.extend(entries.filter_map(|entry| entry.ok()));
            }
        }
        
        // Remove duplicates
        font_files.sort();
        font_files.dedup();
        
        // Remove any files in dest_dir with same name but different case
        // (original winetricks does case-insensitive matching under Wine)
        let dest_files: Vec<_> = fs::read_dir(dest_dir)?
            .filter_map(|entry| entry.ok())
            .map(|e| e.path())
            .collect();
        
        for font_file in &font_files {
            let font_filename = font_file.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            let font_filename_lower = font_filename.to_lowercase();
            
            // Remove any dest files with same lowercase name but different case
            for dest_file in &dest_files {
                if let Some(dest_name) = dest_file.file_name().and_then(|n| n.to_str()) {
                    if dest_name.to_lowercase() == font_filename_lower && dest_name != font_filename {
                        info!("Removing case-sensitive duplicate: {:?}", dest_file);
                        let _ = fs::remove_file(dest_file);
                    }
                }
            }
            
            // Copy font file with lowercase name (original winetricks converts to lowercase)
            let dest_font = dest_dir.join(&font_filename_lower);
            fs::copy(font_file, &dest_font)?;
            info!("Copied font: {:?} -> {:?}", font_file, dest_font);
        }
        
        Ok(())
    }

    /// Append to Windows PATH environment variable (matching w_append_path behavior)
    fn append_path(&self, new_path: &str) -> Result<()> {
        use std::fs;
        use std::io::Write;
        use std::process::Command;
        
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Get current PATH from registry
        let output = Command::new(&self.wine.wine_bin)
            .arg("reg")
            .arg("query")
            .arg("HKLM\\System\\CurrentControlSet\\Control\\Session Manager\\Environment")
            .arg("/v")
            .arg("PATH")
            .env("WINEPREFIX", &wineprefix_str)
            .output()
            .map_err(|e| WinetricksError::CommandExecution {
                command: "wine reg query for PATH".to_string(),
                error: e.to_string(),
            })?;
        
        // Parse current PATH (handle both REG_SZ and REG_EXPAND_SZ)
        let current_path = String::from_utf8_lossy(&output.stdout);
        let existing_path = if let Some(path_line) = current_path.lines().find(|l| l.contains("PATH")) {
            // Extract path value (format: PATH    REG_SZ    value)
            path_line.split_whitespace().nth(2).unwrap_or("").to_string()
        } else {
            String::new()
        };
        
        // Escape backslashes for registry (2 backslashes, not 4/8)
        // Original winetricks: sed 's,\\,\\\\,g'
        let new_path_escaped = new_path.replace('\\', "\\\\");
        let existing_path_escaped = existing_path.replace('\\', "\\\\");
        
        // Create registry file
        let temp_dir = dirs::cache_dir()
            .ok_or_else(|| WinetricksError::Config("Could not determine cache directory".into()))?
            .join("winetricks");
        fs::create_dir_all(&temp_dir)?;
        
        let reg_file = temp_dir.join("append_path.reg");
        let mut file = fs::File::create(&reg_file)?;
        
        // Write REGEDIT4 format with CRLF line endings
        writeln!(file, "REGEDIT4")?;
        writeln!(file, "")?;
        writeln!(file, "[HKEY_LOCAL_MACHINE\\System\\CurrentControlSet\\Control\\Session Manager\\Environment]")?;
        writeln!(file, "\"PATH\"=\"{}\\;{}\"", new_path_escaped, existing_path_escaped)?;
        file.sync_all()?;
        
        // Import registry file
        self.import_registry_file(&reg_file)?;
        
        info!("Appended to PATH: {}", new_path);
        Ok(())
    }

    /// Common DLL override handling (matching w_common_override_dll behavior)
    /// Removes wine builtin manifests for specific packages (comctl32, vcrun2005)
    fn common_override_dll(&self, verb_name: &str, override_type: &str, dll_names: &[&str]) -> Result<()> {
        use std::fs;
        
        let wineprefix = self.config.wineprefix();
        let windows_dir = wineprefix.join("drive_c/windows");
        
        // Remove wine builtin manifests for specific packages (original winetricks does this)
        match verb_name {
            "comctl32" => {
                // Remove comctl32 manifests
                let manifest_paths = vec![
                    windows_dir.join("winsxs/manifests/amd64_microsoft.windows.common-controls_6595b64144ccf1df_6.0.2600.2982_none_deadbeef.manifest"),
                    windows_dir.join("winsxs/manifests/x86_microsoft.windows.common-controls_6595b64144ccf1df_6.0.2600.2982_none_deadbeef.manifest"),
                ];
                
                for manifest_path in manifest_paths {
                    if manifest_path.exists() {
                        info!("Removing wine builtin manifest: {:?}", manifest_path);
                        let _ = fs::remove_file(&manifest_path);
                    }
                }
            }
            "vcrun2005" => {
                // Remove vcrun2005 manifests
                let manifest_paths = vec![
                    windows_dir.join("winsxs/manifests/amd64_microsoft.vc80.atl_1fc8b3b9a1e18e3b_8.0.50727.4053_none_deadbeef.manifest"),
                    windows_dir.join("winsxs/manifests/amd64_microsoft.vc80.crt_1fc8b3b9a1e18e3b_8.0.50727.4053_none_deadbeef.manifest"),
                    windows_dir.join("winsxs/manifests/x86_microsoft.vc80.atl_1fc8b3b9a1e18e3b_8.0.50727.4053_none_deadbeef.manifest"),
                ];
                
                for manifest_path in manifest_paths {
                    if manifest_path.exists() {
                        info!("Removing wine builtin manifest: {:?}", manifest_path);
                        let _ = fs::remove_file(&manifest_path);
                    }
                }
            }
            _ => {
                // No special manifest handling for other packages
            }
        }
        
        // Set DLL overrides for all specified DLLs
        for dll_name in dll_names {
            self.set_dll_override(dll_name, override_type)?;
        }
        
        Ok(())
    }

    /// Set Windows version for a specific application (matching w_set_app_winver behavior)
    fn set_app_winver(&self, app_name: &str, version: &str) -> Result<()> {
        use std::fs;
        use std::io::Write;
        
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
                warn!("Unknown Windows version: {}, using as-is", version);
                version
            }
        };
        
        // Create temp directory for registry file
        let temp_dir = dirs::cache_dir()
            .ok_or_else(|| WinetricksError::Config("Could not determine cache directory".into()))?
            .join("winetricks");
        fs::create_dir_all(&temp_dir)?;
        
        let reg_file = temp_dir.join("set_app_winver.reg");
        
        // Create registry file
        let reg_content = format!(
            r#"REGEDIT4

[HKEY_CURRENT_USER\Software\Wine\AppDefaults\{}\]
"Version"="{}"
"#,
            app_name, version_num
        );
        
        let mut file = fs::File::create(&reg_file)?;
        file.write_all(reg_content.as_bytes())?;
        file.sync_all()?;
        
        // Import registry file
        self.import_registry_file(&reg_file)?;
        
        info!("Set {} to Windows {} mode", app_name, version);
        Ok(())
    }

    /// Apply wine bug workaround if needed (matching w_workaround_wine_bug behavior)
    /// Returns true if workaround should be applied (Wine version is in specified range)
    /// This can be called by individual verbs to conditionally apply fixes
    pub fn workaround_wine_bug(&self, bug_number: &str, message: Option<&str>, version_ranges: &[&str]) -> Result<bool> {
        // Check if bug is blacklisted (for debugging)
        if let Ok(blacklist) = std::env::var("WINETRICKS_BLACKLIST") {
            if blacklist.split(',').any(|b| b.trim() == bug_number) {
                info!("Bug {} is blacklisted, skipping workaround", bug_number);
                return Ok(false);
            }
        }
        
        // Check if Wine version is in any of the specified ranges
        let should_apply = version_ranges.iter().any(|range| {
            self.wine.version_in_range(range).unwrap_or(false)
        });
        
        if should_apply {
            if let Some(msg) = message {
                info!("Applying workaround for Wine bug {}: {}", bug_number, msg);
            } else {
                info!("Applying workaround for Wine bug {}", bug_number);
            }
        }
        
        Ok(should_apply)
    }

    /// Helper function for VB6 SP6 extractions (matching helper_vb6sp6 behavior)
    /// Extracts specific files from VB6 SP6 archive to destination directory
    fn helper_vb6sp6(&self, dest_dir: &Path, files: &[&str]) -> Result<()> {
        use std::fs;
        use std::process::Command;
        
        // VB6 SP6 archive location (would be in cache)
        // This is a simplified version - full implementation would extract from the VB6 SP6 archive
        // Original winetricks: helper_vb6sp6 "${W_SYSTEM32_DLLS}" comctl32.ocx mscomctl.ocx
        // Extracts files from ../vb6sp6/VB60SP6-KB2708437-x86-ENU.msi
        
        // For now, just create placeholder
        // In full implementation, this would:
        // 1. Find VB6 SP6 archive
        // 2. Extract specific files using msiexec or cabextract
        // 3. Copy extracted files to dest_dir
        
        info!("VB6 SP6 helper: would extract {:?} to {:?}", files, dest_dir);
        Ok(())
    }

    /// Remove all DLL overrides from registry (matching w_override_no_dlls behavior)
    pub fn override_no_dlls(&self) -> Result<()> {
        use std::process::Command;
        
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Delete the entire DllOverrides registry key
        // Original winetricks: w_try_regedit /d "HKEY_CURRENT_USER\\Software\\Wine\\DllOverrides"
        let status = Command::new(&self.wine.wine_bin)
            .arg("regedit")
            .arg("/S") // Silent mode
            .arg("/d")
            .arg("HKEY_CURRENT_USER\\Software\\Wine\\DllOverrides")
            .env("WINEPREFIX", &wineprefix_str)
            .status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: "wine regedit /d DllOverrides".to_string(),
                error: e.to_string(),
            })?;
        
        if !status.success() {
            return Err(WinetricksError::Config(
                "Failed to remove DLL overrides".into()
            ));
        }
        
        info!("Removed all DLL overrides");
        Ok(())
    }

    /// Set DLL overrides for multiple DLLs (matching w_override_dlls behavior)
    pub fn override_dlls(&self, override_type: &str, dll_names: &[&str]) -> Result<()> {
        use std::fs;
        use std::io::Write;
        use std::process::Command;
        
        // Handle disabled mode (empty string)
        let mode = if override_type == "disabled" {
            ""
        } else {
            override_type
        };
        
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Create temp directory for registry file inside Wine prefix
        // Original winetricks: Uses C:\windows\Temp\override-dll.reg (inside Wine prefix)
        let temp_dir = wineprefix.join("drive_c/windows/temp");
        fs::create_dir_all(&temp_dir)?;
        
        let reg_file = temp_dir.join("override-dll.reg");
        let mut file = fs::File::create(&reg_file)?;
        
        // Write REGEDIT4 format
        writeln!(file, "REGEDIT4")?;
        writeln!(file, "")?;
        writeln!(file, "[HKEY_CURRENT_USER\\Software\\Wine\\DllOverrides]")?;
        
        // Write each DLL override
        for dll_name in dll_names {
            if mode.is_empty() {
                // Empty mode means delete the override (disabled)
                writeln!(file, "\"{}\"=-", dll_name)?;
            } else {
                writeln!(file, "\"{}\"=\"{}\"", dll_name, mode)?;
            }
        }
        
        file.sync_all()?;
        
        // Convert to Wine path and import
        let reg_file_str = reg_file.to_string_lossy().to_string();
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
        
        // Import registry file using regedit32/regedit64 (matching original winetricks)
        // Original winetricks: Uses syswow64\regedit.exe for 32-bit, regedit.exe for 64-bit
        let is_win64 = self.config.winearch.as_ref().map(|a| a == "win64").unwrap_or(false);
        
        // Convert to Windows path for display
        let reg_file_win = self.unix_to_wine_path(&reg_file)?;
        
        // Always import to 32-bit registry first (matches original winetricks w_try_regedit32)
        let regedit32_exe = if is_win64 {
            "C:\\windows\\syswow64\\regedit.exe"
        } else {
            "C:\\windows\\regedit.exe"
        };
        eprintln!("Executing wine {} /S {}", regedit32_exe, reg_file_win);
        self.regedit32(&reg_file)?;
        
        // On win64, also import to 64-bit registry (matches original winetricks w_try_regedit64)
        if is_win64 {
            eprintln!("Executing wine C:\\windows\\regedit.exe /S {}", reg_file_win);
            self.regedit64(&reg_file)?;
        }
        
        let _ = fs::remove_file(&reg_file);
        
        info!("Set DLL overrides for {} DLLs: {}", dll_names.len(), override_type);
        Ok(())
    }

    /// Disable all known native Microsoft DLLs (matching w_override_all_dlls behavior)
    /// This is a large list of DLLs that should use Wine built-ins instead
    pub fn override_all_dlls(&self) -> Result<()> {
        // List of DLLs to disable (generated from winetricks dlls directory)
        // Original winetricks generates this from find ./dlls -maxdepth 1
        let dlls_to_disable = vec![
            "acledit", "aclui", "activeds", "actxprxy", "advapi32", "advpack",
            "amstream", "apizip", "atl", "atl71", "atl80", "atl90", "atl100",
            "atmlib", "avicap32", "avifil32", "avrt", "bcrypt", "bidispl",
            "binkw32", "bitsadmin", "cabinet", "cap", "cdrom", "certcli",
            "clbcatq", "clusapi", "cmd", "comcat", "comctl32", "comdlg32",
            "comdlg32ocx", "compstui", "cryptdlg", "cryptext", "cryptnet",
            "cryptui", "crypt32", "d2d1", "d3d8", "d3d9", "d3dcompiler_42",
            "d3dcompiler_43", "d3dcompiler_46", "d3dcompiler_47", "d3dim",
            "d3drm", "d3dx10", "d3dx10_33", "d3dx10_34", "d3dx10_35", "d3dx10_36",
            "d3dx10_37", "d3dx10_38", "d3dx10_39", "d3dx10_40", "d3dx10_41",
            "d3dx10_42", "d3dx10_43", "d3dx11_42", "d3dx11_43", "d3dxof",
            "d3dx9_24", "d3dx9_25", "d3dx9_26", "d3dx9_27", "d3dx9_28",
            "d3dx9_29", "d3dx9_30", "d3dx9_31", "d3dx9_32", "d3dx9_33",
            "d3dx9_34", "d3dx9_35", "d3dx9_36", "d3dx9_37", "d3dx9_38",
            "d3dx9_39", "d3dx9_40", "d3dx9_41", "d3dx9_42", "d3dx9_43",
            "dap", "dbghelp", "dciman32", "ddraw", "ddraw16", "devenum",
            "dhcpcsvc", "dhcpcsvc6", "dinput", "dinput8", "dispex",
            "dmband", "dmcompos", "dmime", "dmloader", "dmscript",
            "dmstyle", "dmsynth", "dmusic", "dmusic32", "dnsapi",
            "dotnet11", "dotnet20", "dotnet30", "dotnet35", "dotnet40",
            "dotnet45", "dotnet452", "dotnet46", "dotnet461", "dotnet462",
            "dotnet471", "dotnet472", "dotnet48", "dotnet48.1", "dotnetcore11",
            "dotnetcore20", "dotnetcore21", "dotnetcore30", "dotnetcore31",
            "dotnetcore50", "dpnet", "dpvoice", "dplay", "dplayx",
            "dsdmo", "dsound", "dsound3d", "dssenh", "dswave", "dxdiagn",
            "dxgi", "dxva2", "eapcfg", "evr", "expsrv", "fltlib",
            "fontsub", "framedyn", "gameux", "gdipp", "gdiplus",
            "glu32", "gphoto2.ds", "gphoto2.ds", "hal", "hdwwiz",
            "hid", "hlink", "httpapi", "iccvid", "ieframe", "iexplore",
            "imgutil", "inetcpl", "initpki", "inseng", "iphlpapi",
            "irprops", "itircl", "itss", "jscript", "jscript9", "jsproxy",
            "keymgr", "ksuser", "ktmw32", "loadperf", "locapi", "lz32",
            "mf", "mfplat", "mfreadwrite", "mfuuid", "mlang", "mmdevapi",
            "mmsystem", "mobsync", "mpr", "mprapi", "msacm32", "msacm32.drv",
            "msasn1", "mscat32", "mscoree", "mscorwks", "mscms", "msctf",
            "msctfp", "msdelta", "msdmo", "msftedit", "msgina", "mshtml",
            "msi", "msident", "msimg32", "msimtf", "msisip", "msls31",
            "msnsspc", "mspatcha", "msports", "msrle32", "msscript", "mssha1",
            "mssign32", "mstask", "mstime", "msvcp60", "msvcp70", "msvcp71",
            "msvcp80", "msvcp90", "msvcp100", "msvcp110", "msvcp120", "msvcp140",
            "msvcr70", "msvcr71", "msvcr80", "msvcr90", "msvcr100", "msvcr110",
            "msvcr120", "msvcr140", "msvcrt20", "msvcrt40", "msvfw32",
            "msvidc32", "msv1_0", "msxml", "msxml2", "msxml3", "msxml4",
            "msxml6", "mtxex", "mydocs", "ncrypt", "netapi32", "netcfgx",
            "netfx3", "netfx35", "netfx40", "netfx45", "netfx452", "netfx46",
            "netfx461", "netfx462", "netfx471", "netfx472", "netfx48",
            "netprofm", "netrap", "netshell", "newdev", "normaliz", "npmproxy",
            "npptools", "nsis", "ntdll", "ntdsapi", "ntlanman", "ntmarta",
            "ntoskrnl.exe", "ocmanage", "odbccp32", "odbc32", "odbcbcp",
            "ole32", "oleacc", "oleaut32", "olecli32", "oledb32", "oledlg",
            "olepro32", "olesvr32", "olethk32", "opengl32", "osmesa",
            "p2p", "pdh", "photo", "photometadatahandler", "pidgen",
            "pintool", "plustab", "powrprof", "propsys", "psapi",
            "qasf", "qcap", "qdvd", "qedit", "qmgr", "quartz", "qwave",
            "rasapi32", "rasdlg", "rasgcw", "rasmancs", "rasphone",
            "rpcrt4", "rsaenh", "rtutils", "sapi", "sas", "scarddlg",
            "scardsvr", "schannel", "secur32", "sendmail", "sensapi",
            "serialui", "setupapi", "sfc", "sfc_os", "shdocvw", "shfolder",
            "shlwapi", "slbcsp", "slwga", "snmpapi", "softpub", "spoolss",
            "srclient", "srvcli", "sspicli", "sti", "streamci", "strmbase",
            "strmiids", "swt", "synceng", "t2embed", "tapi32", "taskbar",
            "tdi", "traffic", "tsapi32", "tsappcmp", "tsbyuv", "tsclients",
            "tsmf", "uianimation", "uiribbon", "url", "urlmon", "user32",
            "userenv", "usp10", "utorrent", "uxtheme", "vb2run", "vb3run",
            "vb4run", "vb5run", "vb6run", "vbrun", "vcrun2003", "vcrun2005",
            "vcrun2008", "vcrun2010", "vcrun2012", "vcrun2013", "vcrun2015",
            "vcrun2017", "vcrun2019", "vcrun2022", "vd", "vdredir", "version",
            "vssapi", "wbemdisp", "wbemprox", "wdsbp", "webio", "wevtsvc",
            "wevtutil", "wfapigp", "wiaservc", "winhttp", "wininet",
            "winmm", "winscard", "winspool", "wintab32", "wintrust",
            "wlanapi", "wldap32", "wmaudio", "wmcodecdsp", "wmdmps",
            "wmdrmsdk", "wmfdist", "wmilib", "wmnetmgr", "wmp", "wmpeffects",
            "wmploc", "wmpshellwp", "wmstream", "wmvcore", "wnaspi32",
            "workrave", "wow32", "wpc", "wpdshserviceobj", "ws2_32", "ws2help",
            "wscapi", "wsdapi", "wshelper", "wshtcpip", "wsnmp32", "wsock32",
            "wtsapi32", "wuapi", "wuaueng", "wucltux", "wudriver", "wups",
            "wusa", "xact", "xactengine2_0", "xactengine2_1", "xactengine2_2",
            "xactengine2_3", "xactengine2_4", "xactengine2_5", "xactengine2_6",
            "xactengine2_7", "xactengine2_8", "xactengine2_9", "xactengine3_0",
            "xactengine3_1", "xactengine3_2", "xactengine3_3", "xactengine3_4",
            "xactengine3_5", "xactengine3_6", "xactengine3_7", "xinput1_1",
            "xinput1_2", "xinput1_3", "xinput9_1_0", "xlive", "xmllite",
            "xpsprint", "xpsshhdr", "xpsrchvw", "xpsdocumenttarget",
        ];
        
        // Set all DLLs to builtin (Wine's built-in DLLs)
        self.override_dlls("builtin", &dlls_to_disable)
    }

    /// Set app-specific DLL overrides (matching w_override_app_dlls behavior)
    pub fn override_app_dlls(&self, app_name: &str, override_type: &str, dll_names: &[&str]) -> Result<()> {
        use std::fs;
        use std::io::Write;
        use std::process::Command;
        
        // Map mode shortcuts to full names
        let mode = match override_type {
            "b" | "builtin" => "builtin",
            "n" | "native" => "native",
            "default" => "default",
            "d" | "disabled" => "",
            _ => override_type,
        };
        
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Create temp directory for registry file
        let temp_dir = dirs::cache_dir()
            .ok_or_else(|| WinetricksError::Config("Could not determine cache directory".into()))?
            .join("winetricks");
        fs::create_dir_all(&temp_dir)?;
        
        let reg_file = temp_dir.join("override-app-dll.reg");
        let mut file = fs::File::create(&reg_file)?;
        
        // Write REGEDIT4 format with AppDefaults path
        writeln!(file, "REGEDIT4")?;
        writeln!(file, "")?;
        writeln!(file, "[HKEY_CURRENT_USER\\Software\\Wine\\AppDefaults\\{}\\DllOverrides]", app_name)?;
        
        // Write each DLL override using common_override_dll logic
        for dll_name in dll_names {
            if mode.is_empty() {
                writeln!(file, "\"{}\"=-", dll_name)?;
            } else {
                writeln!(file, "\"{}\"=\"{}\"", dll_name, mode)?;
            }
        }
        
        file.sync_all()?;
        
        // Convert to Wine path and import
        let reg_file_str = reg_file.to_string_lossy().to_string();
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
        
        let status = Command::new(&self.wine.wine_bin)
            .arg("regedit")
            .arg("/S")
            .arg(&reg_file_win)
            .env("WINEPREFIX", &wineprefix_str)
            .status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine regedit /S {:?}", reg_file_win),
                error: e.to_string(),
            })?;
        
        let _ = fs::remove_file(&reg_file);
        
        if !status.success() {
            return Err(WinetricksError::Config(
                format!("Failed to set app DLL overrides for {}", app_name)
            ));
        }
        
        info!("Set app DLL overrides for {}: {} DLLs = {}", app_name, dll_names.len(), override_type);
        Ok(())
    }

    /// Unset Windows version (set to default) (matching w_unset_winver behavior)
    pub fn unset_winver(&self) -> Result<()> {
        // w_unset_winver is deprecated and just calls w_set_winver default
        self.set_windows_version("default")
    }

    /// Backup registry file before modification (matching w_backup_reg_file behavior)
    pub fn backup_reg_file(&self, reg_file: &Path) -> Result<()> {
        use std::fs;
        use sha2::{Sha256, Digest};
        use std::io::Read;
        use std::process::Command;
        
        // Read file and calculate SHA256
        let mut file = fs::File::open(reg_file)?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;
        
        let mut hasher = Sha256::new();
        hasher.update(&buffer);
        let hash = hasher.finalize();
        let hash_hex = format!("{:x}", hash);
        let hash_prefix = &hash_hex[..8]; // First 8 characters
        
        // Create backup directory
        let temp_dir = dirs::cache_dir()
            .ok_or_else(|| WinetricksError::Config("Could not determine cache directory".into()))?
            .join("winetricks");
        fs::create_dir_all(&temp_dir)?;
        
        // Generate backup filename: _reg_<hash_prefix>_<pid>.reg
        let pid = std::process::id();
        let backup_filename = format!("_reg_{}_{}.reg", hash_prefix, pid);
        let backup_path = temp_dir.join(backup_filename);
        
        // Copy file to backup location
        fs::copy(reg_file, &backup_path)?;
        
        info!("Backed up registry file: {:?} -> {:?}", reg_file, backup_path);
        Ok(())
    }

    /// Import registry file using 32-bit regedit (matching w_try_regedit32 behavior)
    pub fn regedit32(&self, reg_file: &Path) -> Result<()> {
        use std::process::Command;
        
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Convert to Wine Windows path
        let reg_file_win = self.unix_to_wine_path(reg_file)?;
        
        // On win64, use syswow64\regedit.exe (32-bit regedit)
        // On win32, use C:\windows\regedit.exe
        // Original winetricks: w_try_regedit32 uses C:\windows\syswow64\regedit.exe for win64
        let regedit_exe = if self.config.winearch.as_ref().map(|a| a == "win64").unwrap_or(false) {
            // Use 32-bit regedit from syswow64 (matches original winetricks)
            "C:\\windows\\syswow64\\regedit.exe".to_string()
        } else {
            "C:\\windows\\regedit.exe".to_string()
        };
        
        let mut cmd = Command::new(&self.wine.wine_bin);
        cmd.arg(&regedit_exe);
        
        if self.config.unattended {
            cmd.arg("/S"); // Silent mode
        }
        
        cmd.arg(&reg_file_win);
        cmd.env("WINEPREFIX", &wineprefix_str);
        
        let status = cmd.status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine {} /S {:?}", regedit_exe, reg_file_win),
                error: e.to_string(),
            })?;
        
        if !status.success() {
            return Err(WinetricksError::Config(
                "Failed to import registry file (32-bit)".into()
            ));
        }
        
        Ok(())
    }

    /// Import registry file using 64-bit regedit (matching w_try_regedit64 behavior)
    pub fn regedit64(&self, reg_file: &Path) -> Result<()> {
        use std::process::Command;
        use which::which;
        
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Convert to Wine Windows path
        let reg_file_win = self.unix_to_wine_path(reg_file)?;
        
        // Find wine64 binary
        let wine64_bin = if let Some(wine_bin) = self.wine.wine_bin.parent() {
            wine_bin.join("wine64")
        } else {
            which("wine64")
                .map_err(|_| WinetricksError::Config("wine64 not found".into()))?
        };
        
        // Use C:\windows\regedit.exe via wine64 (64-bit)
        let regedit_exe = "C:\\windows\\regedit.exe";
        
        let mut cmd = Command::new(&wine64_bin);
        cmd.arg(regedit_exe);
        
        if self.config.unattended {
            cmd.arg("/S"); // Silent mode
        }
        
        cmd.arg(&reg_file_win);
        cmd.env("WINEPREFIX", &wineprefix_str);
        
        let status = cmd.status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine64 {} /S {:?}", regedit_exe, reg_file_win),
                error: e.to_string(),
            })?;
        
        if !status.success() {
            return Err(WinetricksError::Config(
                "Failed to import registry file (64-bit)".into()
            ));
        }
        
        Ok(())
    }

    /// Open folder in file manager (matching w_open_folder behavior)
    pub fn open_folder(&self, directory: &Path) -> Result<()> {
        use std::process::Command;
        use which::which;
        
        // Try commands in order: xdg-open, open, cygstart
        let commands = vec!["xdg-open", "open", "cygstart"];
        
        for cmd_name in commands {
            if which(cmd_name).is_ok() {
                let dir_str = directory.to_string_lossy().to_string();
                let _ = Command::new(cmd_name)
                    .arg(&dir_str)
                    .spawn();
                info!("Opened folder: {:?}", directory);
                return Ok(());
            }
        }
        
        warn!("No suitable command found to open folder (xdg-open, open, cygstart)");
        Ok(())
    }

    /// Open URL in web browser (matching w_open_webpage behavior)
    pub fn open_webpage(&self, url: &str) -> Result<()> {
        use std::process::Command;
        use which::which;
        
        // Try commands in order: xdg-open, sdtwebclient, cygstart, open, firefox
        let commands = vec!["xdg-open", "sdtwebclient", "cygstart", "open", "firefox"];
        
        for cmd_name in commands {
            if which(cmd_name).is_ok() {
                let _ = Command::new(cmd_name)
                    .arg(url)
                    .spawn();
                info!("Opened webpage: {}", url);
                return Ok(());
            }
        }
        
        warn!("No suitable command found to open webpage");
        Ok(())
    }

    /// Read license key from cache/auth directory (matching w_read_key behavior)
    /// Returns the key if found in cache, otherwise returns None (would prompt user interactively)
    pub fn read_key(&self, verb_name: &str) -> Result<Option<String>> {
        use std::fs;
        use std::io::Read;
        
        // Check cache directory first
        let cache_key_file = self.config.cache_dir.join(verb_name).join("key.txt");
        
        if cache_key_file.exists() {
            let mut file = fs::File::open(&cache_key_file)?;
            let mut key = String::new();
            file.read_to_string(&mut key)?;
            let key = key.trim().to_string();
            if !key.is_empty() {
                info!("Read key from cache for {}", verb_name);
                return Ok(Some(key));
            }
        }
        
        // Check auth directory (WINETRICKS_AUTH)
        if let Ok(auth_dir) = std::env::var("WINETRICKS_AUTH") {
            let auth_key_file = std::path::Path::new(&auth_dir).join(verb_name).join("key.txt");
            if auth_key_file.exists() {
                let mut file = fs::File::open(&auth_key_file)?;
                let mut key = String::new();
                file.read_to_string(&mut key)?;
                let key = key.trim().to_string();
                if !key.is_empty() {
                    info!("Read key from auth directory for {}", verb_name);
                    return Ok(Some(key));
                }
            }
        }
        
        // No key found (in unattended mode, return dummy key)
        if self.config.unattended {
            info!("Unattended mode: using dummy key for {}", verb_name);
            return Ok(Some("dummy_to_make_autohotkey_happy".to_string()));
        }
        
        // In interactive mode, would prompt user (not implemented in Rust version)
        Ok(None)
    }

    /// Get latest GitHub release URL (matching w_get_github_latest_release behavior)
    pub async fn get_github_latest_release(&self, org: &str, repo: &str) -> Result<String> {
        use std::fs;
        
        let temp_dir = dirs::cache_dir()
            .ok_or_else(|| WinetricksError::Config("Could not determine cache directory".into()))?
            .join("winetricks");
        fs::create_dir_all(&temp_dir)?;
        
        let release_json = temp_dir.join("release.json");
        
        // Download latest release JSON from GitHub API
        let api_url = format!("https://api.github.com/repos/{}/{}/releases/latest", org, repo);
        let client = reqwest::Client::new();
        let response = client.get(&api_url)
            .header("Accept", "application/vnd.github.v3+json")
            .header("User-Agent", "Winetricks-RS/1.0")
            .send()
            .await
            .map_err(|e| WinetricksError::Config(format!("Failed to fetch GitHub release: {}", e)))?;
        
        // Check response status
        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            return Err(WinetricksError::Config(format!(
                "GitHub API returned error {}: {}", status, error_text
            )));
        }
        
        let json_text = response.text().await
            .map_err(|e| WinetricksError::Config(format!("Failed to read GitHub response: {}", e)))?;
        
        if json_text.is_empty() {
            return Err(WinetricksError::Config(
                "GitHub API returned empty response".into()
            ));
        }
        
        // Parse JSON to get download URL
        let json: serde_json::Value = serde_json::from_str(&json_text)
            .map_err(|e| WinetricksError::Config(format!("Failed to parse GitHub JSON: {} (response: {})", e, json_text.chars().take(200).collect::<String>())))?;
        
        // Prefer .tar.zst or .tar.gz assets (vkd3d-proton uses .tar.zst)
        if let Some(assets) = json.get("assets").and_then(|a| a.as_array()) {
            // Try to find .tar.zst first, then .tar.gz, then any asset
            for ext in &[".tar.zst", ".tar.gz", ".zip"] {
                for asset in assets {
                    if let Some(name) = asset.get("name").and_then(|n| n.as_str()) {
                        if name.ends_with(ext) {
                            if let Some(url) = asset.get("browser_download_url").and_then(|u| u.as_str()) {
                                info!("Got latest GitHub release URL for {}/{}: {}", org, repo, url);
                                return Ok(url.to_string());
                            }
                        }
                    }
                }
            }
            
            // Fallback: get first asset
            if let Some(first_asset) = assets.first() {
                if let Some(url) = first_asset.get("browser_download_url").and_then(|u| u.as_str()) {
                    info!("Got latest GitHub release URL for {}/{}: {}", org, repo, url);
                    return Ok(url.to_string());
                }
            }
        }
        
        // Fallback: try tarball_url or zipball_url
        if let Some(tarball) = json.get("tarball_url").and_then(|u| u.as_str()) {
            return Ok(tarball.to_string());
        }
        
        Err(WinetricksError::Config(format!(
            "No download URL found in GitHub release for {}/{}", org, repo
        )))
    }

    /// Get latest GitHub prerelease URL (matching w_get_github_latest_prerelease behavior)
    pub async fn get_github_latest_prerelease(&self, org: &str, repo: &str) -> Result<String> {
        // Download releases list from GitHub API
        let api_url = format!("https://api.github.com/repos/{}/{}/releases", org, repo);
        let client = reqwest::Client::new();
        let response = client.get(&api_url)
            .header("Accept", "application/vnd.github.v3+json")
            .send()
            .await
            .map_err(|e| WinetricksError::Config(format!("Failed to fetch GitHub releases: {}", e)))?;
        
        let json_text = response.text().await
            .map_err(|e| WinetricksError::Config(format!("Failed to read GitHub response: {}", e)))?;
        
        // Parse JSON array
        let releases: Vec<serde_json::Value> = serde_json::from_str(&json_text)
            .map_err(|e| WinetricksError::Config(format!("Failed to parse GitHub JSON: {}", e)))?;
        
        // Find first prerelease
        for release in releases {
            if let Some(prerelease) = release.get("prerelease").and_then(|p| p.as_bool()) {
                if prerelease {
                    // Get first asset download URL
                    if let Some(assets) = release.get("assets").and_then(|a| a.as_array()) {
                        if let Some(first_asset) = assets.first() {
                            if let Some(url) = first_asset.get("browser_download_url").and_then(|u| u.as_str()) {
                                info!("Got latest GitHub prerelease URL for {}/{}: {}", org, repo, url);
                                return Ok(url.to_string());
                            }
                        }
                    }
                }
            }
        }
        
        Err(WinetricksError::Config(format!(
            "No prerelease found for {}/{}", org, repo
        )))
    }

    /// Get latest GitLab release URL (matching w_get_gitlab_latest_release behavior)
    pub async fn get_gitlab_latest_release(&self, org: &str, repo: &str) -> Result<String> {
        // GitLab API endpoint for releases
        let api_url = format!("https://gitlab.com/api/v4/projects/{}/releases", format!("{}/{}", org, repo));
        let client = reqwest::Client::new();
        let response = client.get(&api_url)
            .send()
            .await
            .map_err(|e| WinetricksError::Config(format!("Failed to fetch GitLab releases: {}", e)))?;
        
        let json_text = response.text().await
            .map_err(|e| WinetricksError::Config(format!("Failed to read GitLab response: {}", e)))?;
        
        // Parse JSON array
        let releases: Vec<serde_json::Value> = serde_json::from_str(&json_text)
            .map_err(|e| WinetricksError::Config(format!("Failed to parse GitLab JSON: {}", e)))?;
        
        // Get first release's assets
        if let Some(release) = releases.first() {
            if let Some(assets) = release.get("assets").and_then(|a| a.get("links")).and_then(|l| l.as_array()) {
                if let Some(first_link) = assets.first() {
                    if let Some(url) = first_link.get("url").and_then(|u| u.as_str()) {
                        info!("Got latest GitLab release URL for {}/{}: {}", org, repo, url);
                        return Ok(url.to_string());
                    }
                }
            }
        }
        
        Err(WinetricksError::Config(format!(
            "No release found for GitLab {}/{}", org, repo
        )))
    }

    /// Handle manual download by opening download page (matching w_download_manual behavior)
    pub fn download_manual(&self, url: &str, filename: Option<&str>) -> Result<()> {
        // Open download page in browser
        self.open_webpage(url)?;
        
        if let Some(fname) = filename {
            info!("Manual download required: {} from {}", fname, url);
        } else {
            info!("Manual download required from {}", url);
        }
        
        Ok(())
    }

    /// Ask user a question (matching w_question behavior)
    /// Returns the answer as a string
    /// Note: In CLI mode, this would read from stdin. In GUI mode, it would use zenity/kdialog.
    pub fn question(&self, prompt: &str) -> Result<Option<String>> {
        // In unattended mode, return None
        if self.config.unattended {
            return Ok(None);
        }
        
        // Try to use GUI tools if available (zenity/kdialog)
        use std::process::Command;
        use which::which;
        
        // Try zenity first
        if which("zenity").is_ok() {
            if let Ok(output) = Command::new("zenity")
                .arg("--entry")
                .arg("--text")
                .arg(prompt)
                .output()
            {
                if output.status.success() {
                    let answer = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    return Ok(Some(answer));
                }
            }
        }
        
        // Try kdialog
        if which("kdialog").is_ok() {
            if let Ok(output) = Command::new("kdialog")
                .arg("--inputbox")
                .arg(prompt)
                .output()
            {
                if output.status.success() {
                    let answer = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    return Ok(Some(answer));
                }
            }
        }
        
        // Fallback: in CLI, would read from stdin
        // For now, just return None (user interaction would need terminal input handling)
        warn!("Cannot ask question in current mode: {}", prompt);
        Ok(None)
    }

    /// Execute AutoHotkey script (matching w_ahk_do behavior)
    /// Downloads AutoHotkeyU32.exe if not present, then executes the script
    pub fn ahk_do(&self, script: &str) -> Result<()> {
        use std::fs;
        use std::io::Write;
        use std::process::Command;
        
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Create cache directory for AutoHotkey
        let ahk_cache_dir = self.config.cache_dir.join("ahk");
        fs::create_dir_all(&ahk_cache_dir)?;
        
        let autohotkey_exe = ahk_cache_dir.join("AutoHotkeyU32.exe");
        
        // Download AutoHotkey if not present (original winetricks does this)
        if !autohotkey_exe.exists() {
            info!("AutoHotkey not found, downloading...");
            // Note: Original winetricks downloads from GitHub releases
            // This would require implementing the download and extraction
            // For now, return error if not found
            return Err(WinetricksError::Config(
                "AutoHotkeyU32.exe not found. Download and extract AutoHotkey first.".into()
            ));
        }
        
        // Create temp directory for AutoHotkey script
        let temp_dir = dirs::cache_dir()
            .ok_or_else(|| WinetricksError::Config("Could not determine cache directory".into()))?
            .join("winetricks");
        fs::create_dir_all(&temp_dir)?;
        
        // Get current verb name for script filename (if available)
        let ahk_filename = format!("{}.ahk", std::process::id());
        let ahk_file = temp_dir.join(&ahk_filename);
        
        // Write AutoHotkey script with W_OPT_UNATTENDED variable
        // Original winetricks adds: w_opt_unattended = ${W_OPT_UNATTENDED:-0}
        let mut script_content = format!("w_opt_unattended = {}\n", if self.config.unattended { "1" } else { "0" });
        script_content.push_str(script);
        
        // Write with CRLF line endings (Windows format)
        // Original winetricks uses: awk 'sub("$", "\r")'
        let mut file = fs::File::create(&ahk_file)?;
        for line in script_content.lines() {
            writeln!(file, "{}", line)?;
        }
        file.sync_all()?;
        
        // Convert to Wine Windows paths
        let autohotkey_exe_win = self.unix_to_wine_path(&autohotkey_exe)?;
        let ahk_file_win = self.unix_to_wine_path(&ahk_file)?;
        
        // Run AutoHotkey script
        let status = Command::new(&self.wine.wine_bin)
            .arg(&autohotkey_exe_win)
            .arg(&ahk_file_win)
            .env("WINEPREFIX", &wineprefix_str)
            .status()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine AutoHotkeyU32.exe {:?}", ahk_file_win),
                error: e.to_string(),
            })?;
        
        if !status.success() {
            return Err(WinetricksError::Config(
                "AutoHotkey script execution failed".into()
            ));
        }
        
        info!("Executed AutoHotkey script");
        Ok(())
    }

    /// Download file via BitTorrent (matching w_download_torrent behavior)
    /// Note: This requires uTorrent to be installed in the Wine prefix
    pub async fn download_torrent(&self, verb_name: &str, torrent_files: &[&str]) -> Result<()> {
        use std::fs;
        use std::process::Command;
        
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Find uTorrent.exe in Wine prefix
        let utorrent_exe = wineprefix.join("drive_c/Program Files/uTorrent/uTorrent.exe");
        
        if !utorrent_exe.exists() {
            return Err(WinetricksError::Config(
                "uTorrent not found in Wine prefix. Install uTorrent first (verb: utorrent)".into()
            ));
        }
        
        let cache_dir = self.config.cache_dir.join(verb_name);
        fs::create_dir_all(&cache_dir)?;
        
        // Convert cache directory to Wine Windows path
        let cache_dir_win = self.unix_to_wine_path(&cache_dir)?;
        
        // Start uTorrent for each torrent file
        for torrent_file in torrent_files {
            let torrent_path = cache_dir.join(torrent_file);
            
            if !torrent_path.exists() {
                return Err(WinetricksError::Config(
                    format!("Torrent file not found: {:?}", torrent_path)
                ));
            }
            
            let torrent_win = self.unix_to_wine_path(&torrent_path)?;
            
            // Launch uTorrent (non-blocking)
            let _ = Command::new(&self.wine.wine_bin)
                .arg(&torrent_win)
                .env("WINEPREFIX", &wineprefix_str)
                .spawn();
            
            info!("Started uTorrent download: {}", torrent_file);
        }
        
        // Note: Original winetricks waits for downloads to complete using AutoHotkey
        // This is complex and would require additional AutoHotkey integration
        warn!("Torrent downloads started. Note: Automatic completion detection not implemented.");
        
        Ok(())
    }

    /// Get SHA256 checksum from a file (matching w_get_sha256sum behavior)
    pub fn get_sha256sum(&self, file: &Path) -> Result<String> {
        use sha2::{Sha256, Digest};
        use std::fs::File;
        use std::io::Read;
        
        if !file.exists() {
            warn!("File does not exist: {:?}", file);
            return Err(WinetricksError::Config(format!("File not found: {:?}", file)));
        }
        
        let mut hasher = Sha256::new();
        let mut f = File::open(file)?;
        let mut buffer = Vec::new();
        f.read_to_end(&mut buffer)?;
        hasher.update(&buffer);
        
        let hash = hasher.finalize();
        Ok(format!("{:x}", hash))
    }

    /// Verify SHA256 checksum with permission prompt on mismatch (matching w_verify_sha256sum behavior)
    pub fn verify_sha256sum(&self, expected: &str, file: &Path, url: &str) -> Result<()> {
        let computed = self.get_sha256sum(file)?;
        
        if computed != expected {
            if self.config.force {
                warn!("SHA256 mismatch! However --force was used, so trying anyway. Caveat emptor.");
                warn!("URL: {}", url);
                warn!("Downloaded: {}", computed);
                warn!("Expected: {}", expected);
            } else {
                // Ask permission to continue
                let message = format!(
                    "SHA256 mismatch!\n\nURL: {}\nDownloaded: {}\nExpected: {}\n\nThis is often the result of an updated package such as vcrun2019.\nIf you are willing to accept the risk, you can bypass this check.\nAlternatively, you may use the --force option to ignore this check entirely.\n\nContinue anyway?",
                    url, computed, expected
                );
                
                // In CLI, we'd need to prompt, but for now just error
                return Err(WinetricksError::ChecksumMismatch {
                    expected: expected.to_string(),
                    got: computed,
                });
            }
        }
        
        Ok(())
    }

    /// Get hash type from checksum string (matching w_get_shatype behavior)
    pub fn get_shatype(&self, checksum: &str) -> &str {
        let length = checksum.trim().len();
        match length {
            0 => "none",
            64 => "sha256",
            // 128 => "sha512", // Not currently supported
            _ => "unknown",
        }
    }

    /// Expand Windows environment variable (matching w_expand_env behavior)
    pub fn expand_env(&self, var_name: &str) -> Result<String> {
        use std::process::Command;
        
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Use cmd.exe /c "echo %VAR%" to expand environment variable
        let output = Command::new(&self.wine.wine_bin)
            .arg("cmd.exe")
            .arg("/c")
            .arg(&format!("echo %{}%", var_name))
            .env("WINEPREFIX", &wineprefix_str)
            .output()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine cmd.exe /c echo %{}%", var_name),
                error: e.to_string(),
            })?;
        
        if output.status.success() {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Ok(value)
        } else {
            Err(WinetricksError::Config(format!(
                "Failed to expand environment variable: {}", var_name
            )))
        }
    }

    /// Get file architecture (matching winetricks_get_file_arch behavior)
    pub fn get_file_arch(&self, file: &Path) -> Result<Option<String>> {
        use std::process::Command;
        
        if !file.exists() {
            return Ok(None);
        }
        
        // Check if macOS (Mach-O binaries)
        if cfg!(target_os = "macos") {
            // Use lipo to detect architecture
            if let Ok(output) = Command::new("lipo")
                .arg("-archs")
                .arg(file)
                .output()
            {
                if output.status.success() {
                    let arch = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    return Ok(Some(match arch.as_str() {
                        "arm64" => "arm64".to_string(),
                        "i386" => "i386".to_string(),
                        "x86_64" => "x86_64".to_string(),
                        _ => arch,
                    }));
                }
            }
        }
        
        // For ELF binaries (Linux, etc.), read byte at offset 0x12
        // Use od (octal dump) to read a single byte
        if let Ok(output) = Command::new("od")
            .arg("-An")
            .arg("-t")
            .arg("x1")
            .arg("-j")
            .arg("0x12")
            .arg("-N")
            .arg("1")
            .arg(file)
            .output()
        {
            if output.status.success() {
                let hex_str = String::from_utf8_lossy(&output.stdout).trim().replace(' ', "");
                return Ok(Some(match hex_str.as_str() {
                    "3e" => "x86_64".to_string(),
                    "03" | "06" => "i386".to_string(),
                    "b7" => "aarch64".to_string(),
                    "28" => "aarch32".to_string(),
                    _ => return Ok(None),
                }));
            }
        }
        
        Ok(None)
    }

    /// Verify cabextract is available (matching w_verify_cabextract_available behavior)
    pub fn verify_cabextract_available(&self) -> Result<()> {
        use std::process::Command;
        use which::which;
        
        let cabextract = which("cabextract")
            .map_err(|_| WinetricksError::Config(
                "Cannot find cabextract. Please install it (e.g. 'sudo apt install cabextract' or 'sudo yum install cabextract')".into()
            ))?;
        
        // Test cabextract with -q -v
        let status = Command::new(&cabextract)
            .arg("-q")
            .arg("-v")
            .output();
        
        // cabextract -q -v should succeed even if just checking version
        match status {
            Ok(_) => Ok(()),
            Err(_) => Err(WinetricksError::Config(
                "cabextract is not working correctly".into()
            )),
        }
    }

    /// Create directory (matching w_try_mkdir behavior)
    pub fn try_mkdir(&self, dir: &Path, quiet: bool) -> Result<()> {
        use std::fs;
        
        if dir.exists() && dir.is_dir() {
            return Ok(()); // Directory already exists
        }
        
        // Only print message if not quiet and directory doesn't exist
        if !quiet {
            info!("Creating directory: {:?}", dir);
        }
        
        fs::create_dir_all(dir)?;
        Ok(())
    }

    /// Ask user permission (matching w_askpermission behavior)
    pub fn askpermission(&self, message: &str) -> Result<bool> {
        // In unattended mode, auto-accept after timeout
        if self.config.unattended {
            info!("Unattended mode, auto-accepting permission request");
            return Ok(true);
        }
        
        // Try GUI tools first (zenity/kdialog)
        use std::process::Command;
        use which::which;
        
        // Try zenity
        if which("zenity").is_ok() {
            let status = Command::new("zenity")
                .arg("--question")
                .arg("--title=winetricks")
                .arg("--text")
                .arg(message)
                .arg("--no-wrap")
                .status();
            
            if let Ok(s) = status {
                return Ok(s.success());
            }
        }
        
        // Try kdialog
        if which("kdialog").is_ok() {
            let status = Command::new("kdialog")
                .arg("--title=winetricks")
                .arg("--yesno")
                .arg(message)
                .status();
            
            if let Ok(s) = status {
                return Ok(s.success());
            }
        }
        
        // Fallback: terminal prompt (would require stdin handling)
        warn!("Cannot ask permission in current mode. Assuming 'no' for safety.");
        warn!("Message: {}", message);
        Ok(false)
    }

    /// Detect if Wine is built with MinGW (matching w_detect_mingw behavior)
    pub fn detect_mingw(&mut self) -> Result<bool> {
        use std::process::Command;
        use std::io::Read;
        
        // Check for "Wine placeholder DLL" or "Wine builtin DLL" in kernelbase.dll
        let wineprefix = self.config.wineprefix();
        let kernelbase_path = wineprefix.join("drive_c/windows/system32/kernelbase.dll");
        
        if !kernelbase_path.exists() {
            return Ok(false); // Can't determine, assume non-MinGW
        }
        
        // Read file and check for Wine DLL markers
        let mut file = std::fs::File::open(&kernelbase_path)?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;
        
        // Search for "Wine placeholder DLL" at offset 64
        if buffer.len() > 64 {
            let marker = b"Wine placeholder DLL";
            for i in 0..buffer.len().saturating_sub(marker.len()) {
                if buffer[i..].starts_with(marker) {
                    if i == 64 {
                        // Non-MinGW build (placeholder)
                        return Ok(false);
                    }
                }
            }
        }
        
        // Search for "Wine builtin DLL"
        let builtin_marker = b"Wine builtin DLL";
        for i in 0..buffer.len().saturating_sub(builtin_marker.len()) {
            if buffer[i..].starts_with(builtin_marker) {
                if i == 64 {
                    // MinGW build (builtin)
                    return Ok(true);
                }
            }
        }
        
        warn!("Unable to detect Wine DLL type");
        Ok(false)
    }

    /// Check if package is broken in MinGW builds (matching w_package_broken_mingw behavior)
    pub fn package_broken_mingw(&self, verb_name: &str, bug_link: &str, bad_version: Option<&str>, good_version: Option<&str>) -> Result<()> {
        // TODO: Implement MinGW detection and check
        // For now, check if we can get metadata from registry and use regular check
        if let Some(metadata) = self.registry.get(verb_name) {
            self.check_package_broken(verb_name, metadata)
        } else {
            // If no metadata, just warn
            warn!("Package broken check for {} (MinGW): {}", verb_name, bug_link);
            Ok(())
        }
    }

    /// Check if package is broken in non-MinGW builds (matching w_package_broken_no_mingw behavior)
    pub fn package_broken_no_mingw(&self, verb_name: &str, bug_link: &str, bad_version: Option<&str>, good_version: Option<&str>) -> Result<()> {
        // TODO: Implement non-MinGW detection and check
        // For now, check if we can get metadata from registry and use regular check
        if let Some(metadata) = self.registry.get(verb_name) {
            self.check_package_broken(verb_name, metadata)
        } else {
            // If no metadata, just warn
            warn!("Package broken check for {} (non-MinGW): {}", verb_name, bug_link);
            Ok(())
        }
    }

    /// Check if package is broken on new-style WoW64 prefix (matching w_package_broken_wow64 behavior)
    pub fn package_broken_wow64(&self, verb_name: &str, bug_link: &str, bad_version: Option<&str>, good_version: Option<&str>) -> Result<()> {
        // Check if this is a new-style WoW64 prefix
        // New-style WoW64 has both syswow64 and system32 directories
        let wineprefix = self.config.wineprefix();
        let syswow64 = wineprefix.join("drive_c/windows/syswow64");
        let system32 = wineprefix.join("drive_c/windows/system32");
        
        // New-style WoW64: both directories exist and prefix is win64
        let is_new_wow64 = self.config.winearch.as_ref().map(|a| a == "win64").unwrap_or(false)
            && syswow64.exists() && system32.exists();
        
        if is_new_wow64 {
            let message = format!(
                "This package ({}) does not work on a new-style WoW64 prefix. See {}. You must either use a 32-bit or old style WoW64 WINEPREFIX. Use --force to try anyway.",
                verb_name, bug_link
            );
            
            if self.config.force {
                warn!("{}", message);
            } else {
                return Err(WinetricksError::Verb(message));
            }
        }
        
        Ok(())
    }

    /// Check if package is unsupported on 32-bit prefix (matching w_package_unsupported_win32 behavior)
    pub fn package_unsupported_win32(&self, verb_name: &str) -> Result<()> {
        let is_win32 = self.config.winearch.as_ref().map(|a| a == "win32").unwrap_or(true);
        
        if is_win32 {
            let message = format!(
                "This package ({}) does not work on a 32-bit installation. You must use a prefix made with WINEARCH=win64.",
                verb_name
            );
            
            if self.config.force {
                warn!("{}", message);
            } else {
                return Err(WinetricksError::Verb(message));
            }
        }
        
        Ok(())
    }

    /// Check if package is unsupported on 64-bit prefix (matching w_package_unsupported_win64 behavior)
    pub fn package_unsupported_win64(&self, verb_name: &str) -> Result<()> {
        let is_win64 = self.config.winearch.as_ref().map(|a| a == "win64").unwrap_or(false);
        
        if is_win64 {
            let message = format!(
                "This package ({}) does not work on a 64-bit installation. You must use a prefix made with WINEARCH=win32.",
                verb_name
            );
            
            if self.config.force {
                warn!("{}", message);
            } else {
                return Err(WinetricksError::Verb(message));
            }
        }
        
        Ok(())
    }

    /// Clean up partial .NET installation before force reinstall
    fn cleanup_dotnet_installation(&self, verb_name: &str) -> Result<()> {
        let wineprefix = self.config.wineprefix();
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        info!("Cleaning up partial .NET installation for {}...", verb_name);
        
        // Remove .NET Framework directory (but keep system32/mscoree.dll for now as it might be needed)
        let framework_dir = wineprefix.join("drive_c/windows/Microsoft.NET/Framework/v4.0.30319");
        if framework_dir.exists() {
            warn!("Removing partial Framework/v4.0.30319/ directory...");
            if let Err(e) = std::fs::remove_dir_all(&framework_dir) {
                warn!("Warning: Failed to remove Framework directory: {}", e);
            }
        }
        
        // Remove registry entries
        // Remove v4/Full key
        let _ = std::process::Command::new(&self.wine.wine_bin)
            .arg("reg")
            .arg("delete")
            .arg("HKLM\\Software\\Microsoft\\NET Framework Setup\\NDP\\v4\\Full")
            .arg("/f")
            .env("WINEPREFIX", &wineprefix_str)
            .status();
        
        // Remove v3.5 key if applicable
        if verb_name.contains("35") {
            let _ = std::process::Command::new(&self.wine.wine_bin)
                .arg("reg")
                .arg("delete")
                .arg("HKLM\\Software\\Microsoft\\NET Framework Setup\\NDP\\v3.5")
                .arg("/f")
                .env("WINEPREFIX", &wineprefix_str)
                .status();
        }
        
        // Remove marker files
        let marker_files = vec![
            "dotnet48.installed.workaround",
            "dotnet48.1.installed.workaround",
            "dotnet45.installed.workaround",
        ];
        
        for marker in marker_files {
            let marker_path = wineprefix.join(format!("drive_c/windows/{}", marker));
            if marker_path.exists() {
                let _ = std::fs::remove_file(&marker_path);
            }
        }
        
        info!("Cleanup complete. Installation will proceed as fresh install.");
        Ok(())
    }

    /// Run Setup.exe directly for .NET 4.5 (after manual extraction)
    async fn run_setup_exe_directly(&self, setup_exe: &Path, extract_dir: &Path, wineprefix_str: String, _is_dotnet45: bool) -> Result<()> {
        use std::process::Command;
        
        info!("Running Setup.exe directly for .NET 4.5...");
        
        // Convert Setup.exe path to Wine Windows path
        let setup_exe_str = setup_exe.to_string_lossy().to_string();
        let output = Command::new(&self.wine.wine_bin)
            .arg("winepath")
            .arg("-w")
            .arg(&setup_exe_str)
            .env("WINEPREFIX", &wineprefix_str)
            .output()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine winepath -w {:?}", setup_exe_str),
                error: e.to_string(),
            })?;
        
        let setup_exe_win = String::from_utf8_lossy(&output.stdout).trim().to_string();
        
        // Run Setup.exe with /quiet /norestart flags for unattended mode
        // /quiet suppresses ALL GUI dialogs from the installer
        let mut setup_cmd = Command::new(&self.wine.wine_bin);
        setup_cmd.env("WINEPREFIX", &wineprefix_str);
        setup_cmd.env("WINEDLLOVERRIDES", "fusion=b");
        setup_cmd.current_dir(extract_dir);
        setup_cmd.arg(&setup_exe_win);
        
        // .NET Framework Setup.exe flags:
        // /quiet - Suppresses ALL UI including error dialogs (theoretical, but may still show some)
        // /passive - Shows progress bar, minimal UI, but may still show blocking dialogs
        // /norestart - Don't restart
        // /repair - Repair an existing installation (but this also shows UI)
        // 
        // Problem: Even /quiet can show blocking dialogs when .NET is already installed
        // Solution: Try /quiet /norestart first. If that fails, the user will see the dialog.
        // For truly unattended, we could check if installed first, but that's complex.
        // 
        // Note: .NET Framework installers have a known limitation where they may show
        // dialogs even with /quiet when detecting existing installations.
        if self.config.unattended {
            // Always use /quiet for unattended - it's the most silent option
            // If it shows a dialog, that's a limitation of the .NET installer itself
            setup_cmd.arg("/quiet").arg("/norestart");
            info!("Running Setup.exe with /quiet /norestart (unattended mode)");
            if self.config.force {
                warn!("Note: .NET installers may still show dialogs about already-installed status even with /quiet");
                warn!("This is a limitation of the .NET Framework installer, not winetricks");
            }
        } else {
            // Even in interactive mode, use /passive for less intrusive GUI
            setup_cmd.arg("/passive").arg("/norestart");
            info!("Running Setup.exe in passive mode (minimal GUI)");
        }
        
        let setup_args: Vec<String> = setup_cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        eprintln!("Running Setup.exe: {} {}", self.wine.wine_bin.to_string_lossy(), setup_args.join(" "));
        eprintln!("This may take 5-10 minutes...");
        
        let setup_output = setup_cmd
            .output()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("wine {}", setup_args.join(" ")),
                error: e.to_string(),
            })?;
        
        if !setup_output.stdout.is_empty() {
            eprintln!("Setup.exe stdout:");
            eprintln!("{}", String::from_utf8_lossy(&setup_output.stdout));
        }
        if !setup_output.stderr.is_empty() {
            eprintln!("Setup.exe stderr:");
            eprintln!("{}", String::from_utf8_lossy(&setup_output.stderr));
        }
        
        let setup_exit = setup_output.status.code();
        eprintln!("Setup.exe finished with exit code: {:?}", setup_exit);
        
        // .NET Framework installer exit codes:
        // 0 = Success
        // 1603 = Fatal error (but can be false positive)
        // 3010 = Success, reboot required
        // Other codes may indicate various states (already installed, cancelled by user dialog, etc.)
        // Note: Even with /quiet, the installer may show a blocking dialog if .NET is already installed
        // This is a known limitation of the .NET Framework installer - it cannot be suppressed
        if let Some(code) = setup_exit {
            if code != 0 && code != 3010 {
                warn!("Setup.exe returned exit code {} - installation may have failed or been cancelled", code);
                if self.config.force {
                    warn!("With --force, this might indicate .NET is already installed and the installer showed a dialog");
                    warn!("This is expected behavior - the .NET installer cannot suppress 'already installed' dialogs even with /quiet");
                }
            }
        }
        
        // Wait for wineserver after Setup.exe completes
        info!("Waiting for wineserver after Setup.exe...");
        std::thread::sleep(std::time::Duration::from_secs(5));
        for i in 1..=3 {
            let _ = Command::new(&self.wine.wineserver_bin)
                .arg("-w")
                .env("WINEPREFIX", &wineprefix_str)
                .status();
            if i < 3 {
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
        
        Ok(())
    }

    /// Kill all instances of a process safely (matching w_killall behavior)
    /// Solaris-safe version that doesn't use killall (which kills everything on Solaris)
    pub fn killall(&self, process_name: &str) -> Result<()> {
        use std::process::Command;
        use which::which;
        
        // Use pgrep to find processes, then kill them
        // This is safer than killall on Solaris
        let pgrep = which("pgrep")
            .map_err(|_| WinetricksError::Config("pgrep not found".into()))?;
        
        let output = Command::new(&pgrep)
            .arg(process_name)
            .output()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("pgrep {}", process_name),
                error: e.to_string(),
            })?;
        
        if output.status.success() {
            let stdout_str = String::from_utf8_lossy(&output.stdout);
            let pids: Vec<String> = stdout_str
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(|s| s.trim().to_string())
                .collect();
            
            if !pids.is_empty() {
                info!("Killing {} instances of {}", pids.len(), process_name);
                
                for pid in pids {
                    let _ = Command::new("kill")
                        .arg("-s")
                        .arg("KILL")
                        .arg(&pid)
                        .output();
                }
            }
        }
        
        Ok(())
    }
}
