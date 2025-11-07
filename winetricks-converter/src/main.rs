//! Converter tool to extract verb definitions from original winetricks script

use anyhow::{Context, Result};
use clap::Parser;
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use winetricks_lib::{MediaType, VerbCategory, VerbFile, VerbMetadata};

#[derive(Parser)]
#[command(name = "winetricks-converter")]
#[command(about = "Convert original winetricks script to JSON metadata")]
struct Cli {
    /// Input winetricks script file
    #[arg(short, long)]
    input: PathBuf,

    /// Output directory for JSON metadata files
    #[arg(short, long, default_value = "files/json")]
    output: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    println!("Reading winetricks script: {:?}", cli.input);
    let content = fs::read_to_string(&cli.input)
        .with_context(|| format!("Failed to read {:?}", cli.input))?;

    // First, parse all load_* functions to extract download URLs and SHA256 hashes
    println!("Extracting download URLs and SHA256 hashes from load_* functions...");
    let downloads = extract_downloads(&content)?;
    println!("Found {} download entries", downloads.len());

    // Create output directories for each category
    let categories = [
        "apps",
        "dlls",
        "fonts",
        "settings",
        "benchmarks",
        "download",
        "manual-download",
    ];
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
                if let Ok(mut verb) = parse_metadata(&verb_name, &category, &metadata_lines) {
                    // Enrich with download URLs from load_* functions
                    enrich_with_downloads(&mut verb, &downloads);
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
        if let Ok(mut verb) = parse_metadata(&verb_name, &category, &metadata_lines) {
            // Enrich with download URLs from load_* functions
            enrich_with_downloads(&mut verb, &downloads);
            verbs.push(verb);
        }
    }

    println!("Found {} verbs", verbs.len());

    // Write JSON files
    for verb in verbs {
        let cat_dir = cli.output.join(verb.category.as_str());
        let json_file = cat_dir.join(format!("{}.json", verb.name));

        let json = serde_json::to_string_pretty(&verb)?;
        fs::write(&json_file, json).with_context(|| format!("Failed to write {:?}", json_file))?;

        println!("Wrote: {:?}", json_file);
    }

    println!("Conversion complete!");
    Ok(())
}

fn parse_metadata(name: &str, cat: &str, lines: &[String]) -> Result<VerbMetadata> {
    let category = VerbCategory::from_str(cat)
        .map_err(|e| anyhow::anyhow!("Unknown category: {} ({})", cat, e))?;

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
        } else if line.starts_with("file") && line.contains("=") {
            // Handle file1=, file2=, file3=, etc.
            let filename = extract_value(line);
            files.push(VerbFile {
                filename,
                url: None,    // Will be extracted from load function
                sha256: None, // Will be extracted from load function
            });
        } else if line.starts_with("installed_file") && line.contains("=") {
            // Handle installed_file1=, installed_file2=, etc. (use first one)
            if installed_file.is_none() {
                installed_file = Some(extract_value(line));
            }
        } else if line.starts_with("installed_exe") && line.contains("=") {
            // Handle installed_exe1=, installed_exe2=, etc. (use first one)
            if installed_exe.is_none() {
                installed_exe = Some(extract_value(line));
            }
        } else if line.starts_with("conflicts=") {
            let conflicts_str = extract_value(line);
            conflicts = conflicts_str
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
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

/// Extract download URLs and SHA256 hashes from load_* functions
/// Returns a map of verb name to (filename, url, sha256)
fn extract_downloads(content: &str) -> Result<HashMap<String, Vec<(String, String, String)>>> {
    let mut downloads: HashMap<String, Vec<(String, String, String)>> = HashMap::new();

    // Pattern to match load_<verb_name>() function
    let load_func_re = Regex::new(r"^load_(\w+)\(\)")?;
    // Pattern to match w_download calls: w_download <url> <sha256>
    let w_download_re = Regex::new(r"^\s+w_download\s+(\S+)\s+(\S+)")?;
    // Pattern to match w_download_to calls: w_download_to <cache_dir> "<url>" <sha256>
    // We'll use a simpler regex and fallback to manual parsing
    let w_download_to_re = Regex::new(r"^\s+w_download_to")?;

    let lines: Vec<&str> = content.lines().collect();
    let mut current_verb: Option<String> = None;
    let mut in_function = false;
    let mut brace_depth = 0;

    for line in lines.iter() {
        // Check if this is a load_* function definition
        if let Some(caps) = load_func_re.captures(line) {
            current_verb = Some(caps[1].to_string());
            in_function = true;
            brace_depth = 0;

            // Count opening braces on this line
            brace_depth += line.matches('{').count();
            brace_depth -= line.matches('}').count();
            continue;
        }

        if let Some(ref verb_name) = current_verb {
            if in_function {
                // Count braces to track function scope
                brace_depth += line.matches('{').count();
                brace_depth -= line.matches('}').count();

                // Check for w_download calls
                if let Some(caps) = w_download_re.captures(line) {
                    let url = caps.get(1).unwrap().as_str().to_string();
                    let sha256 = caps.get(2).unwrap().as_str().to_string();

                    // Try to extract filename from URL or previous file1= assignment
                    let filename = extract_filename_from_url(&url);

                    downloads
                        .entry(verb_name.clone())
                        .or_insert_with(Vec::new)
                        .push((filename, url, sha256));
                }

                // Check for w_download_to calls (used by fonts: w_download_to corefonts "url" sha256)
                // Format: w_download_to <cache_dir> "<url>" <sha256>
                if w_download_to_re.is_match(line) {
                    // Parse manually: w_download_to corefonts "https://..." sha256
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 4 {
                        // parts[0] = "w_download_to"
                        // parts[1] = cache_dir (e.g., "corefonts")
                        // parts[2] = url (may be quoted)
                        // parts[3] = sha256
                        let mut url = parts[2].to_string();
                        url = url.trim_matches('"').trim_matches('\'').to_string();
                        let sha256 = parts[3].to_string();
                        let filename = extract_filename_from_url(&url);

                        downloads
                            .entry(verb_name.clone())
                            .or_insert_with(Vec::new)
                            .push((filename, url, sha256));
                    }
                }

                // Function ended
                if brace_depth <= 0 {
                    in_function = false;
                    current_verb = None;
                }
            }
        }
    }

    Ok(downloads)
}

/// Extract filename from URL or guess based on URL structure
fn extract_filename_from_url(url: &str) -> String {
    // Try to get filename from URL
    if let Some(last_slash) = url.rfind('/') {
        let filename_part = &url[last_slash + 1..];
        // Remove query parameters
        if let Some(qmark) = filename_part.find('?') {
            return filename_part[..qmark].to_string();
        }
        return filename_part.to_string();
    }
    // Fallback: use a placeholder that will be matched later
    "unknown".to_string()
}

/// Enrich verb metadata with download URLs and SHA256 hashes
fn enrich_with_downloads(
    verb: &mut VerbMetadata,
    downloads: &HashMap<String, Vec<(String, String, String)>>,
) {
    if let Some(download_list) = downloads.get(&verb.name) {
        // Match downloads to files based on filename
        for file in &mut verb.files {
            if file.url.is_none() {
                // Try to find matching download by filename
                for (filename, url, sha256) in download_list {
                    if filename == &file.filename
                        || file.filename.contains(filename)
                        || filename.contains(&file.filename)
                        || filename == "unknown"
                    {
                        file.url = Some(url.clone());
                        file.sha256 = Some(sha256.clone());
                        break;
                    }
                }

                // If still no match and we have downloads, use the first one
                if file.url.is_none() && !download_list.is_empty() {
                    let (filename, url, sha256) = &download_list[0];
                    if file.filename.is_empty() || file.filename == "unknown" {
                        file.filename = filename.clone();
                    }
                    file.url = Some(url.clone());
                    file.sha256 = Some(sha256.clone());
                }
            }
        }

        // If verb has no files defined but we have downloads, add them
        if verb.files.is_empty() {
            for (filename, url, sha256) in download_list {
                verb.files.push(VerbFile {
                    filename: filename.clone(),
                    url: Some(url.clone()),
                    sha256: Some(sha256.clone()),
                });
            }
        }
    }
}
