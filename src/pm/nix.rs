use super::{Package, PackageManager};
use eyre::Result;
use std::fs;
use std::path::PathBuf;

pub struct Nix {
    profile_path: PathBuf,
}

impl Nix {
    pub fn new() -> Self {
        // Check for bedrock linux stratum paths and standard nix paths
        let possible_paths = vec![
            PathBuf::from("/nix/var/nix/profiles/default"),
            PathBuf::from(format!(
                "{}/.nix-profile",
                std::env::var("HOME").unwrap_or_default()
            )),
            PathBuf::from("/bedrock/strata/nixos/nix/var/nix/profiles/default"),
        ];

        let profile_path = possible_paths
            .into_iter()
            .find(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("/nix/var/nix/profiles/default"));

        Self { profile_path }
    }

    // Parse installed packages using nix profile list --json
    // Platform-agnostic and works across all Bedrock strata
    fn parse_profile(&self) -> Result<Vec<Package>> {
        let mut packages = Vec::new();

        // Use nix profile list --json - works across all Bedrock strata
        // This command automatically detects and lists all profiles (system and user)
        let output = std::process::Command::new("nix")
            .args(&["profile", "list", "--json"])
            .output()?;

        if !output.status.success() {
            // Fallback to manifest.json parsing if command fails
            return self.parse_profile_manifest();
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Ok(profile_list) = serde_json::from_str::<serde_json::Value>(&stdout) {
            // nix profile list --json returns an object with "elements" key
            // Each element has an "index" and "storePath", and may have "name" or other metadata
            if let Some(elements) = profile_list["elements"].as_object() {
                // Elements is an object where keys are profile element names (package names)
                for (pkg_name, element) in elements {
                    // The key IS the package name (e.g., "hyprland", "nano")
                    // This matches the manifest.json structure
                    let version = element["storePath"]
                        .as_str()
                        .and_then(|path| {
                            // Try to extract version from store path: /nix/store/hash-pkgname-version
                            path.split('/').last().and_then(|name_part| {
                                let parts: Vec<&str> = name_part.split('-').collect();
                                if parts.len() >= 2 {
                                    parts.last().map(|s| s.to_string())
                                } else {
                                    None
                                }
                            })
                        });
                    
                    packages.push(Package {
                        name: pkg_name.clone(),
                        version,
                        description: String::new(),
                        repo: "nix".to_string(),
                        manager: "nix".to_string(),
                        installed: true,
                        homepage: String::new(),
                        license: String::new(),
                        size: None,
                    });
                }
            } else if let Some(elements) = profile_list.as_array() {
                // Fallback: if it's an array, try to extract from storePath
                for element in elements {
                    if let Some(store_path) = element["storePath"].as_str() {
                        if let Some(name_part) = store_path.split('/').last() {
                            let parts: Vec<&str> = name_part.split('-').collect();
                            if parts.len() >= 2 {
                                let pkg_name = parts[1..].join("-");
                                let version = parts.last().map(|s| s.to_string());
                                
                                packages.push(Package {
                                    name: pkg_name,
                                    version,
                                    description: String::new(),
                                    repo: "nix".to_string(),
                                    manager: "nix".to_string(),
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

    // Fallback: Parse manifest.json files (for when nix command is not available)
    fn parse_profile_manifest(&self) -> Result<Vec<Package>> {
        let mut packages = Vec::new();
        let mut all_profile_paths = Vec::new();

        // Add primary profile path
        all_profile_paths.push(self.profile_path.clone());
        
        // Add home profile
        if let Ok(home) = std::env::var("HOME") {
            let home_profile = PathBuf::from(home).join(".nix-profile");
            if home_profile.exists() {
                all_profile_paths.push(home_profile);
            }
        }

        // CRITICAL: Check ALL Bedrock strata for Nix profiles
        if PathBuf::from("/bedrock/strata").exists() {
            if let Ok(entries) = fs::read_dir("/bedrock/strata") {
                for entry in entries.filter_map(|e| e.ok()) {
                    let stratum_nix_path = PathBuf::from("/bedrock/strata")
                        .join(entry.file_name())
                        .join("nix/var/nix/profiles/default");
                    if stratum_nix_path.exists() {
                        all_profile_paths.push(stratum_nix_path);
                    }
                    
                    // Also check for user profile in stratum
                    if let Ok(home) = std::env::var("HOME") {
                        let stratum_user_profile = PathBuf::from("/bedrock/strata")
                            .join(entry.file_name())
                            .join(home.trim_start_matches('/'))
                            .join(".nix-profile");
                        if stratum_user_profile.exists() {
                            all_profile_paths.push(stratum_user_profile);
                        }
                    }
                }
            }
        }

        // Parse ALL profiles found
        for profile_path in all_profile_paths {
            let manifest_path = profile_path.join("manifest.json");
            if manifest_path.exists() {
                if let Ok(content) = fs::read_to_string(&manifest_path) {
                    if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(elements) = manifest["elements"].as_object() {
                            for (key, _element) in elements {
                                // Use the key as the package name
                                let pkg_name = key.clone();
                                
                                packages.push(Package {
                                    name: pkg_name,
                                    version: None,
                                    description: String::new(),
                                    repo: "nix".to_string(),
                                    manager: "nix".to_string(),
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

impl PackageManager for Nix {
    fn name(&self) -> &str {
        "nix"
    }

    fn is_available(&self) -> bool {
        PathBuf::from("/nix/store").exists() || PathBuf::from("/nix/var/nix").exists()
    }

    fn list_all(&self) -> Result<Vec<Package>> {
        // Load from redb cache
        use crate::cache::CacheManager;
        let cache = CacheManager::new()?;
        if let Some(packages) = cache.get("nix_all")? {
            return Ok(packages);
        }
        
        // Fallback: return empty
        Ok(vec![])
    }

    fn list_installed(&self) -> Result<Vec<Package>> {
        self.parse_profile()
    }

    fn search(&self, _query: &str) -> Result<Vec<Package>> {
        self.list_all()
    }

    fn install_command(&self, packages: &[&Package]) -> String {
        let pkg_names: Vec<String> = packages
            .iter()
            .map(|p| format!("nixpkgs#{}", p.name))
            .collect();
        // Use 'add' instead of deprecated 'install', works for all packages (impure and pure)
        format!("nix profile add {} --impure", pkg_names.join(" "))
    }

    fn remove_command(&self, packages: &[&Package]) -> String {
        let pkg_names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        format!("nix profile remove {}", pkg_names.join(" "))
    }

    fn needs_sudo(&self) -> bool {
        false
    }
}
