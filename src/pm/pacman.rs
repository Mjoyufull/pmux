use super::{Package, PackageManager};
use eyre::Result;
use flate2::read::GzDecoder;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use tar::Archive;

pub struct Pacman {
    dbpath: PathBuf,
}

impl Pacman {
    pub fn new() -> Self {
        let mut possible_paths = vec![PathBuf::from("/var/lib/pacman")];

        // Detect Bedrock Linux and scan all strata
        if PathBuf::from("/bedrock/strata").exists() {
            // Read all strata directories
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
            .find(|p| p.exists() && p.join("sync").exists())
            .cloned()
            .unwrap_or_else(|| PathBuf::from("/var/lib/pacman"));

        Self { dbpath }
    }

    fn parse_sync_db(&self, db_file: &Path) -> Result<Vec<Package>> {
        let mut packages = Vec::with_capacity(10000);
        let file = fs::File::open(db_file)?;
        let decoder = GzDecoder::new(file);
        let mut archive = Archive::new(decoder);

        let repo = db_file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Pre-allocate string buffer to avoid repeated allocations
        let mut content = String::with_capacity(1024);

        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?;

            // Fast path check - avoid string allocation
            let path_bytes = path.as_os_str().as_encoded_bytes();
            if !path_bytes.ends_with(b"/desc") {
                continue;
            }

            // Reuse string buffer
            content.clear();
            entry.read_to_string(&mut content)?;

            let mut name: Option<&str> = None;
            let mut version: Option<&str> = None;
            let mut desc: Option<&str> = None;
            let mut url: Option<&str> = None;
            let mut license: Option<&str> = None;
            let mut csize: Option<&str> = None;
            let mut current_field = "";

            // Ultra-fast parsing with zero allocations
            for line in content.lines() {
                if line.is_empty() {
                    continue;
                }

                let bytes = line.as_bytes();
                if bytes.first() == Some(&b'%') && bytes.last() == Some(&b'%') {
                    current_field = &line[1..line.len() - 1];
                } else {
                    match current_field {
                        "NAME" if name.is_none() => name = Some(line),
                        "VERSION" if version.is_none() => version = Some(line),
                        "DESC" if desc.is_none() => desc = Some(line),
                        "URL" if url.is_none() => url = Some(line),
                        "LICENSE" if license.is_none() => license = Some(line),
                        "CSIZE" if csize.is_none() => csize = Some(line),
                        _ => {}
                    }

                    // Early exit when we have all fields
                    if name.is_some()
                        && version.is_some()
                        && desc.is_some()
                        && url.is_some()
                        && license.is_some()
                        && csize.is_some()
                    {
                        break;
                    }
                }
            }

            if let Some(pkg_name) = name {
                let size_bytes = csize.and_then(|s| s.parse::<u64>().ok());

                packages.push(Package {
                    name: pkg_name.to_string(),
                    version: version.map(|s| s.to_string()),
                    description: desc.unwrap_or("").to_string(),
                    repo: repo.clone(),
                    manager: "pacman".to_string(),
                    installed: false,
                    homepage: url.unwrap_or("").to_string(),
                    license: license.unwrap_or("").to_string(),
                    size: size_bytes,
                });
            }
        }

        packages.shrink_to_fit();
        Ok(packages)
    }

    fn parse_local_db(&self) -> Result<Vec<Package>> {
        use rayon::prelude::*;

        let local_path = self.dbpath.join("local");

        if !local_path.exists() {
            return Ok(Vec::new());
        }

        // Collect paths first - optimized
        let desc_files: Vec<PathBuf> = fs::read_dir(&local_path)?
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if path.is_dir() {
                    let desc = path.join("desc");
                    if desc.exists() {
                        return Some(desc);
                    }
                }
                None
            })
            .collect();

        // Parse in parallel - OPTIMIZED: only read NAME field for speed
        let packages: Vec<Package> = desc_files
            .par_iter()
            .filter_map(|desc_file| {
                // Fast read - only get package name
                let content = fs::read_to_string(desc_file).ok()?;

                let mut name: Option<&str> = None;
                let mut in_name_section = false;

                // Ultra-fast parsing - stop after finding name
                for line in content.lines() {
                    if line == "%NAME%" {
                        in_name_section = true;
                        continue;
                    }

                    if in_name_section && !line.is_empty() && !line.starts_with('%') {
                        name = Some(line);
                        break; // Found name, stop parsing
                    }

                    if line.starts_with('%') {
                        in_name_section = false;
                    }
                }

                name.map(|pkg_name| Package {
                    name: pkg_name.to_string(),
                    version: None,
                    description: String::new(),
                    repo: "local".to_string(),
                    manager: "pacman".to_string(),
                    installed: true,
                    homepage: String::new(),
                    license: String::new(),
                    size: None,
                })
            })
            .collect();

        Ok(packages)
    }
}

impl PackageManager for Pacman {
    fn name(&self) -> &str {
        "pacman"
    }

    fn is_available(&self) -> bool {
        self.dbpath.exists() && self.dbpath.join("sync").exists()
    }

    fn list_all(&self) -> Result<Vec<Package>> {
        use rayon::prelude::*;

        // First try pmux cache (downloaded repos)
        let mut db_files = Vec::new();

        if let Some(cache_dir) = dirs::cache_dir() {
            let pmux_cache = cache_dir.join("pmux").join("repos");
            if pmux_cache.exists() {
                db_files = fs::read_dir(&pmux_cache)?
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .and_then(|s| s.to_str())
                            .map_or(false, |s| s.starts_with("pacman-") && s.ends_with(".db"))
                    })
                    .collect();
            }
        }

        // If no cached repos, fall back to system repos
        if db_files.is_empty() {
            let sync_path = self.dbpath.join("sync");
            if sync_path.exists() {
                db_files = fs::read_dir(sync_path)?
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("db"))
                    .collect();
            }
        }

        if db_files.is_empty() {
            return Ok(Vec::new());
        }

        // Parse all databases in parallel
        let all_packages: Vec<Package> = db_files
            .par_iter()
            .filter_map(|path| self.parse_sync_db(path).ok())
            .flatten()
            .collect();
        Ok(all_packages)
    }

    fn list_installed(&self) -> Result<Vec<Package>> {
        self.parse_local_db()
    }

    #[allow(dead_code)]
    fn search(&self, _query: &str) -> Result<Vec<Package>> {
        self.list_all()
    }

    fn install_command(&self, packages: &[&Package]) -> String {
        let pkg_names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        format!("sudo pacman -S {}", pkg_names.join(" "))
    }

    fn remove_command(&self, packages: &[&Package]) -> String {
        let pkg_names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        format!("sudo pacman -R {}", pkg_names.join(" "))
    }

    #[allow(dead_code)]
    fn needs_sudo(&self) -> bool {
        true
    }
}
