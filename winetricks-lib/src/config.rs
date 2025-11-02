//! Configuration management for winetricks

use crate::error::{Result, WinetricksError};
use dirs;
use std::path::PathBuf;

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

    /// Get metadata directory
    pub fn metadata_dir(&self) -> PathBuf {
        self.data_dir.join("verbs")
    }

    /// Ensure directories exist
    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.cache_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.prefixes_root)?;
        std::fs::create_dir_all(self.metadata_dir())?;
        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new().unwrap()
    }
}
