use super::{Package, PackageManager};
use eyre::Result;
use rusqlite::{Connection, OpenFlags};
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

    // Parse RPM database - DIRECT SQLite READ, ZERO COMMAND SPAWNING!
    fn parse_rpmdb(&self) -> Result<Vec<Package>> {
        let mut all_packages = Vec::new();

        // Read from all found RPM databases (for bedrock linux multi-stratum support)
        for rpmdb_path in &self.rpmdb_paths {
            let sqlite_path = rpmdb_path.join("rpmdb.sqlite");

            if !sqlite_path.exists() {
                continue;
            }

            // Direct SQLite read - INSTANT!
            match Connection::open_with_flags(&sqlite_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
                Ok(conn) => {
                    // Query the Name table - simple and fast
                    let mut stmt = conn.prepare("SELECT DISTINCT key FROM Name")?;
                    let packages_iter = stmt.query_map([], |row| {
                        let name: String = row.get(0)?;
                        Ok(Package {
                            name,
                            version: None,
                            description: String::new(),
                            repo: "installed".to_string(),
                            manager: "dnf".to_string(),
                            installed: true,
                            homepage: String::new(),
                            license: String::new(),
                            size: None,
                        })
                    })?;

                    for pkg_result in packages_iter {
                        if let Ok(pkg) = pkg_result {
                            all_packages.push(pkg);
                        }
                    }
                }
                Err(_) => {
                    // Silently skip databases we can't read
                    continue;
                }
            }
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
        // Try pmux cache first
        if let Some(cache_dir) = dirs::cache_dir() {
            let dnf_list = cache_dir
                .join("pmux")
                .join("repos")
                .join("dnf-packages.txt");
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
                        homepage: String::new(),
                        license: String::new(),
                        size: None,
                    });
                }

                packages.shrink_to_fit();
                return Ok(packages);
            }
        }

        // No cache available - return empty list
        // Available packages should be synced with `pmux -Sy`
        Ok(vec![])
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

    fn remove_command(&self, packages: &[&Package]) -> String {
        let pkg_names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
        format!("sudo dnf remove {}", pkg_names.join(" "))
    }

    fn needs_sudo(&self) -> bool {
        true
    }
}
