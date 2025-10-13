use super::{Package, PackageManager};
use eyre::Result;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub struct Paru {
    helper: String,
    dbpath: PathBuf,
}

impl Paru {
    pub fn new() -> Self {
        let helper = if Command::new("paru").arg("--version").output().is_ok() {
            "paru".to_string()
        } else if Command::new("yay").arg("--version").output().is_ok() {
            "yay".to_string()
        } else {
            "paru".to_string()
        };
        
        // Check for bedrock linux stratum paths
        let possible_paths = vec![
            PathBuf::from("/var/lib/pacman"),
            PathBuf::from("/bedrock/strata/arch/var/lib/pacman"),
            PathBuf::from("/bedrock/strata/artix/var/lib/pacman"),
            PathBuf::from("/bedrock/strata/manjaro/var/lib/pacman"),
        ];
        
        let dbpath = possible_paths
            .into_iter()
            .find(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("/var/lib/pacman"));
        
        Self { helper, dbpath }
    }
    
    // Parse local database to get foreign (AUR) packages with descriptions
    fn parse_foreign_packages(&self) -> Result<Vec<Package>> {
        let local_path = self.dbpath.join("local");
        let mut packages = Vec::new();
        
        if !local_path.exists() {
            return Ok(packages);
        }
        
        // Get list of packages from official repos
        let mut official_pkgs = std::collections::HashSet::new();
        let sync_path = self.dbpath.join("sync");
        if sync_path.exists() {
            for entry in fs::read_dir(sync_path)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("db") {
                    // Parse sync db to get official package names
                    if let Ok(file) = fs::File::open(&path) {
                        use flate2::read::GzDecoder;
                        use tar::Archive;
                        
                        let decoder = GzDecoder::new(file);
                        let mut archive = Archive::new(decoder);
                        
                        for entry in archive.entries()? {
                            if let Ok(entry) = entry {
                                let path = entry.path()?;
                                let path_str = path.to_string_lossy();
                                if let Some(pkg_name) = path_str.split('/').next() {
                                    // Remove version suffix
                                    if let Some(name) = pkg_name.split('-').next() {
                                        official_pkgs.insert(name.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        
        // Now parse local db for foreign packages
        for entry in fs::read_dir(local_path)? {
            let entry = entry?;
            let path = entry.path();
            
            if path.is_dir() {
                let desc_file = path.join("desc");
                if desc_file.exists() {
                    let content = fs::read_to_string(desc_file)?;
                    let mut pkg_data = std::collections::HashMap::new();
                    let mut current_field = String::new();
                    
                    for line in content.lines() {
                        let line = line.trim();
                        if line.starts_with('%') && line.ends_with('%') {
                            current_field = line.trim_matches('%').to_string();
                        } else if !line.is_empty() && !current_field.is_empty() {
                            pkg_data
                                .entry(current_field.clone())
                                .or_insert_with(String::new)
                                .push_str(line);
                        }
                    }
                    
                    if let Some(name) = pkg_data.get("NAME") {
                        // Check if it's a foreign package (not in official repos)
                        if !official_pkgs.contains(name) {
                            packages.push(Package {
                                name: name.clone(),
                                version: pkg_data.get("VERSION").cloned(),
                                description: pkg_data.get("DESC").cloned().unwrap_or_default(),
                                repo: "aur".to_string(),
                                manager: "aur".to_string(),
                                installed: true,
                            });
                        }
                    }
                }
            }
        }
        
        Ok(packages)
    }
}

impl PackageManager for Paru {
    fn name(&self) -> &str {
        "aur"
    }
    
    fn is_available(&self) -> bool {
        Command::new(&self.helper).arg("--version").output().is_ok()
    }
    
    fn list_all(&self) -> Result<Vec<Package>> {
        // Try to load from pmux cache first
        if let Some(cache_dir) = dirs::cache_dir() {
            let aur_list = cache_dir.join("pmux").join("repos").join("aur-packages.txt");
            if aur_list.exists() {
                // Use mmap for zero-copy
                let file = fs::File::open(aur_list)?;
                let mmap = unsafe { memmap2::Mmap::map(&file)? };
                let content = std::str::from_utf8(&mmap)?;
                
                // Count lines for pre-allocation
                let line_count = content.bytes().filter(|&b| b == b'\n').count();
                let mut packages = Vec::with_capacity(line_count);
                
                // Fast line parsing
                for line in content.lines() {
                    if line.is_empty() {
                        continue;
                    }
                    
                    packages.push(Package {
                        name: line.trim().to_string(),
                        version: None,
                        description: String::new(),
                        repo: "aur".to_string(),
                        manager: "aur".to_string(),
                        installed: false,
                    });
                }
                
                return Ok(packages);
            }
        }
        
        Ok(vec![])
    }
    
    fn list_installed(&self) -> Result<Vec<Package>> {
        self.parse_foreign_packages()
    }
    
    #[allow(dead_code)]
    fn search(&self, query: &str) -> Result<Vec<Package>> {
        // Use AUR RPC API for search
        let url = format!(
            "https://aur.archlinux.org/rpc/?v=5&type=search&arg={}",
            urlencoding::encode(query)
        );
        
        let response: serde_json::Value = reqwest::blocking::get(&url)?.json()?;
        
        let mut packages = Vec::new();
        if let Some(results) = response["results"].as_array() {
            for result in results {
                if let (Some(name), Some(desc)) = (
                    result["Name"].as_str(),
                    result["Description"].as_str(),
                ) {
                    packages.push(Package {
                        name: name.to_string(),
                        version: result["Version"].as_str().map(|s| s.to_string()),
                        description: desc.to_string(),
                        repo: "aur".to_string(),
                        manager: "aur".to_string(),
                        installed: false,
                    });
                }
            }
        }
        
        Ok(packages)
    }
    
    fn install_command(&self, packages: &[&Package]) -> String {
        let pkg_names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        format!("{} -S {}", self.helper, pkg_names.join(" "))
    }
    
    #[allow(dead_code)]
    fn needs_sudo(&self) -> bool {
        false
    }
}
