//! Download system with caching and checksum verification

use crate::error::{Result, WinetricksError};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Download manager
pub struct DownloadManager {
    client: Client,
    cache_dir: PathBuf,
}

impl DownloadManager {
    /// Create a new download manager
    pub fn new(cache_dir: PathBuf) -> Result<Self> {
        let client = Client::builder()
            .user_agent("Winetricks/1.0")
            .build()?;
        
        std::fs::create_dir_all(&cache_dir)?;
        
        Ok(Self {
            client,
            cache_dir,
        })
    }
    
    /// Download a file to cache
    pub async fn download<P: AsRef<Path>>(
        &self,
        url: &str,
        filename: P,
        expected_sha256: Option<&str>,
        progress: bool,
    ) -> Result<PathBuf> {
        let filename = filename.as_ref();
        let cache_file = self.cache_dir.join(filename);
        
        // Check if already cached
        if cache_file.exists() {
            if let Some(expected) = expected_sha256 {
                if self.verify_checksum(&cache_file, expected)? {
                    return Ok(cache_file);
                }
                // Checksum mismatch - re-download
                std::fs::remove_file(&cache_file)?;
            } else {
                return Ok(cache_file);
            }
        }
        
        // Download file
        let mut response = self.client
            .get(url)
            .send()
            .await?;
        
        let total_size = response.content_length().unwrap_or(0);
        
        let pb = if progress && total_size > 0 {
            let pb = ProgressBar::new(total_size);
            let style = ProgressStyle::default_bar()
                .template("{msg} {bar:40.cyan/blue} {bytes}/{total_bytes} {eta}")
                .map_err(|e| WinetricksError::Download(format!("Progress bar template error: {}", e)))?;
            pb.set_style(style);
            pb.set_message("Downloading");
            Some(pb)
        } else {
            None
        };
        
        let mut file = std::fs::File::create(&cache_file)?;
        let mut hasher = Sha256::new();
        
        while let Some(chunk) = response.chunk().await? {
            file.write_all(&chunk)?;
            hasher.update(&chunk);
            
            if let Some(ref pb) = pb {
                pb.inc(chunk.len() as u64);
            }
        }
        
        if let Some(pb) = pb {
            pb.finish_with_message("Downloaded");
        }
        
        // Verify checksum
        if let Some(expected) = expected_sha256 {
            let computed = format!("{:x}", hasher.finalize());
            if computed != expected {
                std::fs::remove_file(&cache_file)?;
                return Err(WinetricksError::ChecksumMismatch {
                    expected: expected.to_string(),
                    got: computed,
                });
            }
        }
        
        Ok(cache_file)
    }
    
    /// Verify SHA256 checksum
    pub fn verify_checksum<P: AsRef<Path>>(&self, path: P, expected: &str) -> Result<bool> {
        let mut hasher = Sha256::new();
        let mut file = std::fs::File::open(path)?;
        std::io::copy(&mut file, &mut hasher)?;
        
        let computed = format!("{:x}", hasher.finalize());
        Ok(computed == expected)
    }
    
    /// Check if file is cached
    pub fn is_cached<P: AsRef<Path>>(&self, filename: P) -> bool {
        self.cache_dir.join(filename).exists()
    }
    
    /// Get cached file path
    pub fn get_cached_path<P: AsRef<Path>>(&self, filename: P) -> PathBuf {
        self.cache_dir.join(filename)
    }
}

