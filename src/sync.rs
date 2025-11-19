// Repository synchronization module
// Downloads and caches package databases from upstream sources

use eyre::Result;
use std::fs;
use std::path::PathBuf;
use indicatif::{ProgressBar, ProgressStyle};

pub struct RepoSync {
    cache_dir: PathBuf,
}

impl RepoSync {
    pub fn new() -> Result<Self> {
        let cache_dir = dirs::cache_dir()
            .ok_or_else(|| eyre::eyre!("Could not find cache directory"))?
            .join("pmux")
            .join("repos");
        
        fs::create_dir_all(&cache_dir)?;
        
        Ok(Self { cache_dir })
    }
    
    pub fn sync_all(&self, force: bool, enabled_pms: &[String]) -> Result<()> {
        println!(":: Synchronizing package databases...");
        
        if force {
            println!("   Force refresh enabled - re-downloading all databases");
        }
        
        // Only sync enabled PMs
        for pm_name in enabled_pms {
            match pm_name.as_str() {
                "pacman" => {
                    self.sync_pacman(force)?;
                }
                "paru" | "aur" => {
                    self.sync_aur(force)?;
                }
                "nix" => {
                    self.sync_nix(force)?;
                }
                "dnf" => {
                    self.sync_dnf(force)?;
                }
                "emerge" | "portage" => {
                    self.sync_portage(force)?;
                }
                "pkgit" => {
                    self.sync_pkgit(force)?;
                }
                _ => {
                    // Skip unknown PMs
                }
            }
        }
        
        println!(":: Package databases are up to date");
        println!(":: Run 'pmux' to browse packages");
        Ok(())
    }
    
    fn get_enabled_pacman_repos(&self) -> Vec<String> {
        // Read /etc/pacman.conf to find enabled repos
        let mut repos = Vec::new();
        
        if let Ok(content) = fs::read_to_string("/etc/pacman.conf") {
            for line in content.lines() {
                let line = line.trim();
                // Look for [reponame] sections that aren't commented
                if line.starts_with('[') && line.ends_with(']') && !line.starts_with('#') {
                    let repo = &line[1..line.len()-1];
                    // Skip options section
                    if repo != "options" {
                        repos.push(repo.to_string());
                    }
                }
            }
        }
        
        // Fallback to defaults if nothing found
        if repos.is_empty() {
            repos = vec!["core".to_string(), "extra".to_string(), "multilib".to_string()];
        }
        
        repos
    }
    
    fn sync_pacman(&self, force: bool) -> Result<()> {
        println!("  -> Syncing pacman repositories...");
        
        // Arch Linux mirror
        let mirror = "https://geo.mirror.pkgbuild.com";
        let repos = self.get_enabled_pacman_repos();
        
        println!("     Found {} enabled repos: {}", repos.len(), repos.join(", "));
        
        for repo in repos {
            let url = format!("{}/{}/os/x86_64/{}.db", mirror, repo, repo);
            let dest = self.cache_dir.join(format!("pacman-{}.db", repo));
            
            if !force && dest.exists() {
                // Check if less than 1 hour old
                if let Ok(metadata) = fs::metadata(&dest) {
                    if let Ok(modified) = metadata.modified() {
                        if let Ok(elapsed) = modified.elapsed() {
                            if elapsed.as_secs() < 3600 {
                                continue; // Skip, still fresh
                            }
                        }
                    }
                }
            }
            
            println!("     Downloading {}...", repo);
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?;
            
            match client.get(&url).send() {
                Ok(response) => {
                    if response.status().is_success() {
                        let bytes = response.bytes()?;
                        fs::write(&dest, bytes)?;
                    }
                }
                Err(e) => eprintln!("     Warning: Failed to download {}: {}", repo, e),
            }
        }
        
        // Parse downloaded .db files and store in redb
        use crate::redb_cache::RedbCache;
        use crate::pm::PackageManager;
        use crate::pm::pacman::Pacman;
        
        let cache = RedbCache::new()?;
        let pacman = Pacman::new();
        
        // Use PackageManager's list_all() which reads from pmux cache
        if let Ok(all_packages) = pacman.list_all() {
            if !all_packages.is_empty() {
                println!("     Found {} pacman packages", all_packages.len());
                
                // If force refresh, clear all packages first
                if force {
                    cache.clear_packages("pacman")?;
                }
                
                let updated = cache.update_packages("pacman", &all_packages)?;
                
                // Update sync metadata
                use crate::redb_cache::SyncMetadata;
                let metadata = SyncMetadata {
                    last_sync: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_secs(),
                    checksum: String::new(),
                    package_count: all_packages.len(),
                };
                cache.set_sync_metadata("pacman", "main", &metadata)?;
                
                println!("     Successfully cached {} pacman packages ({} updated)", all_packages.len(), updated);
            }
        }
        
        Ok(())
    }
    
    fn sync_aur(&self, force: bool) -> Result<()> {
        println!("  -> Syncing AUR packages with full metadata...");
        
        use crate::redb_cache::{RedbCache, SyncMetadata};
        
        let cache = RedbCache::new()?;
        
        // Check if we need to sync
        if !force {
            if let Ok(metadata) = cache.get_sync_metadata("aur", "main") {
                if let Some(meta) = metadata {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_secs();
                    
                    if now - meta.last_sync < 3600 {
                        println!("     AUR cache is fresh ({} packages cached)", meta.package_count);
                        return Ok(());
                    }
                }
            }
        }
        
        // Step 1: Download package names list
        println!("     Downloading AUR package names list...");
        let url = "https://aur.archlinux.org/packages.gz";
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        
        let package_names = match client.get(url).send() {
            Ok(response) => {
                if response.status().is_success() {
                    let bytes = response.bytes()?;
                    
                    // Decompress the gz file
                    use flate2::read::GzDecoder;
                    use std::io::Read;
                    
                    let mut decoder = GzDecoder::new(&bytes[..]);
                    let mut contents = String::new();
                    decoder.read_to_string(&mut contents)?;
                    
                    let names: Vec<String> = contents
                        .lines()
                        .filter(|line| !line.is_empty())
                        .map(|s| s.to_string())
                        .collect();
                    
                    println!("     Found {} AUR packages", names.len());
                    names
                } else {
                    eprintln!("     Warning: Failed to download AUR list");
                    return Ok(());
                }
            }
            Err(e) => {
                eprintln!("     Warning: Failed to download AUR list: {}", e);
                return Ok(());
            }
        };
        
        // Check which packages we already have cached
        let cached_packages = cache.get_all_packages("aur")?;
        let cached_names: std::collections::HashSet<String> = cached_packages
            .iter()
            .map(|p| p.name.clone())
            .collect();
        
        // Only fetch metadata for NEW packages (incremental update)
        let new_packages: Vec<String> = if force {
            package_names.clone()
        } else {
            package_names
                .iter()
                .filter(|name| !cached_names.contains(*name))
                .cloned()
                .collect()
        };
        
        if !force && new_packages.is_empty() {
            println!("     No new AUR packages to sync");
            
            // Update sync metadata
            let metadata = SyncMetadata {
                last_sync: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs(),
                checksum: String::new(),
                package_count: cached_packages.len(),
            };
            cache.set_sync_metadata("aur", "main", &metadata)?;
            
            return Ok(());
        }
        
        println!("     Fetching metadata for {} packages ({} new)...", 
                 if force { package_names.len() } else { new_packages.len() },
                 new_packages.len());
        
        // Step 2: Batch query AUR RPC API for full metadata (only new packages)
        let to_fetch = if force { &package_names } else { &new_packages };
        let batch_size = 200;
        let total_batches = (to_fetch.len() + batch_size - 1) / batch_size;
        
        let pb = ProgressBar::new(total_batches as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("     [{bar:40}] {pos}/{len} batches ({percent}%)")
                .unwrap()
                .progress_chars("=>-"),
        );
        
        let mut new_pkg_data = Vec::new();
        
        for (batch_idx, chunk) in to_fetch.chunks(batch_size).enumerate() {
            let mut url = "https://aur.archlinux.org/rpc/?v=5&type=info".to_string();
            for pkg in chunk {
                url.push_str(&format!("&arg[]={}", urlencoding::encode(pkg)));
            }
            
            // Retry logic with exponential backoff
            let mut retries = 3;
            let mut delay_ms = 100;
            let mut batch_success = false;
            
            while retries > 0 && !batch_success {
                match client.get(&url).send() {
                    Ok(response) => {
                        if response.status().is_success() {
                            if let Ok(json) = response.json::<serde_json::Value>() {
                                if let Some(results) = json["results"].as_array() {
                                    for result in results {
                                        if let Some(name) = result["Name"].as_str() {
                                            use crate::pm::Package;
                                            let pkg = Package {
                                                name: name.to_string(),
                                                version: result["Version"].as_str().map(|s| s.to_string()),
                                                description: result["Description"].as_str().unwrap_or("").to_string(),
                                                repo: "aur".to_string(),
                                                manager: "aur".to_string(),
                                                installed: false,
                                                homepage: result["URL"].as_str().unwrap_or("").to_string(),
                                                license: result["License"].as_array()
                                                    .and_then(|arr| arr.first())
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("").to_string(),
                                                size: None,
                                            };
                                            new_pkg_data.push(pkg);
                                        }
                                    }
                                    batch_success = true;
                                }
                            }
                        } else {
                            // HTTP error - retry
                            retries -= 1;
                            if retries > 0 {
                                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                                delay_ms *= 2; // Exponential backoff
                            }
                        }
                    }
                    Err(e) => {
                        retries -= 1;
                        if retries > 0 {
                            // Retry with exponential backoff
                            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                            delay_ms *= 2;
                        } else {
                            eprintln!("\n     Warning: Batch {} failed after 3 retries: {}", batch_idx + 1, e);
                        }
                    }
                }
            }
            
            pb.inc(1);
            
            if batch_idx < total_batches - 1 {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        
        pb.finish_and_clear();
        
        // Step 3: Update the cache
        // If force refresh, clear all packages first for a clean rebuild
        if force {
            cache.clear_packages("aur")?;
        }
        
        let updated = cache.update_packages("aur", &new_pkg_data)?;
        
        // Update sync metadata
        let total_count = if force {
            new_pkg_data.len()
        } else {
            cached_packages.len() + updated
        };
        
        let metadata = SyncMetadata {
            last_sync: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            checksum: String::new(),
            package_count: total_count,
        };
        cache.set_sync_metadata("aur", "main", &metadata)?;
        
        println!("     Successfully updated {} AUR packages ({} total cached)", updated, total_count);
        
        Ok(())
    }
    
    fn sync_nix(&self, force: bool) -> Result<()> {
        println!("  -> Syncing Nix packages...");
        
        use crate::redb_cache::{RedbCache, SyncMetadata};
        use crate::pm::Package;
        
        let cache = RedbCache::new()?;
        
        // Check cache freshness
        if !force {
            if let Ok(metadata) = cache.get_sync_metadata("nix", "main") {
                if let Some(meta) = metadata {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_secs();
                    
                    if now - meta.last_sync < 3600 {
                        println!("     Nix cache is fresh ({} packages cached)", meta.package_count);
                        return Ok(());
                    }
                }
            }
        }
        
        // Download nixpkgs package list from GitHub releases
        println!("     Downloading nixpkgs package list...");
        let url = "https://channels.nixos.org/nixos-unstable/packages.json.br";
        
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        
        let packages = match client.get(url).send() {
            Ok(response) => {
                if response.status().is_success() {
                    let bytes = response.bytes()?;
                    
                    // Decompress brotli
                    use std::io::Read;
                    let mut decompressor = brotli::Decompressor::new(&bytes[..], 4096);
                    let mut decompressed = Vec::new();
                    decompressor.read_to_end(&mut decompressed)?;
                    
                    // Parse JSON and convert to Package structs
                    let json: serde_json::Value = serde_json::from_slice(&decompressed)?;
                    let mut packages = Vec::new();
                    
                    if let Some(pkgs_obj) = json.get("packages").and_then(|p| p.as_object()) {
                        for (_attr_path, pkg_data) in pkgs_obj {
                            if let Some(pname) = pkg_data.get("pname").and_then(|p| p.as_str()) {
                                let version = pkg_data.get("version").and_then(|v| v.as_str());
                                
                                let description = pkg_data
                                    .get("meta")
                                    .and_then(|m| m.get("description"))
                                    .and_then(|d| d.as_str())
                                    .or_else(|| pkg_data.get("description").and_then(|d| d.as_str()))
                                    .unwrap_or("");

                                let homepage = pkg_data
                                    .get("meta")
                                    .and_then(|m| m.get("homepage"))
                                    .and_then(|h| h.as_str())
                                    .or_else(|| pkg_data.get("meta")
                                        .and_then(|m| m.get("homepage"))
                                        .and_then(|h| h.as_array())
                                        .and_then(|arr| arr.first())
                                        .and_then(|v| v.as_str()))
                                    .unwrap_or("")
                                    .to_string();

                                let license = pkg_data
                                    .get("meta")
                                    .and_then(|m| m.get("license"))
                                    .and_then(|l| l.as_str())
                                    .or_else(|| pkg_data.get("meta")
                                        .and_then(|m| m.get("license"))
                                        .and_then(|l| l.as_array())
                                        .and_then(|arr| arr.first())
                                        .and_then(|v| v.as_str()))
                                    .unwrap_or("")
                                    .to_string();

                                let size = pkg_data
                                    .get("meta")
                                    .and_then(|m| m.get("outputs"))
                                    .and_then(|o| o.get("out"))
                                    .and_then(|out| out.get("size"))
                                    .and_then(|s| s.as_u64())
                                    .or_else(|| pkg_data.get("size").and_then(|s| s.as_u64()));

                                packages.push(Package {
                                    name: pname.to_string(),
                                    version: version.map(|s| s.to_string()),
                                    description: description.to_string(),
                                    repo: "nixpkgs".to_string(),
                                    manager: "nix".to_string(),
                                    installed: false,
                                    homepage,
                                    license,
                                    size,
                                });
                            }
                        }
                    }
                    
                    packages
                } else {
                    eprintln!("     Warning: Failed to download nixpkgs list");
                    return Ok(());
                }
            }
            Err(e) => {
                eprintln!("     Warning: Failed to download nixpkgs: {}", e);
                return Ok(());
            }
        };
        
        if packages.is_empty() {
            eprintln!("     Warning: No Nix packages found");
            return Ok(());
        }
        
        println!("     Found {} Nix packages", packages.len());
        
        // If force refresh, clear all packages first for a clean rebuild
        if force {
            cache.clear_packages("nix")?;
        }
        
        // Update cache
        let updated = cache.update_packages("nix", &packages)?;
        
        // Update sync metadata
        let metadata = SyncMetadata {
            last_sync: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            checksum: String::new(),
            package_count: packages.len(),
        };
        cache.set_sync_metadata("nix", "main", &metadata)?;
        
        println!("     Successfully cached {} Nix packages ({} updated)", packages.len(), updated);
        
        Ok(())
    }
    
    #[allow(dead_code)]
    fn get_enabled_dnf_repos(&self) -> Vec<(String, String)> {
        // Try dnf command first (works with metalink/mirrorlist)
        if let Ok(output) = std::process::Command::new("dnf")
            .args(&["repolist", "--enabled", "-v"])
            .output()
        {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                let mut repos = Vec::new();
                
                for line in text.lines() {
                    // Look for "Repo-baseurl" lines
                    if line.contains("Repo-baseurl") {
                        if let Some(url_part) = line.split(':').nth(1) {
                            let url = url_part.trim();
                            // Extract repo name from URL (crude but works)
                            if let Some(repo_id_line) = text.lines()
                                .take_while(|l| !l.contains(line))
                                .last()
                            {
                                if let Some(repo_id) = repo_id_line.split_whitespace().next() {
                                    if !url.is_empty() && !repo_id.is_empty() {
                                        repos.push((repo_id.to_string(), url.to_string()));
                                    }
                                }
                            }
                        }
                    }
                }
                
                if !repos.is_empty() {
                    return repos;
                }
            }
        }
        
        // Fallback: Read /etc/yum.repos.d/*.repo files (for baseurl repos)
        let mut repos = Vec::new();
        
        if let Ok(entries) = fs::read_dir("/etc/yum.repos.d") {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("repo") {
                    if let Ok(content) = fs::read_to_string(&path) {
                        let mut current_repo = None;
                        let mut current_baseurl = None;
                        let mut current_metalink = None;
                        let mut enabled = false;
                        
                        for line in content.lines() {
                            let line = line.trim();
                            
                            if line.starts_with('[') && line.ends_with(']') {
                                // Save previous repo if it was enabled
                                if let Some(name) = current_repo.take() {
                                    let url = current_baseurl.take().or(current_metalink.take());
                                    if enabled && url.is_some() {
                                        repos.push((name, url.unwrap()));
                                    }
                                }
                                
                                current_repo = Some(line[1..line.len()-1].to_string());
                                enabled = false;
                                current_baseurl = None;
                                current_metalink = None;
                            } else if line.starts_with("enabled=1") || line.starts_with("enabled = 1") {
                                enabled = true;
                            } else if line.starts_with("baseurl=") {
                                current_baseurl = Some(line[8..].trim().to_string());
                            } else if line.starts_with("metalink=") {
                                // Extract metalink URL
                                current_metalink = Some(line[9..].trim().to_string());
                            } else if line.starts_with("mirrorlist=") {
                                current_metalink = Some(line[11..].trim().to_string());
                            }
                        }
                        
                        // Save last repo
                        if let Some(name) = current_repo {
                            let url = current_baseurl.or(current_metalink);
                            if enabled && url.is_some() {
                                repos.push((name, url.unwrap()));
                            }
                        }
                    }
                }
            }
        }
        
        repos
    }
    
    fn sync_dnf(&self, force: bool) -> Result<()> {
        println!("  -> Syncing DNF repositories...");
        
        use crate::redb_cache::{RedbCache, SyncMetadata};
        use crate::pm::Package;
        
        let cache = RedbCache::new()?;
        
        // Check cache freshness
        if !force {
            if let Ok(metadata) = cache.get_sync_metadata("dnf", "main") {
                if let Some(meta) = metadata {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_secs();
                    
                    if now - meta.last_sync < 3600 {
                        println!("     DNF cache is fresh ({} packages cached)", meta.package_count);
                        return Ok(());
                    }
                }
            }
        }
        
        // Use dnf repoquery (works with metalink/mirrorlist, much simpler!)
        println!("     Querying DNF for available packages (this may take a minute)...");
        
        // Use dnf repoquery with full metadata - summary (single-line), url, license, installsize
        // Use %{summary} instead of %{description} because descriptions can contain newlines
        // which break line-by-line parsing. Summary is always single-line.
        // Use --available to only get available packages (not installed)
        // CRITICAL: Add \n to queryformat to ensure each package is on its own line
        let output = std::process::Command::new("dnf")
            .args(&[
                "repoquery",
                "--available",
                "--quiet",
                "--queryformat",
                "%{name}|||%{version}|||%{summary}|||%{url}|||%{license}|||%{installsize}|||%{repoid}\n"
            ])
            .output();
        
        let packages = match output {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout);
                let mut packages = Vec::new();
                
                for line in text.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    
                    let parts: Vec<&str> = line.split("|||").collect();
                    if parts.len() >= 7 {
                        let name = parts[0].trim();
                        let version = parts[1].trim();
                        let summary = parts[2].trim();
                        let url = parts[3].trim();
                        let license = parts[4].trim();
                        let size_str = parts[5].trim();
                        let repoid = parts[6].trim();
                        
                        // VALIDATION: Filter out obvious license strings and malformed entries
                        // RPM package names can contain: alphanumeric, dots, dashes, underscores, plus signs
                        // They can start with numbers (e.g., "0ad-data")
                        // Reject only obvious license strings or descriptions that got parsed as names
                        let is_invalid = name.is_empty()
                            || name.len() > 200  // Way too long for a package name
                            || name.contains(" AND ")  // License string
                            || name.contains(" OR ")   // License string
                            || name.contains(" WITH ")  // License string
                            || (name.starts_with('(') && name.contains(')'))  // Parenthesized license
                            || (name.len() > 50 && !name.chars().any(|c| c.is_alphanumeric()));  // Long non-alphanumeric string
                        
                        if is_invalid {
                            // This looks like a license string or malformed entry - skip it
                            continue;
                        }
                        
                        // Parse installsize (in bytes)
                        let size = size_str.parse::<u64>().ok();
                        
                        let pkg = Package {
                            name: name.to_string(),
                            version: if version.is_empty() { None } else { Some(version.to_string()) },
                            description: summary.to_string(),
                            repo: repoid.to_string(),
                            manager: "dnf".to_string(),
                            installed: false,
                            homepage: url.to_string(),
                            license: license.to_string(),
                            size,
                        };
                        packages.push(pkg);
                    } else if parts.len() > 0 {
                        // Log malformed lines for debugging (only first few)
                        if packages.len() < 5 {
                            eprintln!("     Warning: Skipping malformed DNF package line: {}", line.chars().take(100).collect::<String>());
                        }
                    }
                }
                
                packages
            }
            Ok(_) => {
                eprintln!("     Warning: DNF repoquery failed");
                return Ok(());
            }
            Err(e) => {
                eprintln!("     Warning: Failed to run dnf repoquery: {}", e);
                eprintln!("     Make sure dnf is installed and configured");
                return Ok(());
            }
        };
        
        if packages.is_empty() {
            eprintln!("     Warning: No DNF packages found");
            return Ok(());
        }
        
        println!("     Found {} packages from DNF repos", packages.len());
        
        // If force refresh, clear all packages first for a clean rebuild
        if force {
            cache.clear_packages("dnf")?;
        }
        
        // Update cache
        let updated = cache.update_packages("dnf", &packages)?;
        
        // Update sync metadata
        let metadata = SyncMetadata {
            last_sync: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            checksum: String::new(),
            package_count: packages.len(),
        };
        cache.set_sync_metadata("dnf", "main", &metadata)?;
        
        println!("     Successfully cached {} DNF packages ({} updated)", packages.len(), updated);
        
        Ok(())
    }
    
    #[allow(dead_code)]
    fn parse_repomd_primary_location(&self, xml: &str) -> Option<String> {
        use quick_xml::Reader;
        use quick_xml::events::Event;
        
        let mut reader = Reader::from_str(xml);
        reader.trim_text(true);
        
        let mut in_primary = false;
        let mut buf = Vec::new();
        
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    match e.name().as_ref() {
                        b"data" => {
                            // Check if type="primary"
                            for attr in e.attributes() {
                                if let Ok(attr) = attr {
                                    if attr.key.as_ref() == b"type" && attr.value.as_ref() == b"primary" {
                                        in_primary = true;
                                    }
                                }
                            }
                        }
                        b"location" if in_primary => {
                            // Get href attribute
                            for attr in e.attributes() {
                                if let Ok(attr) = attr {
                                    if attr.key.as_ref() == b"href" {
                                        if let Ok(location) = std::str::from_utf8(&attr.value) {
                                            return Some(location.to_string());
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::End(e)) => {
                    if e.name().as_ref() == b"data" {
                        in_primary = false;
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        
        None
    }
    
    #[allow(dead_code)]
    fn parse_dnf_primary_xml(&self, xml: &str, repo: &str) -> Vec<serde_json::Value> {
        use quick_xml::Reader;
        use quick_xml::events::Event;
        
        let mut reader = Reader::from_str(xml);
        reader.trim_text(true);
        
        let mut packages = Vec::new();
        let mut buf = Vec::new();
        
        let mut current_name = String::new();
        let mut current_version = String::new();
        let mut current_desc = String::new();
        let mut current_url = String::new();
        let mut current_license = String::new();
        let mut current_size = 0u64;
        let mut in_package = false;
        let mut current_element = String::new();
        
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    match e.name().as_ref() {
                        b"package" => {
                            in_package = true;
                            current_name.clear();
                            current_version.clear();
                            current_desc.clear();
                            current_url.clear();
                            current_license.clear();
                            current_size = 0;
                        }
                        b"name" if in_package => current_element = "name".to_string(),
                        b"summary" if in_package => current_element = "desc".to_string(),
                        b"description" if in_package => current_element = "desc".to_string(),
                        b"url" if in_package => current_element = "url".to_string(),
                        b"version" if in_package => {
                            // Parse version attributes
                            for attr in e.attributes() {
                                if let Ok(attr) = attr {
                                    if attr.key.as_ref() == b"ver" {
                                        if let Ok(ver) = std::str::from_utf8(&attr.value) {
                                            current_version = ver.to_string();
                                        }
                                    } else if attr.key.as_ref() == b"rel" {
                                        if let Ok(rel) = std::str::from_utf8(&attr.value) {
                                            if !current_version.is_empty() {
                                                current_version.push('-');
                                            }
                                            current_version.push_str(rel);
                                        }
                                    }
                                }
                            }
                        }
                        b"size" if in_package => {
                            for attr in e.attributes() {
                                if let Ok(attr) = attr {
                                    if attr.key.as_ref() == b"package" {
                                        if let Ok(size_str) = std::str::from_utf8(&attr.value) {
                                            current_size = size_str.parse().unwrap_or(0);
                                        }
                                    }
                                }
                            }
                        }
                        b"rpm:license" if in_package => current_element = "license".to_string(),
                        _ => {}
                    }
                }
                Ok(Event::Text(e)) => {
                    if !current_element.is_empty() {
                        if let Ok(text) = e.unescape() {
                            match current_element.as_str() {
                                "name" => current_name = text.to_string(),
                                "desc" if current_desc.is_empty() => current_desc = text.to_string(),
                                "url" => current_url = text.to_string(),
                                "license" => current_license = text.to_string(),
                                _ => {}
                            }
                        }
                    }
                }
                Ok(Event::End(e)) => {
                    match e.name().as_ref() {
                        b"package" => {
                            in_package = false;
                            if !current_name.is_empty() {
                                packages.push(serde_json::json!({
                                    "name": current_name,
                                    "version": current_version,
                                    "description": current_desc,
                                    "repo": repo,
                                    "url": current_url,
                                    "license": current_license,
                                    "size": current_size,
                                }));
                            }
                        }
                        b"name" | b"summary" | b"description" | b"url" | b"rpm:license" => {
                            current_element.clear();
                        }
                        _ => {}
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
        
        packages
    }
    
    #[allow(dead_code)]
    fn parse_repomd_for_primary(&self, xml: &str) -> Option<String> {
        // Simple XML parsing to find primary.xml.gz location
        for line in xml.lines() {
            if line.contains("type=\"primary\"") {
                // Look for the location href in nearby lines
                for search_line in xml.lines() {
                    if search_line.contains("<location href=") {
                        if let Some(start) = search_line.find("href=\"") {
                            if let Some(end) = search_line[start + 6..].find("\"") {
                                return Some(search_line[start + 6..start + 6 + end].to_string());
                            }
                        }
                    }
                }
            }
        }
        None
    }
    
    fn get_enabled_portage_overlays(&self) -> Vec<String> {
        // Read /etc/portage/repos.conf/ to find enabled overlays
        let mut overlays = vec!["gentoo".to_string()]; // Main repo always included
        
        let repos_conf_dir = PathBuf::from("/etc/portage/repos.conf");
        
        if repos_conf_dir.exists() {
            if repos_conf_dir.is_dir() {
                // Multiple config files
                if let Ok(entries) = fs::read_dir(&repos_conf_dir) {
                    for entry in entries.filter_map(|e| e.ok()) {
                        if let Ok(content) = fs::read_to_string(entry.path()) {
                            overlays.extend(self.parse_portage_repos_conf(&content));
                        }
                    }
                }
            } else {
                // Single config file
                if let Ok(content) = fs::read_to_string(&repos_conf_dir) {
                    overlays.extend(self.parse_portage_repos_conf(&content));
                }
            }
        }
        
        overlays.sort();
        overlays.dedup();
        overlays
    }
    
    fn parse_portage_repos_conf(&self, content: &str) -> Vec<String> {
        let mut repos = Vec::new();
        let mut current_repo = None;
        
        for line in content.lines() {
            let line = line.trim();
            
            if line.starts_with('[') && line.ends_with(']') {
                current_repo = Some(line[1..line.len()-1].to_string());
            } else if line.starts_with("location") {
                if let Some(repo) = current_repo.take() {
                    if repo != "DEFAULT" {
                        repos.push(repo);
                    }
                }
            }
        }
        
        repos
    }
    
    fn sync_portage(&self, force: bool) -> Result<()> {
        println!("  -> Syncing Portage tree...");
        
        let overlays = self.get_enabled_portage_overlays();
        println!("     Found {} overlays: {}", overlays.len(), overlays.join(", "));
        
        let dest = self.cache_dir.join("portage-snapshot.tar.xz");
        let list_dest = self.cache_dir.join("portage-packages.txt");
        
        if !force && list_dest.exists() {
            if let Ok(metadata) = fs::metadata(&list_dest) {
                if let Ok(modified) = metadata.modified() {
                    if let Ok(elapsed) = modified.elapsed() {
                        if elapsed.as_secs() < 3600 {
                            return Ok(());
                        }
                    }
                }
            }
        }
        
        // Download portage snapshot from Gentoo mirrors
        println!("     Downloading portage snapshot...");
        let url = "https://distfiles.gentoo.org/snapshots/portage-latest.tar.xz";
        
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        
        match client.get(url).send() {
            Ok(response) => {
                if response.status().is_success() {
                    let bytes = response.bytes()?;
                    fs::write(&dest, bytes)?;
                    
                    // Extract package list from snapshot
                    println!("     Extracting package list...");
                    match std::process::Command::new("tar")
                        .args(&["--list", "-f", dest.to_str().unwrap()])
                        .output()
                    {
                        Ok(output) => {
                            if output.status.success() {
                                // Filter for package directories
                                let list: Vec<String> = String::from_utf8_lossy(&output.stdout)
                                    .lines()
                                    .filter(|line| {
                                        let parts: Vec<&str> = line.split('/').collect();
                                        parts.len() >= 3 && !parts[1].starts_with('.')
                                    })
                                    .filter_map(|line| {
                                        let parts: Vec<&str> = line.split('/').collect();
                                        if parts.len() >= 3 {
                                            Some(format!("{}/{}", parts[1], parts[2]))
                                        } else {
                                            None
                                        }
                                    })
                                    .collect::<std::collections::HashSet<_>>()
                                    .into_iter()
                                    .collect();
                                
                                fs::write(&list_dest, list.join("\n"))?;
                                println!("     Portage snapshot downloaded and processed");
                            }
                        }
                        Err(e) => eprintln!("     Warning: Failed to extract: {}", e),
                    }
                } else {
                    eprintln!("     Warning: Failed to download portage snapshot");
                }
            }
            Err(e) => eprintln!("     Warning: Failed to download: {}", e),
        }
        
        // Parse portage tree and store in redb
        use crate::redb_cache::RedbCache;
        use crate::pm::PackageManager;
        use crate::pm::emerge::Emerge;
        
        let cache = RedbCache::new()?;
        let emerge = Emerge::new();
        
        // Parse portage tree using PackageManager trait
        println!("     Parsing portage tree (this may take a moment)...");
        if let Ok(packages) = emerge.list_all() {
            if !packages.is_empty() {
                println!("     Found {} emerge packages", packages.len());
                
                // If force refresh, clear all packages first
                if force {
                    cache.clear_packages("emerge")?;
                }
                
                println!("     Storing packages in cache...");
                let updated = cache.update_packages("emerge", &packages)?;
                
                // Update sync metadata
                use crate::redb_cache::SyncMetadata;
                let metadata = SyncMetadata {
                    last_sync: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_secs(),
                    checksum: String::new(),
                    package_count: packages.len(),
                };
                cache.set_sync_metadata("emerge", "main", &metadata)?;
                
                println!("     Successfully cached {} emerge packages ({} updated)", packages.len(), updated);
            } else {
                println!("     No emerge packages found");
            }
        } else {
            println!("     Warning: Failed to parse portage tree");
        }
        
        Ok(())
    }

    fn sync_pkgit(&self, _force: bool) -> Result<()> {
        println!("  -> Syncing pkgit repositories...");
        
        use crate::pm::pkgit::Pkgit;
        use crate::pm::PackageManager;
        
        let pkgit = Pkgit::new();
        
        if !pkgit.is_available() {
            println!("     Warning: pkgit is not installed, skipping");
            return Ok(());
        }
        
        // Load packages from pkgit
        let packages = pkgit.list_all()?;
        
        if packages.is_empty() {
            println!("     No pkgit repos found");
            return Ok(());
        }
        
        println!("     Found {} pkgit repositories", packages.len());
        
        // Store in cache
        use crate::cache::CacheManager;
        let cache = CacheManager::new()?;
        let redb_cache = cache.redb_cache();
        
        let updated = redb_cache.update_packages("pkgit", &packages)?;
        
        // Update sync metadata
        use crate::redb_cache::SyncMetadata;
        let metadata = SyncMetadata {
            last_sync: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            checksum: String::new(),
            package_count: packages.len(),
        };
        redb_cache.set_sync_metadata("pkgit", "main", &metadata)?;
        
        println!("     Successfully cached {} pkgit packages ({} updated)", packages.len(), updated);
        
        Ok(())
    }
}


