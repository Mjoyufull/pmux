use crate::pm::Package;
use crate::redb_cache::RedbCache;
use eyre::Result;

pub struct CacheManager {
    redb_cache: RedbCache,
}

impl CacheManager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            redb_cache: RedbCache::new()?,
        })
    }

    /// Get packages for a manager (e.g., "pacman_all", "dnf_all")
    pub fn get(&self, key: &str) -> Result<Option<Vec<Package>>> {
        // Parse key: "manager_type" (e.g., "pacman_all", "dnf_installed")
        let manager = key.split('_').next().unwrap_or(key);
        let packages = self.redb_cache.get_all_packages(manager)?;
        
        if packages.is_empty() {
            Ok(None)
        } else {
            Ok(Some(packages))
        }
    }

    /// Set packages for a manager
    pub fn set(&self, key: &str, packages: Vec<Package>) -> Result<()> {
        let manager = key.split('_').next().unwrap_or(key);
        self.redb_cache.set_packages(manager, &packages)
    }

    /// Check if cache is stale
    pub fn is_stale(&self, key: &str, max_age_secs: u64) -> Result<bool> {
        let manager = key.split('_').next().unwrap_or(key);
        self.redb_cache.needs_refresh(manager, "main", max_age_secs)
    }

    /// Get installed packages with short TTL (5 second cache to avoid slow rpm/db queries)
    pub fn get_installed(&self, key: &str) -> Result<Option<Vec<Package>>> {
        if let Ok(false) = self.is_stale(key, 5) {
            self.get(key)
        } else {
            Ok(None)
        }
    }

    /// Set installed packages
    pub fn set_installed(&self, key: &str, packages: Vec<Package>) -> Result<()> {
        self.set(key, packages)
    }
    
    /// Incrementally update packages (only write changes)
    #[allow(dead_code)]
    pub fn update_packages(&self, manager: &str, packages: Vec<Package>) -> Result<usize> {
        self.redb_cache.update_packages(manager, &packages)
    }
    
    /// Get direct access to redb cache for optimized queries
    pub fn redb_cache(&self) -> &RedbCache {
        &self.redb_cache
    }
}
