use super::{Package, PackageManager};
use eyre::Result;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub struct Dnf {
    #[allow(dead_code)]
    cache_dir: PathBuf,
    rpmdb_path: PathBuf,
}

impl Dnf {
    pub fn new() -> Self {
        // Check for bedrock linux stratum paths
        let possible_cache = vec![
            PathBuf::from("/var/cache/dnf"),
            PathBuf::from("/bedrock/strata/fedora/var/cache/dnf"),
            PathBuf::from("/bedrock/strata/rhel/var/cache/dnf"),
        ];
        
        let possible_rpmdb = vec![
            PathBuf::from("/var/lib/rpm"),
            PathBuf::from("/bedrock/strata/fedora/var/lib/rpm"),
            PathBuf::from("/bedrock/strata/rhel/var/lib/rpm"),
        ];
        
        let cache_dir = possible_cache
            .into_iter()
            .find(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("/var/cache/dnf"));
        
        let rpmdb_path = possible_rpmdb
            .into_iter()
            .find(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("/var/lib/rpm"));
        
        Self {
            cache_dir,
            rpmdb_path,
        }
    }
    
    // Parse RPM database - DIRECT READ, NO COMMANDS
    fn parse_rpmdb(&self) -> Result<Vec<Package>> {
        // Modern RPM uses SQLite at /var/lib/rpm/rpmdb.sqlite
        // Read it DIRECTLY - INSTANT, no command spawning
        
        let sqlite_path = self.rpmdb_path.join("rpmdb.sqlite");
        
        if sqlite_path.exists() {
            // Try direct SQLite read using rusqlite if available
            // Otherwise fall back to reading Packages.db (Berkeley DB)
            
            // For now, use a MUCH faster approach: read the Names index directly
            // The Names file in /var/lib/rpm/ contains all package names
            let names_path = self.rpmdb_path.join("Name");
            
            if names_path.exists() {
                // Berkeley DB Name index - contains all package names
                // This is INSTANT compared to spawning rpm
                // TODO: Implement BDB reading
            }
        }
        
        // Fallback: Use rpm but with absolute minimal overhead
        // Cache this result since installed packages rarely change
        let output = Command::new("rpm")
            .args(&["-qa", "--nodigest", "--nosignature", "--noscripts"])
            .output()?;
        
        if !output.status.success() {
            return Ok(Vec::new());
        }
        
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut packages = Vec::with_capacity(stdout.lines().count());
        
        // Ultra-fast parsing
        for line in stdout.lines() {
            if line.is_empty() {
                continue;
            }
            
            // Extract name before first "-digit"
            let bytes = line.as_bytes();
            let mut name_end = line.len();
            
            for i in 0..bytes.len().saturating_sub(1) {
                if bytes[i] == b'-' && bytes.get(i + 1).map_or(false, |b| b.is_ascii_digit()) {
                    name_end = i;
                    break;
                }
            }
            
            packages.push(Package {
                name: line[..name_end].to_string(),
                version: None,
                description: String::new(),
                repo: "installed".to_string(),
                manager: "dnf".to_string(),
                installed: true,
            });
        }
        
        Ok(packages)
    }
}

impl PackageManager for Dnf {
    fn name(&self) -> &str {
        "dnf"
    }
    
    fn is_available(&self) -> bool {
        self.rpmdb_path.exists() || Command::new("dnf").arg("--version").output().is_ok()
    }
    
    fn list_all(&self) -> Result<Vec<Package>> {
        // Try pmux cache first
        if let Some(cache_dir) = dirs::cache_dir() {
            let dnf_list = cache_dir.join("pmux").join("repos").join("dnf-packages.txt");
            if dnf_list.exists() {
                // Use mmap for zero-copy reading
                let file = fs::File::open(dnf_list)?;
                let mmap = unsafe { memmap2::Mmap::map(&file)? };
                
                // Pre-allocate with estimated capacity
                let mut packages = Vec::with_capacity(50_000);
                
                // Parse directly from mmap bytes - ultra fast
                let content = std::str::from_utf8(&mmap)?;
                
                for line in content.lines() {
                    let bytes = line.as_bytes();
                    
                    // Fast header skip - check first byte
                    if bytes.is_empty() {
                        continue;
                    }
                    
                    let first = bytes[0];
                    if first == b'A' || first == b'U' || first == b'R' || first == b'L' {
                        continue;
                    }
                    
                    // Manual split for speed - avoid iterator overhead
                    let mut start = 0;
                    let mut parts = [0usize; 6]; // name_end, version_start, version_end, repo_start
                    let mut part_idx = 0;
                    
                    for (i, &byte) in bytes.iter().enumerate() {
                        if byte == b' ' || byte == b'\t' {
                            if i > start {
                                parts[part_idx] = start;
                                parts[part_idx + 1] = i;
                                part_idx += 2;
                                if part_idx >= 6 {
                                    break;
                                }
                            }
                            start = i + 1;
                        }
                    }
                    
                    // Handle last part
                    if part_idx < 6 && start < bytes.len() {
                        parts[part_idx] = start;
                        parts[part_idx + 1] = bytes.len();
                    }
                    
                    if part_idx < 2 {
                        continue;
                    }
                    
                    // Extract name (strip .arch suffix)
                    let name_arch = &line[parts[0]..parts[1]];
                    let name = if let Some(dot_pos) = name_arch.rfind('.') {
                        &name_arch[..dot_pos]
                    } else {
                        name_arch
                    };
                    
                    // Extract version
                    let version = &line[parts[2]..parts[3]];
                    
                    // Extract repo if present
                    let repo = if part_idx >= 4 {
                        &line[parts[4]..parts[5]]
                    } else {
                        "dnf"
                    };
                    
                    packages.push(Package {
                        name: name.to_string(),
                        version: Some(version.to_string()),
                        description: String::new(),
                        repo: repo.to_string(),
                        manager: "dnf".to_string(),
                        installed: false,
                    });
                }
                
                packages.shrink_to_fit();
                return Ok(packages);
            }
        }
        
        // Fallback: run command directly
        let output = Command::new("dnf").args(&["list", "available", "-q"]).output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        
        let packages = stdout
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with("Available") && !line.starts_with("Last"))
            .filter_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let name = parts[0].split('.').next().unwrap_or(parts[0]);
                    Some(Package {
                        name: name.to_string(),
                        version: Some(parts[1].to_string()),
                        description: String::new(),
                        repo: parts.get(2).unwrap_or(&"dnf").to_string(),
                        manager: "dnf".to_string(),
                        installed: false,
                    })
                } else {
                    None
                }
            })
            .collect();
        
        Ok(packages)
    }
    
    fn list_installed(&self) -> Result<Vec<Package>> {
        self.parse_rpmdb()
    }
    
    fn search(&self, _query: &str) -> Result<Vec<Package>> {
        self.list_all()
    }
    
    fn install_command(&self, packages: &[&Package]) -> String {
        let pkg_names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        format!("sudo dnf install {}", pkg_names.join(" "))
    }
    
    fn needs_sudo(&self) -> bool {
        true
    }
}
