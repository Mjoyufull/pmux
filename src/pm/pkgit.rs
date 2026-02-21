use super::{Package, PackageManager};
use eyre::Result;
use std::fs;
use std::path::PathBuf;

pub struct Pkgit {
    user_level: bool,
    pkgs_dir: PathBuf,
    repos_file: PathBuf,
}

impl Pkgit {
    pub fn new() -> Self {
        let user_level = detect_user_level();
        
        let (pkgs_dir, repos_file) = if user_level {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
            (
                PathBuf::from(format!("{}/.local/share/pkgit/pkgs", home)),
                PathBuf::from(format!("{}/.config/pkgit/repos/repos", home)),
            )
        } else {
            (
                PathBuf::from("/var/pkgit/pkgs"),
                PathBuf::from("/etc/pkgit/repos/repos"),
            )
        };

        Self {
            user_level,
            pkgs_dir,
            repos_file,
        }
    }

    /// Extract package name from git URL
    fn extract_pkg_name(url: &str) -> String {
        // Handle various git URL formats:
        // - https://github.com/user/repo.git
        // - https://github.com/user/repo
        // - git@github.com:user/repo.git
        
        let url = url.trim();
        let url = url.trim_end_matches(".git");
        
        // Split by '/' or ':'
        let parts: Vec<&str> = url.split(&['/', ':'][..]).collect();
        
        parts
            .last()
            .unwrap_or(&url)
            .to_string()
            .to_lowercase()
    }
}

/// Detect if pkgit is running in user-level mode
fn detect_user_level() -> bool {
    // Check system config first
    if let Ok(content) = fs::read_to_string("/etc/pkgit/config.toml") {
        if let Some(user_level) = parse_user_level(&content) {
            return user_level;
        }
    }
    
    // Check user config
    if let Ok(home) = std::env::var("HOME") {
        let user_config = format!("{}/.config/pkgit/config.toml", home);
        if let Ok(content) = fs::read_to_string(user_config) {
            if let Some(user_level) = parse_user_level(&content) {
                return user_level;
            }
        }
    }
    
    // Default to system-level
    false
}

/// Parse user-level setting from config TOML
fn parse_user_level(content: &str) -> Option<bool> {
    // Simple TOML parsing for [general] user-level = true/false
    // We look for the line "user-level = true" or "user-level = false"
    
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("user-level") {
            if line.contains("true") {
                return Some(true);
            } else if line.contains("false") {
                return Some(false);
            }
        }
    }
    
    None
}

/// Check if pkgit binary exists in PATH
fn which_pkgit() -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(':') {
            let pkgit_path = std::path::Path::new(dir).join("pkgit");
            if pkgit_path.exists() && pkgit_path.is_file() {
                return true;
            }
        }
    }
    false
}

impl PackageManager for Pkgit {
    fn name(&self) -> &str {
        "pkgit"
    }

    fn is_available(&self) -> bool {
        which_pkgit()
    }

    fn list_all(&self) -> Result<Vec<Package>> {
        let mut packages = Vec::new();

        if !self.repos_file.exists() {
            return Ok(packages);
        }

        let content = fs::read_to_string(&self.repos_file)?;
        
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let pkg_name = Self::extract_pkg_name(line);
            
            packages.push(Package {
                name: pkg_name,
                version: None, // pkgit repos file doesn't have version info
                description: String::new(), // pkgit doesn't store descriptions
                repo: "pkgit".to_string(),
                manager: "pkgit".to_string(),
                installed: false,
                homepage: line.to_string(), // Store the git URL as homepage
                license: String::new(),
                size: None,
            });
        }

        Ok(packages)
    }

    fn list_installed(&self) -> Result<Vec<Package>> {
        let mut packages = Vec::new();

        if !self.pkgs_dir.exists() {
            return Ok(packages);
        }

        // Scan /var/pkgit/pkgs/ or ~/.local/share/pkgit/pkgs/
        // Each subdirectory is a package
        for entry in fs::read_dir(&self.pkgs_dir)? {
            let entry = entry?;
            let path = entry.path();
            
            if !path.is_dir() {
                continue;
            }

            let pkg_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();

            if pkg_name.is_empty() {
                continue;
            }

            // Check for version subdirectories (tags)
            let mut versions = Vec::new();
            if let Ok(entries) = fs::read_dir(&path) {
                for version_entry in entries {
                    if let Ok(version_entry) = version_entry {
                        let version_path = version_entry.path();
                        if version_path.is_dir() {
                            if let Some(version_name) = version_path.file_name().and_then(|n| n.to_str()) {
                                versions.push(version_name.to_string());
                            }
                        }
                    }
                }
            }

            // Create package entry for each version, or one with "HEAD" if no versions found
            if versions.is_empty() {
                packages.push(Package {
                    name: pkg_name.clone(),
                    version: Some("HEAD".to_string()),
                    description: String::new(),
                    repo: "pkgit".to_string(),
                    manager: "pkgit".to_string(),
                    installed: true,
                    homepage: String::new(),
                    license: String::new(),
                    size: None,
                });
            } else {
                // For multiple versions, just report the first one (or "HEAD" if present)
                let version = if versions.contains(&"HEAD".to_string()) {
                    "HEAD".to_string()
                } else {
                    versions[0].clone()
                };
                
                packages.push(Package {
                    name: pkg_name,
                    version: Some(version),
                    description: String::new(),
                    repo: "pkgit".to_string(),
                    manager: "pkgit".to_string(),
                    installed: true,
                    homepage: String::new(),
                    license: String::new(),
                    size: None,
                });
            }
        }

        Ok(packages)
    }

    #[allow(dead_code)]
    fn search(&self, _query: &str) -> Result<Vec<Package>> {
        // pkgit doesn't have a native search API
        // Just return all packages and let pmux filter
        self.list_all()
    }

    fn install_command(&self, packages: &[&Package]) -> String {
        let pkg_names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        format!("pkgit install {}", pkg_names.join(" "))
    }

    fn remove_command(&self, packages: &[&Package]) -> String {
        let pkg_names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        format!("pkgit remove {}", pkg_names.join(" "))
    }

    #[allow(dead_code)]
    fn needs_sudo(&self) -> bool {
        !self.user_level
    }
}
