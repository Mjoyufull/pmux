use crate::binary_cache::BinaryCache;
use crate::pm::Package;
use eyre::Result;

pub struct CacheManager {
    binary_cache: BinaryCache,
}

impl CacheManager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            binary_cache: BinaryCache::new()?,
        })
    }

    pub fn get(&self, key: &str) -> Result<Option<Vec<Package>>> {
        self.binary_cache.get(key)
    }

    pub fn set(&self, key: &str, packages: Vec<Package>) -> Result<()> {
        self.binary_cache.set(key, &packages)
    }

    pub fn is_stale(&self, key: &str, max_age_secs: u64) -> Result<bool> {
        self.binary_cache.is_stale(key, max_age_secs)
    }

    // Get installed packages with short TTL (5 second cache to avoid slow rpm/db queries)
    pub fn get_installed(&self, key: &str) -> Result<Option<Vec<Package>>> {
        if let Ok(false) = self.is_stale(key, 5) {
            self.get(key)
        } else {
            Ok(None)
        }
    }

    pub fn set_installed(&self, key: &str, packages: Vec<Package>) -> Result<()> {
        self.set(key, packages)
    }
}
