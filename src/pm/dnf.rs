use super::{Package, PackageManager};
use eyre::Result;
use std::fs;
use std::path::PathBuf;

pub struct Dnf {
    #[allow(dead_code)]
    cache_dir: PathBuf,
    rpmdb_paths: Vec<PathBuf>,
}

impl Dnf {
    pub fn new() -> Self {
        // Check for bedrock linux stratum paths
        let possible_cache = vec![
            PathBuf::from("/var/cache/dnf"),
            PathBuf::from("/bedrock/strata/fedora/var/cache/dnf"),
            PathBuf::from("/bedrock/strata/rhel/var/cache/dnf"),
        ];

        // Modern Fedora uses /usr/lib/sysimage/rpm, older uses /var/lib/rpm
        let mut possible_rpmdb = vec![
            PathBuf::from("/usr/lib/sysimage/rpm"),
            PathBuf::from("/var/lib/rpm"),
        ];

        // Check bedrock strata
        if PathBuf::from("/bedrock/strata").exists() {
            if let Ok(entries) = fs::read_dir("/bedrock/strata") {
                for entry in entries.filter_map(|e| e.ok()) {
                    let stratum = entry.file_name();
                    possible_rpmdb.push(
                        PathBuf::from("/bedrock/strata")
                            .join(&stratum)
                            .join("usr/lib/sysimage/rpm"),
                    );
                    possible_rpmdb.push(
                        PathBuf::from("/bedrock/strata")
                            .join(&stratum)
                            .join("var/lib/rpm"),
                    );
                }
            }
        }

        let cache_dir = possible_cache
            .into_iter()
            .find(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("/var/cache/dnf"));

        // Find all existing RPM databases
        let rpmdb_paths: Vec<PathBuf> = possible_rpmdb
            .into_iter()
            .filter(|p| p.join("rpmdb.sqlite").exists() || p.join("Packages").exists())
            .collect();

        Self {
            cache_dir,
            rpmdb_paths,
        }
    }

    // Parse installed packages using dnf list --installed
    // Platform-agnostic and works across all Bedrock strata
    fn parse_installed(&self) -> Result<Vec<Package>> {
        let mut all_packages = Vec::new();

        // Use dnf list --installed - works across all Bedrock strata
        // Output format: "package.arch version repo"
        // Example: "nano.x86_64 2.9.8-1.fc42 @System"
        let output = std::process::Command::new("dnf")
            .args(&["list", "--installed", "--quiet"])
            .output()?;

        if !output.status.success() {
            return Ok(Vec::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("Installed") || line.starts_with("Loaded") {
                continue;
            }

            // Parse: "package.arch version repo"
            // Split by whitespace - first part is package name (may have .arch suffix)
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }

            // Extract package name (remove .arch suffix if present)
            let pkg_name_full = parts[0];
            let pkg_name = pkg_name_full.split('.').next().unwrap_or(pkg_name_full).to_string();

            // Extract version (second part)
            let version = parts.get(1).map(|s| s.to_string());

            // Extract repo (third part, optional)
            let repo = parts.get(2).map(|s| s.to_string()).unwrap_or_else(|| "installed".to_string());

            all_packages.push(Package {
                name: pkg_name,
                version,
                description: String::new(),
                repo,
                manager: "dnf".to_string(),
                installed: true,
                homepage: String::new(),
                license: String::new(),
                size: None,
            });
        }

        Ok(all_packages)
    }
}

impl PackageManager for Dnf {
    fn name(&self) -> &str {
        "dnf"
    }

    fn is_available(&self) -> bool {
        !self.rpmdb_paths.is_empty()
    }

    fn list_all(&self) -> Result<Vec<Package>> {
        // Load from redb cache
        use crate::cache::CacheManager;
        let cache = CacheManager::new()?;
        if let Some(packages) = cache.get("dnf_all")? {
            return Ok(packages);
        }
        
        // No cache available - return empty list
        // Available packages should be synced with `pmux -Sy`
        Ok(vec![])
    }

    fn list_installed(&self) -> Result<Vec<Package>> {
        self.parse_installed()
    }

    fn search(&self, _query: &str) -> Result<Vec<Package>> {
        self.list_all()
    }

    fn install_command(&self, packages: &[&Package]) -> String {
        let pkg_names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        format!("sudo dnf install {}", pkg_names.join(" "))
    }

    fn remove_command(&self, packages: &[&Package]) -> String {
        let pkg_names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        format!("sudo dnf remove {}", pkg_names.join(" "))
    }

    fn needs_sudo(&self) -> bool {
        true
    }
}
