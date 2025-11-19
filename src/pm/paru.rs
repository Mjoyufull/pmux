use super::{Package, PackageManager};
use eyre::Result;
use std::fs;
use std::path::PathBuf;

pub struct Paru {
    helper: String,
    dbpath: PathBuf,
}

impl Paru {
    pub fn new() -> Self {
        // Check for helper binary in PATH
        let helper = if which_helper("paru") {
            "paru".to_string()
        } else if which_helper("yay") {
            "yay".to_string()
        } else {
            "paru".to_string()
        };

        // Detect pacman database path (same as pacman, since AUR uses pacman's local db)
        let mut possible_paths = vec![PathBuf::from("/var/lib/pacman")];

        // Detect Bedrock Linux and scan all strata
        if PathBuf::from("/bedrock/strata").exists() {
            if let Ok(entries) = fs::read_dir("/bedrock/strata") {
                for entry in entries.filter_map(|e| e.ok()) {
                    let pacman_path = PathBuf::from("/bedrock/strata")
                        .join(entry.file_name())
                        .join("var/lib/pacman");
                    possible_paths.push(pacman_path);
                }
            }
        }

        let dbpath = possible_paths
            .iter()
            .find(|p| p.exists() && p.join("local").exists())
            .cloned()
            .unwrap_or_else(|| PathBuf::from("/var/lib/pacman"));

        Self { helper, dbpath }
    }

    // Parse local database to get foreign (AUR) packages
    // A package is foreign if it's installed but NOT in any sync database
    // CRITICAL: Check ALL Bedrock strata
    fn parse_foreign_packages(&self) -> Result<Vec<Package>> {
        // CRITICAL: Check ALL Bedrock strata for installed packages
        let mut all_local_paths = Vec::new();
        let mut all_sync_paths = Vec::new();
        
        // Add primary paths
        let primary_local = self.dbpath.join("local");
        let primary_sync = self.dbpath.join("sync");
        if primary_local.exists() {
            all_local_paths.push(primary_local);
        }
        if primary_sync.exists() {
            all_sync_paths.push(primary_sync);
        }
        
        // Check ALL Bedrock strata
        if PathBuf::from("/bedrock/strata").exists() {
            if let Ok(entries) = fs::read_dir("/bedrock/strata") {
                for entry in entries.filter_map(|e| e.ok()) {
                    let stratum_pacman_path = PathBuf::from("/bedrock/strata")
                        .join(entry.file_name())
                        .join("var/lib/pacman");
                    let stratum_local = stratum_pacman_path.join("local");
                    let stratum_sync = stratum_pacman_path.join("sync");
                    if stratum_local.exists() {
                        all_local_paths.push(stratum_local);
                    }
                    if stratum_sync.exists() {
                        all_sync_paths.push(stratum_sync);
                    }
                }
            }
        }
        
        if all_local_paths.is_empty() {
            return Ok(Vec::new());
        }

        let mut packages = Vec::new();

        // Build set of all packages in sync databases (official repos) from ALL strata
        let mut official_pkgs = std::collections::HashSet::new();

        for sync_path in &all_sync_paths {
            if let Ok(entries) = fs::read_dir(sync_path) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();

                    // Only process .db files (core.db, extra.db, multilib.db, etc.)
                    if path.extension().and_then(|s| s.to_str()) == Some("db") {
                        if let Ok(file) = fs::File::open(&path) {
                            use flate2::read::GzDecoder;
                            use std::io::Read;
                            use tar::Archive;

                            let decoder = GzDecoder::new(file);
                            let mut archive = Archive::new(decoder);

                            // Each package in the archive has a desc file with its NAME
                            if let Ok(archive_entries) = archive.entries() {
                                for entry in archive_entries {
                                    if let Ok(mut entry) = entry {
                                        if let Ok(entry_path) = entry.path() {
                                            let path_str = entry_path.to_string_lossy();

                                            // Look for desc files: "pkgname-version/desc"
                                            if path_str.ends_with("/desc") {
                                                let mut content = String::new();
                                                if entry.read_to_string(&mut content).is_ok() {
                                                    // Parse the desc file to get NAME field
                                                    let mut in_name_section = false;
                                                    for line in content.lines() {
                                                        let line = line.trim();
                                                        if line == "%NAME%" {
                                                            in_name_section = true;
                                                        } else if in_name_section
                                                            && !line.is_empty()
                                                            && !line.starts_with('%')
                                                        {
                                                            official_pkgs.insert(line.to_string());
                                                            break;
                                                        } else if line.starts_with('%') {
                                                            in_name_section = false;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Now check all installed packages from ALL strata - if not in official_pkgs, it's foreign (AUR)
        for local_path in &all_local_paths {
            if let Ok(entries) = fs::read_dir(local_path) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();

                    if !path.is_dir() {
                        continue;
                    }

                    let desc_file = path.join("desc");
                    if !desc_file.exists() {
                        continue;
                    }

                    if let Ok(content) = fs::read_to_string(desc_file) {
                        let mut pkg_name = None;
                        let mut pkg_version = None;
                        let mut pkg_desc = None;
                        let mut current_field = String::new();

                        for line in content.lines() {
                            let line = line.trim();
                            if line.starts_with('%') && line.ends_with('%') {
                                current_field = line.trim_matches('%').to_string();
                            } else if !line.is_empty() && !current_field.is_empty() {
                                match current_field.as_str() {
                                    "NAME" if pkg_name.is_none() => pkg_name = Some(line.to_string()),
                                    "VERSION" if pkg_version.is_none() => pkg_version = Some(line.to_string()),
                                    "DESC" if pkg_desc.is_none() => pkg_desc = Some(line.to_string()),
                                    _ => {}
                                }
                            }
                        }

                        if let Some(name) = pkg_name {
                            // If package is NOT in any sync database, it's foreign (AUR)
                            if !official_pkgs.contains(&name) {
                                packages.push(Package {
                                    name,
                                    version: pkg_version,
                                    description: pkg_desc.unwrap_or_default(),
                                    repo: "aur".to_string(),
                                    manager: "aur".to_string(),
                                    installed: true,
                                    homepage: String::new(),
                                    license: String::new(),
                                    size: None,
                                });
                            }
                        }
                    }
                }
            }
        }

        Ok(packages)
    }
}

// Helper function to check if a binary exists in PATH
fn which_helper(name: &str) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(':') {
            let full_path = std::path::Path::new(dir).join(name);
            if full_path.exists() && full_path.is_file() {
                return true;
            }
        }
    }
    false
}

impl PackageManager for Paru {
    fn name(&self) -> &str {
        "aur"
    }

    fn is_available(&self) -> bool {
        // Check if helper binary exists in PATH
        which_helper(&self.helper)
    }

    fn list_all(&self) -> Result<Vec<Package>> {
        // Load from redb cache
        use crate::cache::CacheManager;
        let cache = CacheManager::new()?;
        if let Some(packages) = cache.get("aur_all")? {
            return Ok(packages);
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
                if let (Some(name), Some(desc)) =
                    (result["Name"].as_str(), result["Description"].as_str())
                {
                    packages.push(Package {
                        name: name.to_string(),
                        version: result["Version"].as_str().map(|s| s.to_string()),
                        description: desc.to_string(),
                        repo: "aur".to_string(),
                        manager: "aur".to_string(),
                        installed: false,
                        homepage: String::new(),
                        license: String::new(),
                        size: None,
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

    fn remove_command(&self, packages: &[&Package]) -> String {
        let pkg_names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        format!("{} -R {}", self.helper, pkg_names.join(" "))
    }

    #[allow(dead_code)]
    fn needs_sudo(&self) -> bool {
        false
    }
}
