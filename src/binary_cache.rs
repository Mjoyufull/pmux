// Ultra-fast binary cache format with memory mapping
// Format: [header][string_table][package_index][package_data]
// Zero-copy, instant loading, minimal allocations

use crate::pm::Package;
use eyre::Result;
use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;

const MAGIC: &[u8; 4] = b"PMUX";
const VERSION: u32 = 1;

#[repr(C)]
struct Header {
    magic: [u8; 4],
    version: u32,
    string_table_offset: u64,
    string_table_size: u64,
    index_offset: u64,
    index_size: u64,
    package_count: u32,
    _padding: u32,
}

pub struct BinaryCache {
    cache_dir: PathBuf,
}

impl BinaryCache {
    pub fn new() -> Result<Self> {
        let cache_dir = dirs::cache_dir()
            .ok_or_else(|| eyre::eyre!("Could not find cache directory"))?
            .join("pmux")
            .join("binary");
        
        std::fs::create_dir_all(&cache_dir)?;
        
        Ok(Self { cache_dir })
    }
    
    fn cache_path(&self, key: &str) -> PathBuf {
        self.cache_dir.join(format!("{}.bin", key))
    }
    
    pub fn get(&self, key: &str) -> Result<Option<Vec<Package>>> {
        let path = self.cache_path(key);
        
        if !path.exists() {
            return Ok(None);
        }
        
        let file = File::open(&path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        
        // Verify header
        if mmap.len() < std::mem::size_of::<Header>() {
            return Ok(None);
        }
        
        let header = unsafe { &*(mmap.as_ptr() as *const Header) };
        
        if &header.magic != MAGIC || header.version != VERSION {
            return Ok(None);
        }
        
        // Parse string table
        let string_table_start = header.string_table_offset as usize;
        let string_table_end = string_table_start + header.string_table_size as usize;
        let string_table_bytes = &mmap[string_table_start..string_table_end];
        
        // Manager is derived from key once
        let manager = key.split('_').next().unwrap_or("unknown");
        
        // Parse packages - ULTRA FAST with minimal allocations
        let mut packages = Vec::with_capacity(header.package_count as usize);
        let index_start = header.index_offset as usize;
        let index_ptr = unsafe { mmap.as_ptr().add(index_start) as *const u32 };
        
        for i in 0..header.package_count as usize {
            // Read 8 u32 values directly from memory (32 bytes)
            let idx = i * 8;
            let name_offset = unsafe { *index_ptr.add(idx) } as usize;
            let name_len = unsafe { *index_ptr.add(idx + 1) } as usize;
            let version_offset = unsafe { *index_ptr.add(idx + 2) } as usize;
            let version_len = unsafe { *index_ptr.add(idx + 3) } as usize;
            let desc_offset = unsafe { *index_ptr.add(idx + 4) } as usize;
            let desc_len = unsafe { *index_ptr.add(idx + 5) } as usize;
            let repo_offset = unsafe { *index_ptr.add(idx + 6) } as usize;
            let repo_len = unsafe { *index_ptr.add(idx + 7) } as usize;
            
            // Fast string extraction with bounds checking
            let name = if name_offset + name_len <= string_table_bytes.len() {
                unsafe { std::str::from_utf8_unchecked(&string_table_bytes[name_offset..name_offset + name_len]) }
            } else {
                ""
            };
            
            let version = if version_len > 0 && version_offset + version_len <= string_table_bytes.len() {
                Some(unsafe { std::str::from_utf8_unchecked(&string_table_bytes[version_offset..version_offset + version_len]) }.to_string())
            } else {
                None
            };
            
            let description = if desc_offset + desc_len <= string_table_bytes.len() {
                unsafe { std::str::from_utf8_unchecked(&string_table_bytes[desc_offset..desc_offset + desc_len]) }
            } else {
                ""
            };
            
            let repo = if repo_offset + repo_len <= string_table_bytes.len() {
                unsafe { std::str::from_utf8_unchecked(&string_table_bytes[repo_offset..repo_offset + repo_len]) }
            } else {
                ""
            };
            
            packages.push(Package {
                name: name.to_string(),
                version,
                description: description.to_string(),
                repo: repo.to_string(),
                manager: manager.to_string(),
                installed: false,
            });
        }
        
        Ok(Some(packages))
    }
    
    pub fn set(&self, key: &str, packages: &[Package]) -> Result<()> {
        let path = self.cache_path(key);
        let temp_path = self.cache_dir.join(format!("{}.tmp", key));
        
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)?;
        
        let mut writer = BufWriter::with_capacity(1024 * 1024, file);
        
        // Build string table with deduplication
        let mut string_table = Vec::new();
        let mut string_offsets: HashMap<String, (u32, u32)> = HashMap::new();
        
        let mut add_string = |s: &str| -> (u32, u32) {
            if let Some(&offsets) = string_offsets.get(s) {
                return offsets;
            }
            
            let offset = string_table.len() as u32;
            let len = s.len() as u32;
            string_table.extend_from_slice(s.as_bytes());
            string_offsets.insert(s.to_string(), (offset, len));
            (offset, len)
        };
        
        // Collect all string offsets
        let mut package_entries = Vec::with_capacity(packages.len());
        
        for pkg in packages {
            let name = add_string(&pkg.name);
            let version = if let Some(ref v) = pkg.version {
                add_string(v)
            } else {
                (0, 0)
            };
            let desc = add_string(&pkg.description);
            let repo = add_string(&pkg.repo);
            
            package_entries.push((name, version, desc, repo));
        }
        
        // Write header (placeholder)
        let header_size = std::mem::size_of::<Header>();
        writer.write_all(&vec![0u8; header_size])?;
        
        // Write string table
        let string_table_offset = header_size as u64;
        let string_table_size = string_table.len() as u64;
        writer.write_all(&string_table)?;
        
        // Write package index
        let index_offset = string_table_offset + string_table_size;
        
        for (name, version, desc, repo) in package_entries {
            writer.write_all(&name.0.to_le_bytes())?;
            writer.write_all(&name.1.to_le_bytes())?;
            writer.write_all(&version.0.to_le_bytes())?;
            writer.write_all(&version.1.to_le_bytes())?;
            writer.write_all(&desc.0.to_le_bytes())?;
            writer.write_all(&desc.1.to_le_bytes())?;
            writer.write_all(&repo.0.to_le_bytes())?;
            writer.write_all(&repo.1.to_le_bytes())?;
        }
        
        writer.flush()?;
        drop(writer);
        
        // Write actual header
        let mut file = OpenOptions::new().write(true).open(&temp_path)?;
        
        let header = Header {
            magic: *MAGIC,
            version: VERSION,
            string_table_offset,
            string_table_size,
            index_offset,
            index_size: (packages.len() * 32) as u64,
            package_count: packages.len() as u32,
            _padding: 0,
        };
        
        let header_bytes = unsafe {
            std::slice::from_raw_parts(
                &header as *const Header as *const u8,
                std::mem::size_of::<Header>(),
            )
        };
        
        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(0))?;
        file.write_all(header_bytes)?;
        file.sync_all()?;
        drop(file);
        
        std::fs::rename(temp_path, path)?;
        
        Ok(())
    }
    
    pub fn is_stale(&self, key: &str, max_age_secs: u64) -> Result<bool> {
        let path = self.cache_path(key);
        
        if !path.exists() {
            return Ok(true);
        }
        
        let metadata = std::fs::metadata(&path)?;
        let modified = metadata.modified()?;
        let age = modified.elapsed()?.as_secs();
        
        Ok(age > max_age_secs)
    }
}
