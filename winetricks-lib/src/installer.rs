//! Installer type detection and silent switch generation
//!
//! This module provides functionality to detect installer types (NSIS, Inno Setup,
//! InstallShield, MSI, etc.) and generate appropriate silent installation switches.

use std::path::Path;

/// Installer type detection result
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallerType {
    /// NSIS (Nullsoft Scriptable Install System)
    NSIS,
    /// Inno Setup
    InnoSetup,
    /// InstallShield
    InstallShield,
    /// MSI wrapper/bootstrapper (EXE that wraps MSI)
    MsiBootstrapper,
    /// .NET Framework installer
    DotNet,
    /// Visual C++ Redistributable
    VcRedist,
    /// Generic/unknown installer
    Generic,
}

/// Detect installer type from filename and file content hints
pub fn detect_installer_type(filename: &str, verb_name: &str) -> InstallerType {
    // Check for known .NET Framework installers
    if filename.contains("dotnet")
        || filename.contains("ndp")
        || filename.starts_with("NDP")
        || verb_name.contains("dotnet")
    {
        return InstallerType::DotNet;
    }

    // Check for Visual C++ Redistributables
    if filename.contains("vcredist")
        || filename.contains("vc_redist")
        || verb_name.starts_with("vcrun20")
        || verb_name.starts_with("ucrtbase")
    {
        return InstallerType::VcRedist;
    }

    // Check filename patterns for common installer types
    let filename_lower = filename.to_lowercase();

    // NSIS installers often have "setup" or "install" in name
    // Could also check file strings, but filename pattern is a start
    if filename_lower.contains("nsis") || filename_lower.contains("nullsoft") {
        return InstallerType::NSIS;
    }

    // Known NSIS installers (7zip, etc.)
    if filename_lower.starts_with("7z") && filename_lower.ends_with(".exe") {
        return InstallerType::NSIS;
    }

    // Inno Setup installers
    // Many Inno Setup installers are named "Setup.exe" or have "setup" in the name
    // Try to detect by filename pattern first - files ending in "-Setup.exe" or just "Setup.exe" are often Inno Setup
    if filename_lower.contains("innosetup") || filename_lower.contains("inno") {
        return InstallerType::InnoSetup;
    }

    // Files named "Setup.exe" or ending in "-Setup.exe" are commonly Inno Setup
    // Check this before generic fallback
    if filename_lower == "setup.exe"
        || filename_lower.ends_with("-setup.exe")
        || filename_lower.ends_with("_setup.exe")
    {
        // This might be Inno Setup, but we'll let file-based detection confirm
        // For now, we'll still return Generic but prioritize Inno Setup switches in generic handler
        // Actually, let's be more aggressive - many Setup.exe files are Inno Setup
        return InstallerType::InnoSetup;
    }

    // InstallShield
    if filename_lower.contains("installshield") {
        return InstallerType::InstallShield;
    }

    // MSI bootstrappers (EXE that wraps MSI)
    // Often have "setup" or "installer" in name, but are EXE files
    // This is detected by process of elimination in the executor

    InstallerType::Generic
}

/// Get silent installation switches for an installer type
pub fn get_silent_switches(installer_type: InstallerType, unattended: bool) -> Vec<String> {
    if !unattended {
        return vec![];
    }

    match installer_type {
        InstallerType::NSIS => {
            vec!["/S".to_string()]
        }
        InstallerType::InnoSetup => {
            vec![
                "/VERYSILENT".to_string(),
                "/NORESTART".to_string(),
                "/SP-".to_string(),
            ]
        }
        InstallerType::InstallShield => {
            vec!["/s".to_string()]
        }
        InstallerType::MsiBootstrapper => {
            vec!["/quiet".to_string(), "/norestart".to_string()]
        }
        InstallerType::DotNet => {
            // .NET installers have version-specific handling in executor
            // Default fallback
            vec!["/q".to_string(), "/norestart".to_string()]
        }
        InstallerType::VcRedist => {
            vec!["/q".to_string()]
        }
        InstallerType::Generic => {
            // For generic installers, try common switches
            // Most Windows installers accept /q or /quiet
            // We'll use /q as it's more universal than /S or /VERYSILENT
            vec!["/q".to_string()]
        }
    }
}

/// Get MSI silent switch
pub fn get_msi_silent_switch(unattended: bool) -> Option<String> {
    if unattended {
        // Use /qn for explicit "no UI" (more standard than /q)
        Some("/qn".to_string())
    } else {
        None
    }
}

/// Detect installer type from file (attempts to read file headers/strings)
pub fn detect_from_file(file_path: &Path) -> Option<InstallerType> {
    use std::fs::File;
    use std::io::Read;

    // Read file to check for installer signatures
    // Try reading first 32KB - installer signatures can appear in various places
    if let Ok(mut file) = File::open(file_path) {
        let mut buffer = vec![0u8; 32768]; // 32KB buffer

        // Try to read up to 32KB
        match file.read(&mut buffer) {
            Ok(bytes_read) if bytes_read > 0 => {
                // Only use what we actually read
                let buffer = &buffer[..bytes_read];
                let content = String::from_utf8_lossy(buffer);
                let content_lower = content.to_lowercase();

                // Check for NSIS signatures
                if content_lower.contains("nullsoft") || content_lower.contains("nsis") {
                    return Some(InstallerType::NSIS);
                }

                // Check for Inno Setup signatures
                if content_lower.contains("inno setup") || content_lower.contains("innosetup") {
                    return Some(InstallerType::InnoSetup);
                }

                // Check for InstallShield signatures
                if content_lower.contains("installshield") {
                    return Some(InstallerType::InstallShield);
                }
            }
            _ => {}
        }
    }

    None
}
