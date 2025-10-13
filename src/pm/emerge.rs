use super::{Package, PackageManager};
use eyre::Result;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub struct Emerge {
    portage_dir: PathBuf,
    vdb_dir: PathBuf,
}

impl Emerge {
    pub fn new() -> Self {
        // Check for bedrock linux stratum paths
        let possible_portage = vec![
            PathBuf::from("/var/db/repos/gentoo"),
            PathBuf::from("/usr/portage"),
            PathBuf::from("/bedrock/strata/gentoo/var/db/repos/gentoo"),
        ];
        
        let possible_vdb = vec![
            PathBuf::from("/var/db/pkg"),
            PathBuf::from("/bedrock/strata/gentoo/var/db/pkg"),
        ];
        
        let portage_dir = possible_portage
            .into_iter()
            .find(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("/var/db/repos/gentoo"));
        
        let vdb_dir = possible_vdb
            .into_iter()
            .find(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("/var/db/pkg"));
        
        Self {
            portage_dir,
            vdb_dir,
        }
    }
    
    // Parse installed packages from VDB (Portage database) - OPTIMIZED
    fn parse_vdb(&self) -> Result<Vec<Package>> {
        use rayon::prelude::*;
        
        if !self.vdb_dir.exists() {
            return Ok(Vec::new());
        }
        
        // Collect all package paths first
        let mut pkg_paths = Vec::new();
        
        // VDB structure: /var/db/pkg/category/package-version/
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
                        if let Some(pkg_full_name) = pkg_path.file_name().and_then(|s| s.to_str()) {
                            pkg_paths.push((category_name.clone(), pkg_full_name.to_string()));
                        }
                    }
                }
            }
        }
        
        // Parse in parallel - FAST
        let packages: Vec<Package> = pkg_paths
            .par_iter()
            .map(|(category, pkg_full_name)| {
                // Parse package name (strip version)
                let (name, _version) = parse_portage_name(pkg_full_name);
                
                Package {
                    name: format!("{}/{}", category, name),
                    version: None,
                    description: String::new(),
                    repo: "installed".to_string(),
                    manager: "emerge".to_string(),
                    installed: true,
                }
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
                if category_name.starts_with('.') || category_name == "metadata" || category_name == "profiles" {
                    continue;
                }
                
                for pkg_entry in fs::read_dir(&category_path)? {
                    let pkg_entry = pkg_entry?;
                    let pkg_path = pkg_entry.path();
                    
                    if pkg_path.is_dir() {
                        let pkg_name = pkg_path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("");
                        
                        // Read metadata.xml for description
                        let metadata_file = pkg_path.join("metadata.xml");
                        let description = if metadata_file.exists() {
                            // Simple XML parsing - just extract text between <description> tags
                            if let Ok(content) = fs::read_to_string(metadata_file) {
                                extract_description_from_xml(&content)
                            } else {
                                String::new()
                            }
                        } else {
                            String::new()
                        };
                        
                        packages.push(Package {
                            name: format!("{}/{}", category_name, pkg_name),
                            version: None,
                            description,
                            repo: "gentoo".to_string(),
                            manager: "emerge".to_string(),
                            installed: false,
                        });
                    }
                }
            }
        }
        
        Ok(packages)
    }
}

// Helper to parse portage package name-version format
fn parse_portage_name(full_name: &str) -> (String, String) {
    // Format: package-name-1.2.3-r1
    // Need to split at the last version-like component
    let parts: Vec<&str> = full_name.rsplitn(2, '-').collect();
    if parts.len() == 2 {
        // Check if the last part looks like a version
        if parts[0].chars().next().map_or(false, |c| c.is_numeric()) {
            return (parts[1].to_string(), parts[0].to_string());
        }
    }
    (full_name.to_string(), String::new())
}

// Simple XML description extractor
fn extract_description_from_xml(xml: &str) -> String {
    if let Some(start) = xml.find("<description>") {
        if let Some(end) = xml[start..].find("</description>") {
            let desc = &xml[start + 13..start + end];
            return desc.trim().to_string();
        }
    }
    String::new()
}

impl PackageManager for Emerge {
    fn name(&self) -> &str {
        "emerge"
    }
    
    fn is_available(&self) -> bool {
        self.vdb_dir.exists() || Command::new("emerge").arg("--version").output().is_ok()
    }
    
    fn list_all(&self) -> Result<Vec<Package>> {
        // Try pmux cache first
        if let Some(cache_dir) = dirs::cache_dir() {
            let portage_list = cache_dir.join("pmux").join("repos").join("portage-packages.txt");
            if portage_list.exists() {
                let content = fs::read_to_string(portage_list)?;
                let packages: Vec<Package> = content
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(|line| {
                        // Format: category/package-version
                        let line = line.trim();
                        Package {
                            name: line.to_string(),
                            version: None,
                            description: String::new(),
                            repo: "portage".to_string(),
                            manager: "emerge".to_string(),
                            installed: false,
                        }
                    })
                    .collect();
                
                return Ok(packages);
            }
        }
        
        // Fallback: Use eix if available
        if Command::new("eix").arg("--version").output().is_ok() {
            let output = Command::new("eix")
                .args(&["-c", "--only-names"])
                .output()?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            
            let packages = stdout
                .lines()
                .filter(|line| !line.is_empty())
                .map(|line| Package {
                    name: line.trim().to_string(),
                    version: None,
                    description: String::new(),
                    repo: "gentoo".to_string(),
                    manager: "emerge".to_string(),
                    installed: false,
                })
                .collect();
            
            Ok(packages)
        } else {
            // Last resort: parse portage tree directly
            self.parse_portage_tree()
        }
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
    
    fn needs_sudo(&self) -> bool {
        true
    }
}
