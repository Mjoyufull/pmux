// redb-based package cache with incremental update support
use crate::pm::Package;
use eyre::Result;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use bincode;

// Table definitions
const PACKAGES_TABLE: TableDefinition<(&str, &str), &[u8]> = TableDefinition::new("packages");
const SYNC_METADATA_TABLE: TableDefinition<(&str, &str), &[u8]> = TableDefinition::new("sync_metadata");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncMetadata {
    pub last_sync: u64,      // Unix timestamp
    pub checksum: String,    // Hash of repo metadata
    pub package_count: usize,
}

pub struct RedbCache {
    db: Database,
}

impl RedbCache {
    pub fn new() -> Result<Self> {
        let cache_dir = dirs::cache_dir()
            .ok_or_else(|| eyre::eyre!("Could not find cache directory"))?
            .join("pmux");
        
        std::fs::create_dir_all(&cache_dir)?;
        
        let db_path = cache_dir.join("packages.redb");
        let db = Database::create(&db_path)?;
        
        // Initialize tables if they don't exist (create them by opening in a write transaction)
        let write_txn = db.begin_write()?;
        {
            // Opening tables in write mode creates them if they don't exist
            let _ = write_txn.open_table(PACKAGES_TABLE)?;
            let _ = write_txn.open_table(SYNC_METADATA_TABLE)?;
        }
        write_txn.commit()?;
        
        Ok(Self { db })
    }
    
    /// Get a single package by manager and name
    #[allow(dead_code)]
    pub fn get_package(&self, manager: &str, name: &str) -> Result<Option<Package>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PACKAGES_TABLE)?;
        
        if let Some(value) = table.get((manager, name))? {
            let bytes = value.value();
            let package: Package = bincode::deserialize(bytes)?;
            Ok(Some(package))
        } else {
            Ok(None)
        }
    }
    
    /// Get package count for a manager (FAST - doesn't load data)
    pub fn get_package_count(&self, manager: &str) -> Result<usize> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PACKAGES_TABLE)?;
        
        let range_start = (manager, "");
        let range_end = (manager, "\u{10FFFF}");
        
        // Just count entries - very fast
        let count = table.range(range_start..=range_end)?.count();
        Ok(count)
    }
    
    /// Get packages in a range (for lazy loading)
    /// Returns (packages, total_count)
    pub fn get_packages_range(&self, manager: &str, start: usize, end: usize) -> Result<(Vec<Package>, usize)> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PACKAGES_TABLE)?;
        
        let range_start = (manager, "");
        let range_end = (manager, "\u{10FFFF}");
        
        // Collect entries in range
        let mut entries = Vec::new();
        let mut iter = table.range(range_start..=range_end)?;
        let mut current_idx = 0;
        
        while let Some(entry) = iter.next() {
            if current_idx >= end {
                break;
            }
            if current_idx >= start {
                let (_, value) = entry?;
                entries.push(value.value().to_vec());
            } else {
                let _ = entry?; // Skip but still advance
            }
            current_idx += 1;
        }
        
        let total_count = current_idx;
        
        // Drop transaction early
        drop(iter);
        drop(table);
        drop(read_txn);
        
        // Deserialize in parallel
        use rayon::prelude::*;
        let packages: Vec<Package> = entries
            .into_par_iter()
            .filter_map(|bytes| bincode::deserialize(&bytes).ok())
            .collect();
        
        Ok((packages, total_count))
    }
    
    /// Get all packages for a manager (for compatibility - but try to avoid this!)
    /// OPTIMIZED: Uses range query instead of full table scan for instant loading
    /// Uses bincode for 10-100x faster deserialization than JSON
    pub fn get_all_packages(&self, manager: &str) -> Result<Vec<Package>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(PACKAGES_TABLE)?;
        
        // Use range query: from (manager, "") to (manager, "\u{10FFFF}") to get all packages for this manager
        // This is MUCH faster than scanning the entire table - uses B-tree index directly
        let range_start = (manager, "");
        // Use max Unicode char as range end (redb uses string comparison)
        let range_end = (manager, "\u{10FFFF}");
        
        // Collect all entries first, then deserialize in parallel
        // This is faster because we can batch deserialize
        let mut entries = Vec::new();
        let mut iter = table.range(range_start..=range_end)?;
        while let Some(entry) = iter.next() {
            let (_, value) = entry?;
            entries.push(value.value().to_vec()); // Copy bytes out of transaction
        }
        
        // Drop transaction early - we have all the data
        drop(iter);
        drop(table);
        drop(read_txn);
        
        // Deserialize in parallel using rayon for 300k+ packages
        use rayon::prelude::*;
        let packages: Vec<Package> = entries
            .into_par_iter()
            .filter_map(|bytes| bincode::deserialize(&bytes).ok())
            .collect();
        
        Ok(packages)
    }
    
    /// Store packages for a manager (batch insert)
    pub fn set_packages(&self, manager: &str, packages: &[Package]) -> Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(PACKAGES_TABLE)?;
            
            for pkg in packages {
                let key = (manager, pkg.name.as_str());
                // Use bincode instead of JSON - MUCH faster serialization
                let value = bincode::serialize(pkg)?;
                table.insert(key, value.as_slice())?;
            }
        }
        write_txn.commit()?;
        
        Ok(())
    }
    
    /// Delete all packages for a manager
    pub fn clear_packages(&self, manager: &str) -> Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(PACKAGES_TABLE)?;
            
            // Collect keys to delete
            let keys_to_delete: Vec<(String, String)> = {
                let mut keys = Vec::new();
                let mut iter = table.iter()?;
                while let Some(entry) = iter.next() {
                    let (key, _) = entry?;
                    let (entry_manager, entry_name) = key.value();
                    if entry_manager == manager {
                        keys.push((entry_manager.to_string(), entry_name.to_string()));
                    }
                }
                keys
            };
            
            // Delete them
            for (mgr, name) in keys_to_delete {
                table.remove((mgr.as_str(), name.as_str()))?;
            }
        }
        write_txn.commit()?;
        
        Ok(())
    }
    
    /// Incrementally update packages: only insert/update changed ones
    pub fn update_packages(&self, manager: &str, packages: &[Package]) -> Result<usize> {
        let write_txn = self.db.begin_write()?;
        let mut updated_count = 0;
        
        {
            let mut table = write_txn.open_table(PACKAGES_TABLE)?;
            
            for pkg in packages {
                let key = (manager, pkg.name.as_str());
                // Use bincode instead of JSON - MUCH faster serialization
                let new_value = bincode::serialize(pkg)?;
                
                // Check if package exists and is different
                let needs_update = if let Some(existing) = table.get(key)? {
                    existing.value() != new_value.as_slice()
                } else {
                    true // New package
                };
                
                if needs_update {
                    table.insert(key, new_value.as_slice())?;
                    updated_count += 1;
                }
            }
        }
        
        write_txn.commit()?;
        
        Ok(updated_count)
    }
    
    /// Get sync metadata for a manager/repo
    pub fn get_sync_metadata(&self, manager: &str, repo: &str) -> Result<Option<SyncMetadata>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(SYNC_METADATA_TABLE)?;
        
        if let Some(value) = table.get((manager, repo))? {
            let bytes = value.value();
            let metadata: SyncMetadata = bincode::deserialize(bytes)?;
            Ok(Some(metadata))
        } else {
            Ok(None)
        }
    }
    
    /// Set sync metadata for a manager/repo
    pub fn set_sync_metadata(&self, manager: &str, repo: &str, metadata: &SyncMetadata) -> Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(SYNC_METADATA_TABLE)?;
            let key = (manager, repo);
            // Use bincode instead of JSON - MUCH faster serialization
            let value = bincode::serialize(metadata)?;
            table.insert(key, value.as_slice())?;
        }
        write_txn.commit()?;
        
        Ok(())
    }
    
    /// Check if cache needs refresh based on age
    pub fn needs_refresh(&self, manager: &str, repo: &str, max_age_secs: u64) -> Result<bool> {
        if let Some(metadata) = self.get_sync_metadata(manager, repo)? {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            
            Ok(now - metadata.last_sync > max_age_secs)
        } else {
            // No metadata = needs refresh
            Ok(true)
        }
    }
    
    /// Get database statistics
    #[allow(dead_code)]
    pub fn stats(&self) -> Result<CacheStats> {
        let read_txn = self.db.begin_read()?;
        let pkg_table = read_txn.open_table(PACKAGES_TABLE)?;
        
        let mut total_packages = 0;
        let mut packages_by_manager: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        
        let mut iter = pkg_table.iter()?;
        while let Some(entry) = iter.next() {
            let (key, _) = entry?;
            let (manager, _) = key.value();
            
            total_packages += 1;
            *packages_by_manager.entry(manager.to_string()).or_insert(0) += 1;
        }
        
        Ok(CacheStats {
            total_packages,
            packages_by_manager,
        })
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct CacheStats {
    pub total_packages: usize,
    pub packages_by_manager: std::collections::HashMap<String, usize>,
}

