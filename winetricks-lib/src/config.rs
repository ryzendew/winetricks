//! Configuration management for winetricks

use crate::error::{Result, WinetricksError};
use dirs;
use std::path::{Path, PathBuf};
use tracing::info;

/// Winetricks configuration
#[derive(Debug, Clone)]
pub struct Config {
    /// Cache directory for downloads
    pub cache_dir: PathBuf,

    /// Data directory for winetricks files
    pub data_dir: PathBuf,

    /// Root directory for wine prefixes
    pub prefixes_root: PathBuf,

    /// Current wine prefix
    pub wineprefix: Option<PathBuf>,

    /// Verbosity level (0-2)
    pub verbosity: u8,

    /// Force operations even if already installed
    pub force: bool,

    /// Unattended mode (no prompts)
    pub unattended: bool,

    /// Use torify for downloads
    pub torify: bool,

    /// Wine architecture (win32 or win64)
    pub winearch: Option<String>,

    /// Wine D3D renderer (opengl, vulkan, or none)
    pub renderer: Option<String>,

    /// Wine display driver (wayland, xwayland, or auto)
    pub wayland: Option<String>,

    /// Isolate each app in its own prefix (--isolate)
    pub isolate: bool,

    /// Don't delete temp directories (--no-clean)
    pub no_clean: bool,
}

impl Config {
    /// Create a new config with default paths
    pub fn new() -> Result<Self> {
        let cache_dir = dirs::cache_dir()
            .ok_or_else(|| WinetricksError::Config("Could not determine cache directory".into()))?
            .join("winetricks");

        let data_dir = dirs::data_dir()
            .ok_or_else(|| WinetricksError::Config("Could not determine data directory".into()))?
            .join("winetricks");

        let prefixes_root = dirs::data_dir()
            .ok_or_else(|| WinetricksError::Config("Could not determine data directory".into()))?
            .join("wineprefixes");

        Ok(Self {
            cache_dir,
            data_dir,
            prefixes_root,
            wineprefix: None,
            verbosity: 0,
            force: false,
            unattended: false,
            torify: false,
            winearch: None,
            renderer: None,
            wayland: None,
            isolate: false,
            no_clean: false,
        })
    }

    /// Get the wine prefix path (default or configured)
    pub fn wineprefix(&self) -> PathBuf {
        self.wineprefix
            .clone()
            .or_else(|| std::env::var("WINEPREFIX").ok().map(PathBuf::from))
            .unwrap_or_else(|| dirs::home_dir().unwrap().join(".wine"))
    }

    /// Get source JSON directory (files/json/ in project, or empty if not found)
    pub fn source_json_dir(&self) -> Option<PathBuf> {
        if let Ok(current_exe) = std::env::current_exe() {
            let mut exe_path = current_exe.clone();
            while exe_path.parent().is_some() {
                exe_path = exe_path.parent().unwrap().to_path_buf();
                let json_dir = exe_path.join("files").join("json");
                if json_dir.exists() && json_dir.is_dir() {
                    return Some(json_dir);
                }
            }
        }
        None
    }

    /// Get cached verbs directory (~/.config/winetricks/)
    /// This serves as the roadmap - all JSON files are stored here with category subdirectories
    pub fn cached_verbs_dir(&self) -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap().join(".config"))
            .join("winetricks")
    }

    /// Initialize cache from source JSON files if needed
    pub fn ensure_cache_initialized(&self) -> Result<()> {
        let cached_dir = self.cached_verbs_dir();
        let source_dir = match self.source_json_dir() {
            Some(dir) => {
                info!("Found source JSON directory: {:?}", dir);
                dir
            }
            None => {
                info!("No source JSON directory found, skipping cache initialization");
                return Ok(()); // No source directory, skip cache initialization
            }
        };

        // Check if cache needs updating
        let needs_update = if !cached_dir.exists() {
            true
        } else {
            // Check if any source file is newer than cache
            self.is_cache_stale(&source_dir, &cached_dir)?
        };

        if needs_update {
            info!("Initializing/updating verb cache from source files...");
            self.copy_json_to_cache(&source_dir, &cached_dir)?;
            info!("Verb cache initialized at: {:?}", cached_dir);
        }

        Ok(())
    }

    /// Check if cache is stale (source files are newer)
    fn is_cache_stale(&self, source_dir: &Path, cached_dir: &Path) -> Result<bool> {
        use std::fs;

        // Get most recent modification time from source
        let mut source_mtime = None;
        for entry in fs::read_dir(source_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                // Check subdirectories recursively
                if let Ok(mtime) = self.get_dir_mtime(&path) {
                    source_mtime = source_mtime.max(Some(mtime));
                }
            }
        }

        // Get most recent modification time from cache (check category subdirectories)
        let cache_mtime = if cached_dir.exists() {
            // Check all category subdirectories for modification times
            let mut latest_cache_mtime = None;
            if let Ok(entries) = fs::read_dir(cached_dir) {
                for entry in entries {
                    if let Ok(entry) = entry {
                        let path = entry.path();
                        if path.is_dir() {
                            // This is a category directory (dlls/, apps/, etc.)
                            if let Ok(mtime) = self.get_dir_mtime(&path) {
                                latest_cache_mtime = latest_cache_mtime.max(Some(mtime));
                            }
                        }
                    }
                }
            }
            latest_cache_mtime
        } else {
            None
        };

        Ok(source_mtime > cache_mtime || cache_mtime.is_none())
    }

    /// Get most recent modification time of files in a directory (recursive)
    fn get_dir_mtime(&self, dir: &Path) -> Result<std::time::SystemTime> {
        use std::fs;
        let mut latest = None;

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            
            let mtime = if path.is_dir() {
                self.get_dir_mtime(&path)?
            } else {
                fs::metadata(&path)?.modified()?
            };

            latest = latest.max(Some(mtime));
        }

        latest.ok_or_else(|| WinetricksError::Config("Directory has no files".into()))
    }

    /// Copy JSON files from source to cache directory
    /// Preserves the directory structure (dlls/, apps/, fonts/, etc.) in ~/.config/winetricks/
    fn copy_json_to_cache(&self, source_dir: &Path, cached_dir: &Path) -> Result<()> {
        use std::fs;

        // Ensure cache directory exists (don't remove it - we want to keep other files)
        fs::create_dir_all(&cached_dir)?;

        // Copy all JSON files preserving directory structure
        for entry in fs::read_dir(source_dir)? {
            let entry = entry?;
            let source_path = entry.path();
            
            if source_path.is_dir() {
                let category_name = source_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| WinetricksError::Config("Invalid category directory".into()))?;
                
                let cached_category_dir = cached_dir.join(category_name);
                fs::create_dir_all(&cached_category_dir)?;

                // Copy all JSON files in this category
                for json_entry in fs::read_dir(&source_path)? {
                    let json_entry = json_entry?;
                    let json_path = json_entry.path();
                    
                    if json_path.extension().and_then(|s| s.to_str()) == Some("json") {
                        let filename = json_path
                            .file_name()
                            .ok_or_else(|| WinetricksError::Config("Invalid filename".into()))?;
                        
                        let dest_path = cached_category_dir.join(filename);
                        fs::copy(&json_path, &dest_path)?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Get metadata directory (uses cached location, falls back to source)
    /// The cached location is ~/.config/winetricks/ which contains category subdirectories (dlls/, apps/, etc.)
    pub fn metadata_dir(&self) -> PathBuf {
        // If data_dir is already verbs_metadata (development mode), return it directly
        if self.data_dir.ends_with("verbs_metadata") {
            self.data_dir.clone()
        } else {
            // Use cached directory (~/.config/winetricks/) - it contains category subdirectories
            let cached_dir = self.cached_verbs_dir();
            // Check if any category directories exist in cache
            if cached_dir.exists() {
                // Check if it has category subdirectories (dlls/, apps/, etc.)
                let has_categories = std::fs::read_dir(&cached_dir)
                    .ok()
                    .map(|entries| {
                        entries
                            .filter_map(|e| e.ok())
                            .any(|e| e.path().is_dir())
                    })
                    .unwrap_or(false);
                
                if has_categories {
                    cached_dir
                } else if self.data_dir.ends_with("files") {
                    // Fallback to files/json if cache doesn't have categories yet
                    self.data_dir.join("json")
                } else {
                    cached_dir // Return it anyway, will be empty but that's okay
                }
            } else if self.data_dir.ends_with("files") {
                // Fallback to files/json if cache doesn't exist yet
                self.data_dir.join("json")
            } else {
                // System data directory fallback
                let verbs_dir = self.data_dir.join("verbs");
                if verbs_dir.exists() {
                    verbs_dir
                } else {
                    let verbs_meta = self.data_dir.join("verbs_metadata");
                    if verbs_meta.exists() {
                        verbs_meta
                    } else {
                        verbs_dir
                    }
                }
            }
        }
    }

    /// Ensure directories exist
    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.cache_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.prefixes_root)?;
        std::fs::create_dir_all(self.metadata_dir())?;
        Ok(())
    }

    /// Set D3D renderer in wineprefix registry (persistent setting)
    pub fn set_renderer_in_registry(&self, renderer: Option<&str>) -> Result<()> {
        use std::fs;
        use std::io::Write;
        use std::process::Command;
        use crate::Wine;

        let wineprefix = self.wineprefix();
        
        // Create temp directory for registry file
        let temp_dir = dirs::cache_dir()
            .ok_or_else(|| WinetricksError::Config("Could not determine cache directory".into()))?
            .join("winetricks");
        fs::create_dir_all(&temp_dir)?;

        let reg_file = temp_dir.join("set_renderer.reg");
        let wine = Wine::detect()?;

        // Convert renderer to Wine format
        let renderer_value = match renderer {
            Some(r) => match r.to_lowercase().as_str() {
                "opengl" | "gl" | "w" => "gl",
                "vulkan" | "vk" | "v" => "vulkan",
                "gdi" => "gdi",
                "no3d" => "no3d",
                _ => r,
            },
            None => {
                // If None, remove the setting (set to empty string or remove key)
                // For now, we'll just return success without writing
                return Ok(());
            }
        };

        // Create registry file
        let reg_content = format!(
            r#"REGEDIT4

[HKEY_CURRENT_USER\Software\Wine\Direct3D]
"renderer"="{}"
"#,
            renderer_value
        );

        let mut file = fs::File::create(&reg_file)?;
        file.write_all(reg_content.as_bytes())?;
        file.sync_all()?;

        // Convert Unix path to Wine Windows path
        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        let reg_file_str = reg_file.to_string_lossy().to_string();
        
        // Get Windows path for the reg file
        let output = Command::new(&wine.wine_bin)
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
        let status = Command::new(&wine.wine_bin)
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
                "Failed to set renderer in registry (exit code: {:?})",
                status.code()
            )));
        }

        Ok(())
    }

    /// Get D3D renderer from wineprefix registry
    pub fn get_renderer_from_registry(&self) -> Option<String> {
        use std::process::Command;
        use crate::Wine;

        let wineprefix = self.wineprefix();
        let wine = match Wine::detect() {
            Ok(w) => w,
            Err(_) => return None,
        };

        let wineprefix_str = wineprefix.to_string_lossy().to_string();

        // Query registry using wine reg query
        // HKEY_CURRENT_USER\Software\Wine\Direct3D -> renderer value
        let output = Command::new(&wine.wine_bin)
            .arg("reg")
            .arg("query")
            .arg("HKEY_CURRENT_USER\\Software\\Wine\\Direct3D")
            .arg("/v")
            .arg("renderer")
            .env("WINEPREFIX", &wineprefix_str)
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        // Parse output: should contain "renderer" REG_SZ "value"
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.contains("renderer") && line.contains("REG_SZ") {
                // Extract value between quotes or after last whitespace
                // Format: "    renderer    REG_SZ    gl"
                let parts: Vec<&str> = line.split_whitespace().collect();
                if let Some(value) = parts.last() {
                    let value = value.trim().trim_matches('"');
                    if !value.is_empty() {
                        // Convert Wine format back to user-friendly format
                        match value.to_lowercase().as_str() {
                            "gl" => return Some("opengl".to_string()),
                            "vulkan" | "vk" | "v" => return Some("vulkan".to_string()),
                            "gdi" => return Some("gdi".to_string()),
                            "no3d" => return Some("no3d".to_string()),
                            _ => return Some(value.to_string()),
                        }
                    }
                }
            }
        }

        None
    }

    /// Load renderer setting from wineprefix (registry) if available
    pub fn load_renderer_from_prefix(&mut self) {
        if let Some(renderer) = self.get_renderer_from_registry() {
            self.renderer = Some(renderer);
        }
    }

    /// Get Graphics driver from wineprefix registry
    pub fn get_wayland_from_registry(&self) -> Option<String> {
        use std::process::Command;
        use crate::Wine;

        let wineprefix = self.wineprefix();
        let wine = match Wine::detect() {
            Ok(w) => w,
            Err(_) => return None,
        };

        let wineprefix_str = wineprefix.to_string_lossy().to_string();

        // Query registry for Graphics driver
        let output = Command::new(&wine.wine_bin)
            .arg("reg")
            .arg("query")
            .arg("HKEY_CURRENT_USER\\Software\\Wine\\Drivers")
            .arg("/v")
            .arg("Graphics")
            .env("WINEPREFIX", &wineprefix_str)
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        // Parse output
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if line.contains("Graphics") && line.contains("REG_SZ") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if let Some(value) = parts.last() {
                    let value = value.trim().trim_matches('"').to_lowercase();
                    if !value.is_empty() {
                        match value.as_str() {
                            "wayland" => return Some("wayland".to_string()),
                            "x11" | "xwayland" => return Some("xwayland".to_string()),
                            _ => return Some(value),
                        }
                    }
                }
            }
        }

        None
    }

    /// Detect current display server (Wayland or XWayland)
    pub fn detect_display_server(&self) -> Option<String> {
        // Check environment variables to detect current display server
        let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
        let display = std::env::var("DISPLAY").ok();

        if wayland_display.is_some() && (display.is_none() || display.as_deref() == Some("")) {
            // Wayland is available and DISPLAY is unset/empty - likely using Wayland
            return Some("wayland".to_string());
        } else if display.is_some() && !display.as_ref().unwrap().is_empty() {
            // DISPLAY is set - using X11/XWayland
            return Some("xwayland".to_string());
        }

        None
    }

    /// Load wayland setting from wineprefix (registry) if available
    /// Does NOT fall back to environment detection (preserves Auto setting)
    pub fn load_wayland_from_prefix(&mut self) {
        // First try registry
        if let Some(wayland) = self.get_wayland_from_registry() {
            self.wayland = Some(wayland);
            return;
        }
        
        // If no registry setting, clear it (don't use environment as fallback)
        // This allows Auto to remain as Auto when user selects it
        self.wayland = None;
    }
    
    /// Load wayland setting with environment fallback (for initial detection)
    pub fn load_wayland_from_prefix_with_env(&mut self) {
        // First try registry
        if let Some(wayland) = self.get_wayland_from_registry() {
            self.wayland = Some(wayland);
            return;
        }
        
        // Fallback to detection from environment (for initial load only)
        if let Some(display_server) = self.detect_display_server() {
            self.wayland = Some(display_server);
        }
    }

    /// Set Graphics driver in wineprefix registry (persistent setting)
    pub fn set_wayland_in_registry(&self, wayland: Option<&str>) -> Result<()> {
        use std::fs;
        use std::io::Write;
        use std::process::Command;
        use crate::Wine;

        let wineprefix = self.wineprefix();
        
        // Create temp directory for registry file
        let temp_dir = dirs::cache_dir()
            .ok_or_else(|| WinetricksError::Config("Could not determine cache directory".into()))?
            .join("winetricks");
        fs::create_dir_all(&temp_dir)?;

        let reg_file = temp_dir.join("set_wayland.reg");
        let wine = Wine::detect()?;

        let wineprefix_str = wineprefix.to_string_lossy().to_string();
        
        // Handle Auto - delete the registry key to let Wine decide
        if wayland.is_none() {
            // Delete the Graphics key using wine reg delete
            let status = Command::new(&wine.wine_bin)
                .arg("reg")
                .arg("delete")
                .arg("HKEY_CURRENT_USER\\Software\\Wine\\Drivers")
                .arg("/v")
                .arg("Graphics")
                .arg("/f") // Force delete
                .env("WINEPREFIX", &wineprefix_str)
                .status()
                .map_err(|e| WinetricksError::CommandExecution {
                    command: format!("wine reg delete HKEY_CURRENT_USER\\Software\\Wine\\Drivers /v Graphics /f"),
                    error: e.to_string(),
                })?;

            // It's OK if the key doesn't exist (exit code 1)
            if !status.success() && status.code() != Some(1) {
                return Err(WinetricksError::Config(format!(
                    "Failed to delete Graphics driver from registry (exit code: {:?})",
                    status.code()
                )));
            }

            return Ok(());
        }

        // Convert wayland option to Wine format
        let graphics_value = match wayland {
            Some("wayland") => "wayland",
            Some("xwayland") | Some("x11") => "x11",
            _ => {
                return Err(WinetricksError::Config(
                    format!("Invalid wayland value: {}", wayland.unwrap_or("None"))
                ));
            }
        };

        // Create registry file
        let reg_content = format!(
            r#"REGEDIT4

[HKEY_CURRENT_USER\Software\Wine\Drivers]
"Graphics"="{}"
"#,
            graphics_value
        );

        let mut file = fs::File::create(&reg_file)?;
        file.write_all(reg_content.as_bytes())?;
        file.sync_all()?;

        // Convert Unix path to Wine Windows path
        let reg_file_str = reg_file.to_string_lossy().to_string();
        
        // Get Windows path for the reg file
        let output = Command::new(&wine.wine_bin)
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
        let status = Command::new(&wine.wine_bin)
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
                "Failed to set Graphics driver in registry (exit code: {:?})",
                status.code()
            )));
        }

        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new().unwrap()
    }
}
