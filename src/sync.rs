// Repository synchronization module
// Downloads and caches package databases from upstream sources

use eyre::Result;
use std::fs;
use std::path::PathBuf;

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
    
    pub fn sync_all(&self, force: bool) -> Result<()> {
        println!(":: Synchronizing package databases...");
        
        // Sync each PM's repos
        self.sync_pacman(force)?;
        self.sync_aur(force)?;
        self.sync_nix(force)?;
        self.sync_dnf(force)?;
        self.sync_portage(force)?;
        
        println!(":: Package databases are up to date");
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
        
        Ok(())
    }
    
    fn sync_aur(&self, force: bool) -> Result<()> {
        println!("  -> Syncing AUR package list...");
        
        // AUR doesn't have a full package list API
        // We'll download the package names list and extract it
        let url = "https://aur.archlinux.org/packages.gz";
        let dest_gz = self.cache_dir.join("aur-packages.gz");
        let dest_txt = self.cache_dir.join("aur-packages.txt");
        
        if !force && dest_txt.exists() {
            if let Ok(metadata) = fs::metadata(&dest_txt) {
                if let Ok(modified) = metadata.modified() {
                    if let Ok(elapsed) = modified.elapsed() {
                        if elapsed.as_secs() < 3600 {
                            return Ok(()); // Still fresh
                        }
                    }
                }
            }
        }
        
        println!("     Downloading AUR package list...");
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        
        match client.get(url).send() {
            Ok(response) => {
                if response.status().is_success() {
                    let bytes = response.bytes()?;
                    fs::write(&dest_gz, &bytes)?;
                    
                    // Decompress the gz file
                    use flate2::read::GzDecoder;
                    use std::io::Read;
                    
                    let file = fs::File::open(&dest_gz)?;
                    let mut decoder = GzDecoder::new(file);
                    let mut contents = String::new();
                    decoder.read_to_string(&mut contents)?;
                    fs::write(&dest_txt, contents)?;
                    
                    println!("     AUR package list updated");
                }
            }
            Err(e) => eprintln!("     Warning: Failed to download AUR list: {}", e),
        }
        
        Ok(())
    }
    
    fn sync_nix(&self, force: bool) -> Result<()> {
        println!("  -> Syncing Nix packages...");
        
        let dest = self.cache_dir.join("nix-packages.json");
        
        if !force && dest.exists() {
            if let Ok(metadata) = fs::metadata(&dest) {
                if let Ok(modified) = metadata.modified() {
                    if let Ok(elapsed) = modified.elapsed() {
                        if elapsed.as_secs() < 3600 {
                            return Ok(());
                        }
                    }
                }
            }
        }
        
        // Download nixpkgs package list from GitHub releases
        // This is the official package list JSON
        println!("     Downloading nixpkgs package list...");
        let url = "https://channels.nixos.org/nixos-unstable/packages.json.br";
        
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        
        match client.get(url).send() {
            Ok(response) => {
                if response.status().is_success() {
                    let bytes = response.bytes()?;
                    
                    // Decompress brotli
                    use std::io::Read;
                    let mut decompressor = brotli::Decompressor::new(&bytes[..], 4096);
                    let mut decompressed = Vec::new();
                    decompressor.read_to_end(&mut decompressed)?;
                    
                    fs::write(&dest, decompressed)?;
                    println!("     Nix packages downloaded and cached");
                } else {
                    eprintln!("     Warning: Failed to download nixpkgs list");
                }
            }
            Err(e) => eprintln!("     Warning: Failed to download nixpkgs: {}", e),
        }
        
        Ok(())
    }
    
    fn get_enabled_dnf_repos(&self) -> Vec<(String, String)> {
        // Read /etc/yum.repos.d/*.repo files to find enabled repos
        let mut repos = Vec::new();
        
        if let Ok(entries) = fs::read_dir("/etc/yum.repos.d") {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("repo") {
                    if let Ok(content) = fs::read_to_string(&path) {
                        let mut current_repo = None;
                        let mut current_baseurl = None;
                        let mut enabled = false;
                        
                        for line in content.lines() {
                            let line = line.trim();
                            
                            if line.starts_with('[') && line.ends_with(']') {
                                // Save previous repo if it was enabled
                                if let (Some(name), Some(url)) = (current_repo.take(), current_baseurl.take()) {
                                    if enabled {
                                        repos.push((name, url));
                                    }
                                }
                                
                                current_repo = Some(line[1..line.len()-1].to_string());
                                enabled = false;
                            } else if line.starts_with("enabled=1") || line.starts_with("enabled = 1") {
                                enabled = true;
                            } else if line.starts_with("baseurl=") {
                                current_baseurl = Some(line[8..].trim().to_string());
                            } else if line.starts_with("metalink=") || line.starts_with("mirrorlist=") {
                                // For now, skip metalink/mirrorlist repos
                                // Could be enhanced to resolve these
                            }
                        }
                        
                        // Save last repo
                        if let (Some(name), Some(url)) = (current_repo, current_baseurl) {
                            if enabled {
                                repos.push((name, url));
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
        
        let dest = self.cache_dir.join("dnf-packages.txt");
        
        if !force && dest.exists() {
            if let Ok(metadata) = fs::metadata(&dest) {
                if let Ok(modified) = metadata.modified() {
                    if let Ok(elapsed) = modified.elapsed() {
                        if elapsed.as_secs() < 3600 {
                            println!("     DNF cache is fresh");
                            return Ok(());
                        }
                    }
                }
            }
        }
        
        let enabled_repos = self.get_enabled_dnf_repos();
        
        if enabled_repos.is_empty() {
            println!("     No enabled DNF repos found");
            return Ok(());
        }
        
        println!("     Found {} enabled repos", enabled_repos.len());
        println!("     Querying DNF for all available packages...");
        
        // Use dnf to list all available packages from all enabled repos
        // This is way simpler than trying to download/parse repomd.xml
        let output = std::process::Command::new("dnf")
            .args(&["list", "--available", "-q"])
            .output();
        
        match output {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                fs::write(&dest, stdout.as_ref())?;
                println!("     DNF package list cached ({} repos)", enabled_repos.len());
            }
            Ok(_) => {
                eprintln!("     Warning: dnf command failed");
            }
            Err(e) => {
                eprintln!("     Warning: Failed to run dnf: {}", e);
            }
        }
        
        Ok(())
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
        
        Ok(())
    }
}
