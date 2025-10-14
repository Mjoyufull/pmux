use super::{Package, PackageManager};
use eyre::Result;
use serde_json;
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

    // Parse nix profile manifest
    fn parse_profile(&self) -> Result<Vec<Package>> {
        let mut packages = Vec::new();

        // Try new-style nix profile (manifest.json)
        let manifest_path = self.profile_path.join("manifest.json");
        if manifest_path.exists() {
            let content = fs::read_to_string(manifest_path)?;
            if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(elements) = manifest["elements"].as_object() {
                    for (_key, element) in elements {
                        if let Some(store_paths) = element["storePaths"].as_array() {
                            for store_path in store_paths {
                                if let Some(path_str) = store_path.as_str() {
                                    // Extract package name from store path
                                    // Format: /nix/store/hash-name-version
                                    if let Some(name_part) = path_str.split('/').last() {
                                        if let Some(name) = name_part.split('-').nth(1) {
                                            packages.push(Package {
                                                name: name.to_string(),
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
        // Try pmux cache first
        if let Some(cache_dir) = dirs::cache_dir() {
            let nix_json = cache_dir
                .join("pmux")
                .join("repos")
                .join("nix-packages.json");
            if nix_json.exists() {
                let content = fs::read_to_string(nix_json)?;
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                    let mut packages = Vec::new();

                    // The JSON structure is: { "packages": { "attr.path": { "pname": "...", "version": "..." } } }
                    if let Some(pkgs_obj) = json.get("packages").and_then(|p| p.as_object()) {
                        for (_attr_path, pkg_data) in pkgs_obj {
                            if let Some(pname) = pkg_data.get("pname").and_then(|p| p.as_str()) {
                                let version = pkg_data.get("version").and_then(|v| v.as_str());
                                let description = pkg_data
                                    .get("description")
                                    .and_then(|d| d.as_str())
                                    .unwrap_or("");

                                packages.push(Package {
                                    name: pname.to_string(),
                                    version: version.map(|s| s.to_string()),
                                    description: description.to_string(),
                                    repo: "nixpkgs".to_string(),
                                    manager: "nix".to_string(),
                                    installed: false,
                                    homepage: String::new(),
                                    license: String::new(),
                                    size: None,
                                });
                            }
                        }
                    }

                    if !packages.is_empty() {
                        return Ok(packages);
                    }
                }
            }
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
        format!("nix profile install {} --impure", pkg_names.join(" "))
    }

    fn remove_command(&self, packages: &[&Package]) -> String {
        let pkg_names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        format!("nix profile remove {}", pkg_names.join(" "))
    }

    fn needs_sudo(&self) -> bool {
        false
    }
}
