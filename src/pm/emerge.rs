use super::{Package, PackageManager};
use eyre::Result;
use std::fs;
use std::path::PathBuf;

pub struct Emerge {
    portage_dir: PathBuf,
    vdb_dir: PathBuf,
}

impl Emerge {
    pub fn new() -> Self {
        let mut possible_portage = vec![
            PathBuf::from("/var/db/repos/gentoo"),
            PathBuf::from("/usr/portage"),
        ];

        let mut possible_vdb = vec![PathBuf::from("/var/db/pkg")];

        // Detect Bedrock Linux and scan all strata
        if PathBuf::from("/bedrock/strata").exists() {
            if let Ok(entries) = fs::read_dir("/bedrock/strata") {
                for entry in entries.filter_map(|e| e.ok()) {
                    let stratum = entry.file_name();
                    possible_portage.push(
                        PathBuf::from("/bedrock/strata")
                            .join(&stratum)
                            .join("var/db/repos/gentoo"),
                    );
                    possible_portage.push(
                        PathBuf::from("/bedrock/strata")
                            .join(&stratum)
                            .join("usr/portage"),
                    );
                    possible_vdb.push(
                        PathBuf::from("/bedrock/strata")
                            .join(&stratum)
                            .join("var/db/pkg"),
                    );
                }
            }
        }

        let portage_dir = possible_portage
            .iter()
            .find(|p| p.exists())
            .cloned()
            .unwrap_or_else(|| PathBuf::from("/var/db/repos/gentoo"));

        let vdb_dir = possible_vdb
            .iter()
            .find(|p| p.exists())
            .cloned()
            .unwrap_or_else(|| PathBuf::from("/var/db/pkg"));

        Self {
            portage_dir,
            vdb_dir,
        }
    }

    // Parse installed packages from VDB (Portage database)
    // VDB structure: /var/db/pkg/category/package-version/
    // Each package dir contains files: DESCRIPTION, SLOT, repository, etc.
    fn parse_vdb(&self) -> Result<Vec<Package>> {
        use rayon::prelude::*;

        if !self.vdb_dir.exists() {
            return Ok(Vec::new());
        }

        // Collect all package paths first
        let mut pkg_paths = Vec::new();

        for category_entry in fs::read_dir(&self.vdb_dir)? {
            let category_entry = category_entry?;
            let category_path = category_entry.path();

            if !category_path.is_dir() {
                continue;
            }

            let category_name = category_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            for pkg_entry in fs::read_dir(&category_path)? {
                if let Ok(pkg_entry) = pkg_entry {
                    let pkg_path = pkg_entry.path();
                    if pkg_path.is_dir() {
                        pkg_paths.push((category_name.clone(), pkg_path));
                    }
                }
            }
        }

        // Parse in parallel for speed
        let packages: Vec<Package> = pkg_paths
            .par_iter()
            .filter_map(|(category, pkg_path)| {
                // Read metadata files from VDB
                let pf = pkg_path.join("PF");
                let desc_file = pkg_path.join("DESCRIPTION");
                let homepage_file = pkg_path.join("HOMEPAGE");
                let license_file = pkg_path.join("LICENSE");
                let size_file = pkg_path.join("SIZE");
                let repo_file = pkg_path.join("repository");

                // PF contains the full package name without category
                let pkg_name = if pf.exists() {
                    fs::read_to_string(pf).ok()?.trim().to_string()
                } else {
                    // Fallback: use directory name
                    pkg_path.file_name()?.to_str()?.to_string()
                };

                // Parse package name from PF (strip version)
                let (name, version) = parse_portage_pf(&pkg_name);

                let description = desc_file
                    .exists()
                    .then(|| fs::read_to_string(desc_file).ok())
                    .flatten()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();

                let homepage = homepage_file
                    .exists()
                    .then(|| fs::read_to_string(homepage_file).ok())
                    .flatten()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();

                let license = license_file
                    .exists()
                    .then(|| fs::read_to_string(license_file).ok())
                    .flatten()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();

                let size = size_file
                    .exists()
                    .then(|| fs::read_to_string(size_file).ok())
                    .flatten()
                    .and_then(|s| s.trim().parse::<u64>().ok());

                let repo = repo_file
                    .exists()
                    .then(|| fs::read_to_string(repo_file).ok())
                    .flatten()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|| "gentoo".to_string());

                // Package name without slot for matching
                let full_name = format!("{}/{}", category, name);

                Some(Package {
                    name: full_name,
                    version: Some(version),
                    description,
                    repo,
                    manager: "emerge".to_string(),
                    installed: true,
                    homepage,
                    license,
                    size,
                })
            })
            .collect();

        Ok(packages)
    }

    // Parse available packages from portage tree
    fn parse_portage_tree(&self) -> Result<Vec<Package>> {
        let mut packages = Vec::new();

        if !self.portage_dir.exists() {
            return Ok(packages);
        }

        // Portage tree structure: /var/db/repos/gentoo/category/package/
        for category_entry in fs::read_dir(&self.portage_dir)? {
            let category_entry = category_entry?;
            let category_path = category_entry.path();

            if category_path.is_dir() {
                let category_name = category_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");

                // Skip metadata directories
                if category_name.starts_with('.')
                    || category_name == "metadata"
                    || category_name == "profiles"
                {
                    continue;
                }

                for pkg_entry in fs::read_dir(&category_path)? {
                    let pkg_entry = pkg_entry?;
                    let pkg_path = pkg_entry.path();

                    if pkg_path.is_dir() {
                        let pkg_name = pkg_path.file_name().and_then(|s| s.to_str()).unwrap_or("");

                        // Find the latest ebuild file to extract metadata
                        let mut description = String::new();
                        let mut homepage = String::new();
                        let mut license = String::new();

                        if let Ok(entries) = fs::read_dir(&pkg_path) {
                            // Find any .ebuild file
                            for entry in entries.filter_map(|e| e.ok()) {
                                let entry_path = entry.path();
                                if entry_path.extension().and_then(|s| s.to_str()) == Some("ebuild")
                                {
                                    if let Ok(content) = fs::read_to_string(&entry_path) {
                                        // Extract DESCRIPTION, HOMEPAGE, LICENSE from ebuild
                                        for line in content.lines() {
                                            let line = line.trim();
                                            if line.starts_with("DESCRIPTION=") {
                                                description =
                                                    line[12..].trim_matches('"').to_string();
                                            } else if line.starts_with("HOMEPAGE=") {
                                                homepage = line[9..].trim_matches('"').to_string();
                                            } else if line.starts_with("LICENSE=") {
                                                license = line[8..].trim_matches('"').to_string();
                                            }

                                            // Stop after finding all three
                                            if !description.is_empty()
                                                && !homepage.is_empty()
                                                && !license.is_empty()
                                            {
                                                break;
                                            }
                                        }
                                    }
                                    // Use first ebuild found
                                    break;
                                }
                            }
                        }

                        packages.push(Package {
                            name: format!("{}/{}", category_name, pkg_name),
                            version: None,
                            description,
                            repo: "gentoo".to_string(),
                            manager: "emerge".to_string(),
                            installed: false,
                            homepage,
                            license,
                            size: None,
                        });
                    }
                }
            }
        }

        Ok(packages)
    }
}

// Parse portage PF (package-version-revision) format
// Examples: "firefox-120.0.1-r1", "python-3.11.6", "lib32-mesa-23.3.1"
// This mimics portage's pkgsplit() function
fn parse_portage_pf(pf: &str) -> (String, String) {
    // Find the last '-' followed by a version number
    let chars: Vec<char> = pf.chars().collect();
    let mut split_pos = None;

    for i in (0..chars.len()).rev() {
        if chars[i] == '-' && i + 1 < chars.len() {
            // Check if what follows looks like a version (starts with digit)
            if chars[i + 1].is_ascii_digit() {
                // Make sure there's a package name before this
                if i > 0 {
                    split_pos = Some(i);
                    break;
                }
            }
        }
    }

    if let Some(pos) = split_pos {
        let name = pf[..pos].to_string();
        let version = pf[pos + 1..].to_string();
        (name, version)
    } else {
        // No version found, return as-is
        (pf.to_string(), String::new())
    }
}

impl PackageManager for Emerge {
    fn name(&self) -> &str {
        "emerge"
    }

    fn is_available(&self) -> bool {
        self.vdb_dir.exists()
    }

    fn list_all(&self) -> Result<Vec<Package>> {
        // Parse portage tree directly - no commands
        self.parse_portage_tree()
    }

    fn list_installed(&self) -> Result<Vec<Package>> {
        self.parse_vdb()
    }

    fn search(&self, _query: &str) -> Result<Vec<Package>> {
        self.list_all()
    }

    fn install_command(&self, packages: &[&Package]) -> String {
        let pkg_names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        format!("sudo emerge {}", pkg_names.join(" "))
    }

    fn remove_command(&self, packages: &[&Package]) -> String {
        let pkg_names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        // Use -C (unmerge) to remove packages
        format!("sudo emerge -C {}", pkg_names.join(" "))
    }

    fn needs_sudo(&self) -> bool {
        true
    }
}
