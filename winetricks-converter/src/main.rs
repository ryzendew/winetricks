//! Converter tool to extract verb definitions from original winetricks script

use anyhow::{Context, Result};
use regex::Regex;
use std::fs;
use std::path::PathBuf;
use clap::Parser;
use serde_json;
use winetricks_lib::{VerbCategory, VerbMetadata, VerbFile, MediaType};

#[derive(Parser)]
#[command(name = "winetricks-converter")]
#[command(about = "Convert original winetricks script to JSON metadata")]
struct Cli {
    /// Input winetricks script file
    #[arg(short, long)]
    input: PathBuf,
    
    /// Output directory for JSON metadata files
    #[arg(short, long, default_value = "verbs")]
    output: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    
    println!("Reading winetricks script: {:?}", cli.input);
    let content = fs::read_to_string(&cli.input)
        .with_context(|| format!("Failed to read {:?}", cli.input))?;
    
    // Create output directories for each category
    let categories = ["apps", "dlls", "fonts", "settings", "benchmarks", "download", "manual-download"];
    for cat in &categories {
        fs::create_dir_all(cli.output.join(cat))?;
    }
    
    // Pattern to match w_metadata calls
    let metadata_re = Regex::new(r"(?m)^w_metadata\s+(\w+)\s+(\w+)\s+\\?$")?;
    
    // Find all metadata declarations
    let mut verb_name = String::new();
    let mut category = String::new();
    let mut in_metadata = false;
    let mut metadata_lines = Vec::new();
    let mut verbs = Vec::new();
    
    for line in content.lines() {
        // Check if this is a metadata declaration start
        if let Some(caps) = metadata_re.captures(line) {
            // Save previous verb if exists
            if !verb_name.is_empty() {
                if let Ok(verb) = parse_metadata(&verb_name, &category, &metadata_lines) {
                    verbs.push(verb);
                }
            }
            
            verb_name = caps[1].to_string();
            category = caps[2].to_string();
            metadata_lines.clear();
            in_metadata = true;
            
            // Get continuation lines
            let trimmed = line.trim();
            if trimmed.ends_with('\\') {
                continue;
            } else {
                // Single line metadata (unlikely but possible)
                in_metadata = false;
            }
        } else if in_metadata {
            metadata_lines.push(line.to_string());
            if !line.trim().ends_with('\\') {
                in_metadata = false;
            }
        }
    }
    
    // Save last verb
    if !verb_name.is_empty() {
        if let Ok(verb) = parse_metadata(&verb_name, &category, &metadata_lines) {
            verbs.push(verb);
        }
    }
    
    println!("Found {} verbs", verbs.len());
    
    // Write JSON files
    for verb in verbs {
        let cat_dir = cli.output.join(verb.category.as_str());
        let json_file = cat_dir.join(format!("{}.json", verb.name));
        
        let json = serde_json::to_string_pretty(&verb)?;
        fs::write(&json_file, json)
            .with_context(|| format!("Failed to write {:?}", json_file))?;
        
        println!("Wrote: {:?}", json_file);
    }
    
    println!("Conversion complete!");
    Ok(())
}

fn parse_metadata(name: &str, cat: &str, lines: &[String]) -> Result<VerbMetadata> {
    let category = VerbCategory::from_str(cat)
        .ok_or_else(|| anyhow::anyhow!("Unknown category: {}", cat))?;
    
    let mut title = name.to_string();
    let mut publisher = None;
    let mut year = None;
    let mut media = MediaType::Download;
    let mut files = Vec::new();
    let mut installed_file = None;
    let mut installed_exe = None;
    let mut conflicts = Vec::new();
    
    // Parse metadata lines
    for line in lines {
        let line = line.trim();
        if line.is_empty() || line == "\\" {
            continue;
        }
        
        // Remove trailing backslash
        let line = line.trim_end_matches('\\').trim();
        
        if line.starts_with("title=") {
            title = extract_value(line);
        } else if line.starts_with("publisher=") {
            publisher = Some(extract_value(line));
        } else if line.starts_with("year=") {
            year = Some(extract_value(line));
        } else if line.starts_with("media=") {
            let media_val = extract_value(line);
            media = match media_val.as_str() {
                "download" => MediaType::Download,
                "manual_download" => MediaType::ManualDownload,
                _ => MediaType::Download,
            };
        } else if line.starts_with("file1=") {
            let filename = extract_value(line);
            files.push(VerbFile {
                filename,
                url: None, // Will be extracted from load function
                sha256: None, // Will be extracted from load function
            });
        } else if line.starts_with("installed_file1=") {
            installed_file = Some(extract_value(line));
        } else if line.starts_with("installed_exe1=") {
            installed_exe = Some(extract_value(line));
        } else if line.starts_with("conflicts=") {
            let conflicts_str = extract_value(line);
            conflicts = conflicts_str.split_whitespace().map(|s| s.to_string()).collect();
        }
    }
    
    Ok(VerbMetadata {
        name: name.to_string(),
        category,
        title,
        publisher,
        year,
        media,
        files,
        installed_file,
        installed_exe,
        conflicts,
    })
}

fn extract_value(line: &str) -> String {
    // Extract value from key="value" or key=value
    if let Some(eq_pos) = line.find('=') {
        let value = &line[eq_pos + 1..];
        // Remove quotes if present
        value.trim_matches('"').trim_matches('\'').to_string()
    } else {
        String::new()
    }
}

