//! Verb system for winetricks packages

use crate::error::{Result, WinetricksError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

/// Verb categories
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VerbCategory {
    #[serde(rename = "apps")]
    Apps,
    #[serde(rename = "dlls")]
    Dlls,
    #[serde(rename = "fonts")]
    Fonts,
    #[serde(rename = "settings")]
    Settings,
    #[serde(rename = "benchmarks")]
    Benchmarks,
    #[serde(rename = "download")]
    Download,
    #[serde(rename = "manual-download")]
    ManualDownload,
}

impl VerbCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            VerbCategory::Apps => "apps",
            VerbCategory::Dlls => "dlls",
            VerbCategory::Fonts => "fonts",
            VerbCategory::Settings => "settings",
            VerbCategory::Benchmarks => "benchmarks",
            VerbCategory::Download => "download",
            VerbCategory::ManualDownload => "manual-download",
        }
    }
}

impl FromStr for VerbCategory {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "apps" => Ok(VerbCategory::Apps),
            "dlls" => Ok(VerbCategory::Dlls),
            "fonts" => Ok(VerbCategory::Fonts),
            "settings" => Ok(VerbCategory::Settings),
            "benchmarks" => Ok(VerbCategory::Benchmarks),
            "download" => Ok(VerbCategory::Download),
            "manual-download" => Ok(VerbCategory::ManualDownload),
            _ => Err(format!("Unknown category: {}", s)),
        }
    }
}

/// Media type for verb
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MediaType {
    #[default]
    #[serde(rename = "download")]
    Download,
    #[serde(rename = "manual_download")]
    ManualDownload,
}

/// Verb metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerbMetadata {
    /// Verb name/code
    pub name: String,

    /// Category
    pub category: VerbCategory,

    /// Display title
    pub title: String,

    /// Publisher name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,

    /// Release year
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<String>,

    /// Media type
    #[serde(default)]
    pub media: MediaType,

    /// Download files
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<VerbFile>,

    /// Installed file to check (Windows path)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_file: Option<String>,

    /// Installed executable to check (Windows path)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_exe: Option<String>,

    /// Conflicting verbs
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<String>,
}

/// File to download
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerbFile {
    /// Filename
    pub filename: String,

    /// URL (if downloadable)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// SHA256 checksum
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// Verb registry
#[derive(Debug, Default)]
pub struct VerbRegistry {
    /// Map of verb name to metadata
    verbs: HashMap<String, VerbMetadata>,

    /// Index by category
    by_category: HashMap<VerbCategory, Vec<String>>,
}

impl VerbRegistry {
    /// Create a new verb registry
    pub fn new() -> Self {
        Self::default()
    }

    /// Load verbs from metadata directory
    pub fn load_from_dir<P: AsRef<Path>>(dir: P) -> Result<Self> {
        let mut registry = Self::new();
        let dir = dir.as_ref();

        // Scan all category directories
        for category_dir in std::fs::read_dir(dir)? {
            let category_dir = category_dir?;
            let path = category_dir.path();

            if !path.is_dir() {
                continue;
            }

            let category_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| WinetricksError::Verb("Invalid category directory name".into()))?;

            let category = VerbCategory::from_str(category_name).map_err(WinetricksError::Verb)?;

            // Load all JSON files in category directory
            for entry in std::fs::read_dir(&path)? {
                let entry = entry?;
                let file_path = entry.path();

                if file_path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }

                let verb_name = file_path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| WinetricksError::Verb("Invalid verb filename".into()))?;

                let metadata = Self::load_verb_metadata(&file_path)?;
                registry.register(verb_name.to_string(), metadata, category)?;
            }
        }

        Ok(registry)
    }

    /// Load verb metadata from JSON file
    fn load_verb_metadata<P: AsRef<Path>>(path: P) -> Result<VerbMetadata> {
        let content = std::fs::read_to_string(path.as_ref())?;
        let mut metadata: VerbMetadata = serde_json::from_str(&content)?;

        // Ensure name matches filename
        let name = path
            .as_ref()
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        metadata.name = name.to_string();

        Ok(metadata)
    }

    /// Register a verb
    pub fn register(
        &mut self,
        name: String,
        metadata: VerbMetadata,
        category: VerbCategory,
    ) -> Result<()> {
        if self.verbs.contains_key(&name) {
            return Err(WinetricksError::Verb(format!(
                "Verb '{}' already registered",
                name
            )));
        }

        let mut metadata = metadata;
        metadata.name = name.clone();
        metadata.category = category;

        self.verbs.insert(name.clone(), metadata);
        self.by_category.entry(category).or_default().push(name);

        Ok(())
    }

    /// Get verb metadata
    pub fn get(&self, name: &str) -> Option<&VerbMetadata> {
        self.verbs.get(name)
    }

    /// List all verbs
    pub fn list(&self) -> Vec<&VerbMetadata> {
        self.verbs.values().collect()
    }

    /// List verbs by category
    pub fn list_by_category(&self, category: VerbCategory) -> Vec<&VerbMetadata> {
        self.by_category
            .get(&category)
            .map(|names| {
                names
                    .iter()
                    .filter_map(|name| self.verbs.get(name))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Check if verb exists
    pub fn exists(&self, name: &str) -> bool {
        self.verbs.contains_key(name)
    }
}

/// Verb executor (placeholder for now)
#[derive(Debug)]
pub struct Verb {
    pub metadata: VerbMetadata,
}

impl Verb {
    pub fn new(metadata: VerbMetadata) -> Self {
        Self { metadata }
    }
}
