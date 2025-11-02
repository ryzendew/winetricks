//! Wine interface for detecting and managing Wine installations

use crate::error::{Result, WinetricksError};
use std::path::PathBuf;
use std::process::Command;
use which::which;

/// Wine installation and version information
#[derive(Debug, Clone)]
pub struct Wine {
    /// Path to wine binary
    pub wine_bin: PathBuf,

    /// Path to wineserver binary
    pub wineserver_bin: PathBuf,

    /// Wine version string
    pub version: String,

    /// Stripped version (e.g., "8.0" from "wine-8.0")
    pub version_stripped: String,

    /// Architecture (win32 or win64)
    pub arch: String,
}

impl Wine {
    /// Detect Wine installation
    /// Checks for custom Wine in WINEPREFIX first, then falls back to PATH
    pub fn detect() -> Result<Self> {
        // Check if WINEPREFIX has a custom Wine installation
        // Common locations: WINEPREFIX/bin/wine, WINEPREFIX/wine/bin/wine, WINEPREFIX/../ElementalWarriorWine/bin/wine
        let wineprefix = Self::get_wineprefix();
        let mut wine_bin = None;
        let mut wineserver_bin = None;
        
        // Try common custom Wine locations
        let mut custom_wine_paths = vec![
            wineprefix.join("bin").join("wine"),
            wineprefix.join("wine").join("bin").join("wine"),
        ];
        
        // Add parent directory paths if they exist
        if let Some(parent) = wineprefix.parent() {
            custom_wine_paths.push(parent.join("ElementalWarriorWine").join("bin").join("wine"));
            custom_wine_paths.push(parent.join("wine").join("bin").join("wine"));
        }
        
        for wine_path in &custom_wine_paths {
            if wine_path.exists() && wine_path.is_file() {
                let potential_wineserver = wine_path.parent()
                    .and_then(|p| Some(p.join("wineserver")));
                
                if let Some(ws_path) = potential_wineserver {
                    if ws_path.exists() {
                        wine_bin = Some(wine_path.clone());
                        wineserver_bin = Some(ws_path);
                        break;
                    }
                }
            }
        }
        
        // Fall back to PATH if no custom Wine found
        let wine_bin = match wine_bin {
            Some(bin) => bin,
            None => which("wine")
                .map_err(|_| WinetricksError::Wine("wine binary not found in PATH".into()))?,
        };

        let wineserver_bin = match wineserver_bin {
            Some(bin) => bin,
            None => which("wineserver")
                .map_err(|_| WinetricksError::Wine("wineserver binary not found in PATH".into()))?,
        };

        let version = Self::get_version(&wine_bin)?;
        let version_stripped = Self::strip_version(&version);

        // Detect architecture by checking if wineserver is 64-bit
        // This is a simplified check - real winetricks does more complex detection
        // For now, default to win32 (will be improved later)
        let arch = "win32".to_string();

        Ok(Self {
            wine_bin,
            wineserver_bin,
            version,
            version_stripped,
            arch,
        })
    }

    /// Get wine version
    fn get_version(wine_bin: &PathBuf) -> Result<String> {
        let output = Command::new(wine_bin)
            .arg("--version")
            .output()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("{:?} --version", wine_bin),
                error: e.to_string(),
            })?;

        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();

        if version.is_empty() {
            return Err(WinetricksError::Wine(
                "wine --version returned empty".into(),
            ));
        }

        Ok(version)
    }

    /// Strip version string to just the number
    /// e.g., "wine-8.0" -> "8.0"
    fn strip_version(version: &str) -> String {
        version
            .replace("wine-", "")
            .split_whitespace()
            .next()
            .unwrap_or(version)
            .split("-rc")
            .next()
            .unwrap_or(version)
            .to_string()
    }

    /// Check if wine version is >= specified version
    pub fn version_ge(&self, version: &str) -> Result<bool> {
        // Simple comparison - could be enhanced
        Ok(self.version_stripped.as_str() >= version)
    }

    /// Execute a wine command
    pub fn exec(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = Command::new(&self.wine_bin)
            .args(args)
            .output()
            .map_err(|e| WinetricksError::CommandExecution {
                command: format!("{:?} {:?}", self.wine_bin, args),
                error: e.to_string(),
            })?;

        if !output.status.success() {
            return Err(WinetricksError::Wine(format!(
                "wine command failed: {:?}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        Ok(output)
    }

    /// Get wine prefix path
    pub fn get_wineprefix() -> PathBuf {
        std::env::var("WINEPREFIX")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs::home_dir().unwrap().join(".wine"))
    }
}
