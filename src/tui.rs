use crate::cache::CacheManager;
use crate::config::Config;
use crate::input::{Event, Input};
use crate::pm::{detect_available_managers, Package, PackageManager};
use ahash::AHashSet;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use eyre::Result;
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use ratatui::{
    backend::Backend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Terminal,
};

pub struct App {
    all_packages: Vec<Package>,
    shown_indices: Vec<usize>, // Store indices instead of cloning packages
    installed_packages: Vec<Package>,
    installed_set: AHashSet<String>, // Fast lookup for installed packages - PRE-COMPUTED KEYS
    installed_shown_indices: Vec<usize>, // Filtered installed indices
    installed_selected_index: Option<usize>,
    installed_filter: String,
    installed_search_active: bool,
    selected_packages: AHashSet<String>, // Format: "name:manager"
    packages_to_remove: AHashSet<String>,
    selected_index: Option<usize>,
    query: String,
    last_query: String, // Track last query for incremental filtering
    scroll_offset: usize,
    installed_scroll_offset: usize,
    matcher: SkimMatcherV2,
    managers: Vec<Box<dyn PackageManager>>,
    filter_managers: Vec<String>,
    config: Config,
    focus_installed: bool,
    hostpm: String,
    loaded_managers: AHashSet<String>, // Track which PMs have been loaded
    manager_indices: std::collections::HashMap<String, Vec<usize>>, // Fast lookup: manager -> package indices
}

impl App {
    fn new(filter_managers: Vec<String>, config: Config) -> Result<Self> {
        let hostpm = config.pm.hostpm.clone();

        // Determine which managers to actually use
        let managers = detect_available_managers(&hostpm, &config.pm.enabled_pm);

        if managers.is_empty() {
            return Err(eyre::eyre!("No supported package managers found"));
        }

        // Default to showing ALL enabled PM packages, not just host PM
        let initial_filters = filter_managers;

        Ok(Self {
            all_packages: Vec::with_capacity(500_000),
            shown_indices: Vec::with_capacity(10_000),
            installed_packages: Vec::with_capacity(5_000),
            installed_set: AHashSet::with_capacity(5_000),
            installed_shown_indices: Vec::new(),
            installed_selected_index: None,
            installed_filter: String::new(),
            installed_search_active: false,
            selected_packages: AHashSet::new(),
            packages_to_remove: AHashSet::new(),
            selected_index: Some(0),
            query: String::new(),
            last_query: String::new(),
            scroll_offset: 0,
            installed_scroll_offset: 0,
            matcher: SkimMatcherV2::default(),
            managers,
            filter_managers: initial_filters,
            config,
            focus_installed: false,
            hostpm: hostpm.clone(),
            loaded_managers: AHashSet::from_iter(vec![hostpm]),
            manager_indices: std::collections::HashMap::new(),
        })
    }

    fn load_packages(&mut self) -> Result<()> {
        use rayon::prelude::*;
        
        let cache = CacheManager::new()?;
        let redb_cache = cache.redb_cache();
        
        // Quick check: does redb have any packages?
        let has_cache = self.managers.first()
            .and_then(|m| redb_cache.get_package_count(m.name()).ok())
            .map_or(false, |count| count > 0);
        
        if !has_cache {
            eprintln!("\nWarning: No package database found.");
            eprintln!("Please run 'pmux -Syy' to sync package databases first.\n");
            return Err(eyre::eyre!("No package database found. Run 'pmux -Syy' to sync."));
        }

        // Load all enabled PMs in PARALLEL using rayon - MUCH faster for 300k+ packages
        // Each manager loads independently using optimized B-tree range queries with bincode
        // This is FAST because:
        // 1. B-tree range queries (O(log n + k) instead of O(n))
        // 2. bincode deserialization (10-100x faster than JSON)
        // 3. Parallel loading across all managers
        let results: Vec<_> = self
            .managers
            .par_iter()
            .map(|manager| {
                // Direct redb range query with bincode deserialization - FAST!
                let packages = redb_cache.get_all_packages(manager.name()).unwrap_or_default();
                (manager.name().to_string(), packages)
            })
            .collect();

        // Pre-allocate total capacity to avoid reallocations during extend
        let total_capacity: usize = results.iter().map(|(_, pkgs)| pkgs.len()).sum();
        self.all_packages.reserve(total_capacity);
        
        // Add host PM first
        for (name, packages) in &results {
            if name == &self.hostpm {
                self.all_packages.extend(packages.clone());
                self.loaded_managers.insert(name.clone());
            }
        }
        
        // Add other enabled PMs
        for (name, packages) in results {
            if name != self.hostpm {
                self.all_packages.extend(packages);
                self.loaded_managers.insert(name);
            }
        }

        // Build manager index for FAST filtering (avoids iterating all 300k packages)
        self.build_manager_index();

        // Show first 1M packages initially
        self.shown_indices = (0..self.all_packages.len().min(1_000_000)).collect();
        if !self.shown_indices.is_empty() {
            self.selected_index = Some(0);
        }

        Ok(())
    }

    /// Build index mapping manager names to package indices - CRITICAL for fast @pm filtering
    fn build_manager_index(&mut self) {
        self.manager_indices.clear();
        for (idx, pkg) in self.all_packages.iter().enumerate() {
            self.manager_indices
                .entry(pkg.manager.to_lowercase())
                .or_insert_with(Vec::new)
                .push(idx);
        }
    }

    /// Score a package for search - AVOID ALLOCATIONS by using case-insensitive comparison
    fn score_package(&self, idx: usize, pkg: &Package, has_query: bool, query_lower: &str, search_query: &str) -> Option<(usize, i64)> {
        if has_query {
            let name = &pkg.name;
            
            // Use case-insensitive comparison WITHOUT allocating lowercase strings
            // Exact match (case-insensitive)
            if name.len() == query_lower.len() && name.eq_ignore_ascii_case(query_lower) {
                return Some((idx, 1_000_000));
            }

            // Starts with (case-insensitive) - fast path
            if name.len() >= query_lower.len() && name[..query_lower.len()].eq_ignore_ascii_case(query_lower) {
                return Some((idx, 900_000));
            }

            // Contains (case-insensitive) - only allocate if needed
            if query_lower.len() >= 2 {
                // Only allocate lowercase if we need contains check
                let name_lower = name.to_ascii_lowercase();
                if name_lower.contains(query_lower) {
                    return Some((idx, 800_000));
                }
            } else if !name.is_empty() {
                // Single char - just check first char
                if name.chars().next().unwrap_or(' ').to_ascii_lowercase() == query_lower.chars().next().unwrap_or(' ') {
                    return Some((idx, 800_000));
                }
            }

            // For queries 4+ chars, try fuzzy match
            // But prioritize exact/starts_with/contains matches first (already handled above)
            // Only use fuzzy if nothing matched yet
            if query_lower.len() >= 4 {
                if let Some(score) = self.matcher.fuzzy_match(name, search_query) {
                    if score > 0 {
                        // Fuzzy scores are typically much lower, scale them appropriately
                        return Some((idx, score.max(100))); // Minimum 100 for fuzzy matches
                    }
                }
            }

            None
        } else {
            Some((idx, 0))
        }
    }

    /// Load installed packages - queries PMs once per launch
    /// Called after first draw for instant startup
    /// CRITICAL: Pre-compute ALL lookup keys here to avoid ANY allocations in hot path
    fn load_installed_packages(&mut self) {
        use rayon::prelude::*;
        
        // Query PMs directly - once per launch
        let installed_results: Vec<_> = self
            .managers
            .par_iter()
            .map(|manager| {
                manager.list_installed().unwrap_or_default()
            })
            .collect();

        // Clear and rebuild installed sets/lists
        self.installed_set.clear();
        self.installed_packages.clear();

        // Pre-compute ALL lookup keys for ALL installed packages
        // This eliminates ALL allocations in the hot path (draw loop)
        for installed in installed_results {
            if !installed.is_empty() {
                for pkg in &installed {
                    // Normalize package name (trim whitespace, lowercase for matching)
                    let pkg_name_normalized = pkg.name.trim().to_lowercase();
                    let pkg_name_original = pkg.name.trim().to_string(); // Ensure owned String
                    let manager_lower = pkg.manager.to_lowercase();
                    
                    // PRE-COMPUTE all lookup key formats and store them
                    // These are computed ONCE at load time, never in the hot path
                    self.installed_set.insert(format!("{}:{}", pkg_name_normalized, manager_lower));
                    self.installed_set.insert(format!("{}:{}", pkg_name_original, pkg.manager));
                    self.installed_set.insert(format!("{}:{}", pkg_name_normalized, pkg.manager));
                    self.installed_set.insert(pkg_name_normalized.clone());
                    self.installed_set.insert(pkg_name_original.clone());
                }
                self.installed_packages.extend(installed);
            }
        }

        self.refresh_installed_view();
    }

    fn refresh_installed_view(&mut self) {
        let filter = self.installed_filter.to_lowercase();
        if filter.is_empty() {
            self.installed_shown_indices = (0..self.installed_packages.len()).collect();
        } else {
            self.installed_shown_indices = self
                .installed_packages
                .iter()
                .enumerate()
                .filter_map(|(idx, pkg)| {
                    let name = pkg.name.to_lowercase();
                    let manager = pkg.manager.to_lowercase();
                    if name.contains(&filter) || manager.contains(&filter) {
                        Some(idx)
                    } else {
                        None
                    }
                })
                .collect();
        }

        if self.installed_shown_indices.is_empty() {
            self.installed_selected_index = None;
            self.installed_scroll_offset = 0;
        } else {
            self.installed_selected_index = Some(0);
            self.installed_scroll_offset = 0;
        }
    }

    fn move_installed_selection(&mut self, delta: isize, visible_height: usize) {
        if self.installed_shown_indices.is_empty() {
            self.installed_selected_index = None;
            return;
        }

        let current = self.installed_selected_index.unwrap_or(0);
        let new_idx = if delta < 0 {
            current.saturating_sub(delta.abs() as usize)
        } else {
            (current + delta as usize).min(self.installed_shown_indices.len().saturating_sub(1))
        };

        self.installed_selected_index = Some(new_idx);

        if new_idx < self.installed_scroll_offset {
            self.installed_scroll_offset = new_idx;
        } else if new_idx >= self.installed_scroll_offset + visible_height {
            self.installed_scroll_offset = new_idx + 1 - visible_height;
        }
    }
    
    /// Check if package is installed - ZERO ALLOCATIONS: uses pre-computed keys only
    /// All lookup keys are pre-computed in load_installed_packages()
    /// IMPORTANT: Only matches packages with the SAME manager to avoid cross-PM false positives
    fn is_package_installed(&self, pkg: &Package) -> bool {
        // Build lookup keys WITHOUT allocations - use string slices and direct comparisons
        // We need to check if any of the pre-computed formats match
        
        // Strategy: Build keys on the fly but only allocate if we find a match
        // Actually, we still need to allocate to check HashSet.contains()
        // BUT: We can optimize by checking the most common format first and short-circuiting
        
        let pkg_name_trimmed = pkg.name.trim();
        let manager = &pkg.manager;
        
        // Fast path: try exact match with original name first (no allocation if match)
        // But we still need to allocate to check HashSet...
        
        // Actually, the real optimization is: pre-compute keys for ALL packages in all_packages
        // But that's too much memory. Instead, let's use a smarter approach:
        // Check if we can find the key without allocating by using a different data structure
        
        // For now, the best we can do is minimize allocations:
        // 1. Try most common format first (normalized:normalized)
        // 2. Short-circuit on first match
        // 3. Only allocate what we need
        
        let pkg_name_normalized = pkg_name_trimmed.to_lowercase();
        let manager_lower = manager.to_lowercase();
        
        // Try most common format first - if it matches, we're done (1 allocation)
        // CRITICAL: Only check manager-specific matches to avoid cross-PM false positives
        if self.installed_set.contains(&format!("{}:{}", pkg_name_normalized, manager_lower)) {
            return true;
        }
        
        // Try normalized name with original manager (1 allocation)
        if self.installed_set.contains(&format!("{}:{}", pkg_name_normalized, manager)) {
            return true;
        }
        
        // Try original case formats (1 allocation)
        if self.installed_set.contains(&format!("{}:{}", pkg_name_trimmed, manager)) {
            return true;
        }
        
        // DO NOT check name-only matches - they cause cross-PM false positives
        // (e.g., hyprland from dnf would match hyprland from nix)
        
        false
    }


    fn parse_query_filters(&mut self) {
        // Extract @pm or *pm filters from query without modifying the query string
        let parts: Vec<&str> = self.query.split_whitespace().collect();
        let mut new_filters = Vec::new();

        for part in parts {
            if part.starts_with('@') || part.starts_with('*') {
                let filter_name = part[1..].to_lowercase();
                // CRITICAL: Only add filter if it has a name (not just "@" or "*")
                // This prevents triggering lazy loading when user is still typing "@nix"
                if !filter_name.is_empty() {
                    new_filters.push(filter_name);
                }
            }
        }

        // Update filter_managers if we found any VALID filters
        if !new_filters.is_empty() {
            // Only trigger lazy loading if filters actually changed
            let filters_changed = new_filters != self.filter_managers;
            self.filter_managers = new_filters.clone();

            // Lazy load packages for newly requested managers (only if filters changed)
            if filters_changed {
                self.lazy_load_managers(&new_filters);
            }
        } else if self.query.is_empty() {
            // Clear filters if query is empty
            self.filter_managers.clear();
        }
    }

    fn lazy_load_managers(&mut self, requested_managers: &[String]) {
        use rayon::prelude::*;

        let cache = CacheManager::new().ok();
        if cache.is_none() {
            return;
        }
        let cache = cache.unwrap();

        // Find managers that need to be loaded
        // CRITICAL: Only load managers that are NOT already loaded
        // This prevents re-loading on every keystroke
        let to_load: Vec<_> = self
            .managers
            .iter()
            .filter(|m| {
                let name = m.name().to_lowercase();
                // Only load if:
                // 1. The manager name matches a requested filter, AND
                // 2. The manager is NOT already loaded
                requested_managers
                    .iter()
                    .any(|req| name == req.to_lowercase())
                    && !self.loaded_managers.contains(&name)
            })
            .collect();

        if to_load.is_empty() {
            return;
        }

        // Load in parallel
        let results: Vec<_> = to_load
            .par_iter()
            .map(|manager| {
                let cache_key = format!("{}_all", manager.name());
                let name = manager.name().to_string();

                // Try cache first
                let packages = if let Ok(false) = cache.is_stale(&cache_key, 3600) {
                    cache.get(&cache_key).ok().flatten().unwrap_or_default()
                } else {
                    let pkgs = manager.list_all().unwrap_or_default();
                    let _ = cache.set(&cache_key, pkgs.clone());
                    pkgs
                };

                (name, packages)
            })
            .collect();

        // Merge results and trigger re-filter
        for (name, packages) in results {
            let manager_lower = name.to_lowercase();
            let start_idx = self.all_packages.len();
            // Update manager index with new packages
            for (offset, _) in packages.iter().enumerate() {
                let idx = start_idx + offset;
                self.manager_indices
                    .entry(manager_lower.clone())
                    .or_insert_with(Vec::new)
                    .push(idx);
            }
            self.all_packages.extend(packages);
            self.loaded_managers.insert(name);
        }

        // Force re-filter to show new packages
        self.last_query.clear();
        self.filter();
    }

    fn get_search_query(&self) -> String {
        // Get the search query without the @pm or *pm filters
        self.query
            .split_whitespace()
            .filter(|part| !part.starts_with('@') && !part.starts_with('*'))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn filter(&mut self) {
        let search_query = self.get_search_query();
        let query_lower = search_query.to_lowercase();
        let has_query = !search_query.is_empty();

        // Fast path: no query and no filters
        if !has_query && self.filter_managers.is_empty() {
            // Only rebuild if not already showing all
            if self.shown_indices.len() != self.all_packages.len().min(10_000) {
                self.shown_indices = (0..self.all_packages.len().min(10_000)).collect();
                self.selected_index = Some(0);
                self.scroll_offset = 0;
            }
            return;
        }

        // Check for incremental query (avoid re-filtering if query unchanged)
        if has_query && query_lower == self.last_query {
            return;
        }
        self.last_query = query_lower.clone();

        // OPTIMIZATION: Use manager index for INSTANT filtering (no iteration through 300k packages!)
        let candidate_indices: Vec<usize> = if !self.filter_managers.is_empty() {
            // Use manager index - INSTANT lookup instead of iterating all packages!
            let mut indices = Vec::new();
            for pm_filter in &self.filter_managers {
                let pm_lower = pm_filter.to_lowercase();
                // Map filter names to actual manager names
                let manager_names: Vec<&str> = match pm_lower.as_str() {
                    "aur" | "paru" | "yay" => vec!["aur"],
                    "emerge" | "gentoo" | "portage" => vec!["emerge"],
                    "nix" => vec!["nix"],
                    "dnf" => vec!["dnf"],
                    "pacman" => vec!["pacman"],
                    "pkgit" => vec!["pkgit"],
                    _ => vec![pm_lower.as_str()],
                };
                
                for manager_name in manager_names {
                    if let Some(pkg_indices) = self.manager_indices.get(manager_name) {
                        indices.extend(pkg_indices.iter().copied());
                    }
                }
            }
            indices
        } else {
            (0..self.all_packages.len()).collect()
        };

        // Score and filter candidates - SMART LIMIT: process up to 50k candidates per keystroke
        // This prevents CPU spikes while still showing all matching results
        // We process candidates in batches, but show ALL results (no truncation of final results)
        let max_candidates_to_process = 50_000;
        let candidates_to_process: Vec<usize> = if candidate_indices.len() > max_candidates_to_process {
            // For very large sets, process top 50k (prioritize host PM first)
            let mut sorted_candidates = candidate_indices;
            // Sort by host PM priority
            sorted_candidates.sort_by(|&a, &b| {
                let a_is_host = self.all_packages[a].manager == self.hostpm;
                let b_is_host = self.all_packages[b].manager == self.hostpm;
                b_is_host.cmp(&a_is_host)
            });
            sorted_candidates.into_iter().take(max_candidates_to_process).collect()
        } else {
            candidate_indices
        };
        
        // Use parallel processing for large sets, sequential for small sets
        use rayon::prelude::*;
        let mut scored: Vec<(usize, i64)> = if candidates_to_process.len() > 5_000 {
            // Parallel processing for large sets (5k+)
            candidates_to_process
                .into_par_iter()
                .filter_map(|idx| {
                    let pkg = &self.all_packages[idx];
                    self.score_package(idx, pkg, has_query, &query_lower, &search_query)
                })
                .collect()
        } else {
            // Sequential for small sets (faster overhead)
            candidates_to_process
                .into_iter()
                .filter_map(|idx| {
                    let pkg = &self.all_packages[idx];
                    self.score_package(idx, pkg, has_query, &query_lower, &search_query)
                })
                .collect()
        };

        // Sort by score, then prioritize host PM
        // NO TRUNCATION - show ALL matching packages
        if has_query {
            scored.sort_unstable_by(|a, b| {
                let score_cmp = b.1.cmp(&a.1);
                if score_cmp == std::cmp::Ordering::Equal {
                    let a_is_host = self.all_packages[a.0].manager == self.hostpm;
                    let b_is_host = self.all_packages[b.0].manager == self.hostpm;
                    b_is_host.cmp(&a_is_host)
                } else {
                    score_cmp
                }
            });
            // NO TRUNCATION - keep all results
        } else {
            scored.sort_unstable_by(|a, b| {
                let a_is_host = self.all_packages[a.0].manager == self.hostpm;
                let b_is_host = self.all_packages[b.0].manager == self.hostpm;
                b_is_host.cmp(&a_is_host)
            });
            // NO TRUNCATION - keep all results
        }

        // Deduplicate by package name
        let mut seen_names = AHashSet::new();
        let deduplicated: Vec<usize> = scored
            .into_iter()
            .filter_map(|(idx, _)| {
                let pkg_name = &self.all_packages[idx].name;
                if seen_names.insert(pkg_name.clone()) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();

        self.shown_indices = deduplicated;

        if !self.shown_indices.is_empty() {
            self.selected_index = Some(0);
            self.scroll_offset = 0;
        } else {
            self.selected_index = None;
        }
    }

    fn toggle_selection(&mut self) {
        if self.focus_installed {
            if let Some(idx) = self.installed_selected_index {
                if idx < self.installed_shown_indices.len() {
                    let pkg_idx = self.installed_shown_indices[idx];
                    if let Some(pkg) = self.installed_packages.get(pkg_idx) {
                        let pkg_key = format!("{}:{}", pkg.name, pkg.manager);
                        if self.packages_to_remove.contains(&pkg_key) {
                            self.packages_to_remove.remove(&pkg_key);
                        } else {
                            self.packages_to_remove.insert(pkg_key);
                        }
                    }
                }
            }
            return;
        }

        if let Some(idx) = self.selected_index {
            if idx < self.shown_indices.len() {
                let pkg_idx = self.shown_indices[idx];
                let pkg = &self.all_packages[pkg_idx];
                let pkg_key = format!("{}:{}", pkg.name, pkg.manager);

                // OPTIMIZED: Use helper function to avoid allocations in hot path
                let is_installed = self.is_package_installed(pkg);

                if is_installed {
                    // Toggle removal
                    if self.packages_to_remove.contains(&pkg_key) {
                        self.packages_to_remove.remove(&pkg_key);
                    } else {
                        self.packages_to_remove.insert(pkg_key);
                    }
                } else {
                    // Toggle installation - store "name:manager" to track exact package
                    if self.selected_packages.contains(&pkg_key) {
                        self.selected_packages.remove(&pkg_key);
                    } else {
                        self.selected_packages.insert(pkg_key);
                    }
                }
            }
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.shown_indices.is_empty() {
            return;
        }

        let current = self.selected_index.unwrap_or(0);
        let new_idx = if delta < 0 {
            current.saturating_sub(delta.abs() as usize)
        } else {
            (current + delta as usize).min(self.shown_indices.len() - 1)
        };

        self.selected_index = Some(new_idx);
    }

    fn get_install_command(&self) -> Option<String> {
        if self.selected_packages.is_empty() && self.packages_to_remove.is_empty() {
            return None;
        }

        let mut commands = vec![];
        let sudo_cmd = &self.config.main.sudoers;

        // Handle removals first
        if !self.packages_to_remove.is_empty() {
            for manager in &self.managers {
                let manager_name = manager.name();
                let pkgs: Vec<&Package> = self
                    .installed_packages
                    .iter()
                    .filter(|p| {
                        let pkg_key = format!("{}:{}", p.name, p.manager);
                        p.manager == manager_name && self.packages_to_remove.contains(&pkg_key)
                    })
                    .collect();

                if !pkgs.is_empty() {
                    let mut cmd = manager.remove_command(&pkgs);
                    if cmd.starts_with("sudo ") {
                        cmd = cmd.replacen("sudo", sudo_cmd, 1);
                    }
                    commands.push(cmd);
                }
            }
        }

        // Handle installations
        if !self.selected_packages.is_empty() {
            for manager in &self.managers {
                let manager_name = manager.name();
                let pkgs: Vec<&Package> = self
                    .all_packages
                    .iter()
                    .filter(|p| {
                        let pkg_key = format!("{}:{}", p.name, p.manager);
                        p.manager == manager_name && self.selected_packages.contains(&pkg_key)
                    })
                    .collect();

                if !pkgs.is_empty() {
                    let mut cmd = manager.install_command(&pkgs);
                    if cmd.starts_with("sudo ") {
                        cmd = cmd.replacen("sudo", sudo_cmd, 1);
                    }
                    commands.push(cmd);
                }
            }
        }

        if commands.is_empty() {
            None
        } else {
            Some(commands.join(" && "))
        }
    }
}

/// Load packages BEFORE terminal setup for instant startup
pub fn load_packages_before_tui(
    filter_managers: Vec<String>,
    config: &Config,
) -> Result<App> {
    let mut app = App::new(filter_managers, config.clone())?;
    app.load_packages()?;
    Ok(app)
}

pub fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    opts: crate::cli::TuiOpts,
    _config: Config,
    mut app: App,
) -> Result<Option<String>>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    // Pre-fill search if provided
    if let Some(search) = opts.search_string {
        app.query = search.clone();
        app.last_query = String::new(); // Clear last_query to FORCE filter to run
    }
    
    // Trigger initial filter if query was provided OR if filter_managers is set
    if !app.query.is_empty() || !app.filter_managers.is_empty() {
        app.filter();
    }

    let input = Input::new();
    let mut list_state = ListState::default();

    let mut first_draw = true;
    let mut installed_loaded = false;

    loop {
        terminal.draw(|f| {
            let size = f.area();

            // Main layout: left column and right column (configurable)
            let right_width = app.config.layout.right_column_width_percent;
            let left_width = 100 - right_width;
            let main_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(left_width),
                    Constraint::Percentage(right_width),
                ])
                .split(size);

            // Left column: Results, Input, Description
            // Dynamically calculate heights based on config
            let input_height = app.config.layout.input_field_height;
            let description_height = app.config.layout.description_unit_height;
            let left_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(10), // Results panel - takes remaining space
                    Constraint::Length(input_height), // Input field - fixed height
                    Constraint::Length(description_height), // Description panel - configurable height
                ])
                .split(main_chunks[0]);

            // Results panel
            let results_height = left_chunks[0].height.saturating_sub(2) as usize;
            let visible_results: Vec<ListItem> = app
                .shown_indices
                .iter()
                .skip(app.scroll_offset)
                .take(results_height)
                .map(|&idx| {
                    let pkg = &app.all_packages[idx];
                    // OPTIMIZED: Use helper function to avoid allocations in hot path
                    let is_installed = app.is_package_installed(pkg);

                    let pkg_key = format!("{}:{}", pkg.name, pkg.manager);

                    if is_installed {
                        // Installed package
                        if app.packages_to_remove.contains(&pkg_key) {
                            let description = if pkg.description.is_empty() { "" } else { &format!(" - {}", pkg.description) };
                            let line = Line::from(vec![
                                Span::styled("[-] ", Style::default().fg(Color::Red)),
                                Span::raw(format!("{} [{}]{}", pkg.name, pkg.manager, description)),
                            ]);
                            ListItem::new(line)
                        } else {
                            let description = if pkg.description.is_empty() { "" } else { &format!(" - {}", pkg.description) };
                            let line = Line::from(vec![
                                Span::raw("[=] "),
                                Span::raw(format!("{} [{}]{}", pkg.name, pkg.manager, description)),
                            ]);
                            ListItem::new(line)
                        }
                    } else {
                        // Not installed
                        if app.selected_packages.contains(&pkg_key) {
                            let description = if pkg.description.is_empty() { "" } else { &format!(" - {}", pkg.description) };
                            let line = Line::from(vec![
                                Span::styled("[+] ", Style::default().fg(Color::Green)),
                                Span::raw(format!("{} [{}]{}", pkg.name, pkg.manager, description)),
                            ]);
                            ListItem::new(line)
                        } else {
                            let description = if pkg.description.is_empty() { "" } else { &format!(" - {}", pkg.description) };
                            let line = Line::from(vec![
                                Span::raw("[ ] "),
                                Span::raw(format!("{} [{}]{}", pkg.name, pkg.manager, description)),
                            ]);
                            ListItem::new(line)
                        }
                    }
                })
                .collect();

            let border_type = if app.config.main.rounded_borders {
                BorderType::Rounded
            } else {
                BorderType::Plain
            };

            let highlight_color = app
                .config
                .parse_color(&app.config.text_colours.results_unit_highlight_text);
            let focused_border_color = app
                .config
                .parse_color(&app.config.border_colours.focused_border);
            let results_border_color = if !app.focus_installed {
                focused_border_color
            } else {
                app.config
                    .parse_color(&app.config.border_colours.results_unit)
            };
            let text_color = app
                .config
                .parse_color(&app.config.text_colours.results_unit_text);

            let title_color = app
                .config
                .parse_color(&app.config.text_colours.unit_title_text);

            let results_list = List::new(visible_results)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(border_type)
                        .border_style(Style::default().fg(results_border_color))
                        .title(Span::styled(" Results ", Style::default().fg(title_color))),
                )
                .style(Style::default().fg(text_color))
                .highlight_style(
                    Style::default()
                        .fg(highlight_color)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("> ");

            let visible_selection = app.selected_index.and_then(|sel| {
                if sel >= app.scroll_offset && sel < app.scroll_offset + results_height {
                    Some(sel - app.scroll_offset)
                } else {
                    None
                }
            });
            list_state.select(visible_selection);

            f.render_stateful_widget(results_list, left_chunks[0], &mut list_state);

            // Input panel
            let input_border_color = app
                .config
                .parse_color(&app.config.border_colours.results_unit);

            let input_text = Line::from(vec![
                Span::raw("("),
                Span::styled(
                    (app.selected_index.map_or(0, |v| v + 1)).to_string(),
                    Style::default().fg(highlight_color),
                ),
                Span::raw("/"),
                Span::raw(app.shown_indices.len().to_string()),
                Span::raw(") >> "),
                Span::raw(&app.query),
                Span::styled("█", Style::default().fg(highlight_color)),
            ]);

            let input_paragraph = Paragraph::new(input_text)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(border_type)
                        .border_style(Style::default().fg(input_border_color))
                        .title(Span::styled(" Input ", Style::default().fg(title_color))),
                )
                .style(Style::default().fg(text_color))
                .alignment(Alignment::Left);

            f.render_widget(input_paragraph, left_chunks[1]);

            // Description panel
            let description_border_color = app
                .config
                .parse_color(&app.config.border_colours.description_unit);
            let description_highlight_color = app
                .config
                .parse_color(&app.config.text_colours.description_unit_highlight_text);

            let description_text = {
                if let Some(idx) = app.selected_index {
                    if idx < app.shown_indices.len() {
                        let pkg_idx = app.shown_indices[idx];
                        let pkg = &app.all_packages[pkg_idx];

                        // OPTIMIZED: Use helper function to avoid allocations in hot path
                        let is_installed = app.is_package_installed(pkg);

                        // Format version info
                        let version_available = pkg
                            .version
                            .as_ref()
                            .map(|v| v.as_str())
                            .unwrap_or("unknown");
                        let version_installed = if is_installed {
                            pkg.version
                                .as_ref()
                                .map(|v| v.as_str())
                                .unwrap_or("unknown")
                        } else {
                            "[ Not Installed ]"
                        };

                        // Format size
                        let size_str = if let Some(size) = pkg.size {
                            let kb = size / 1024;
                            format!("{} KiB", kb)
                        } else {
                            "N/A".to_string()
                        };

                        // Format homepage
                        let homepage = if pkg.homepage.is_empty() {
                            "N/A"
                        } else {
                            &pkg.homepage
                        };

                        // Format description
                        let description = if pkg.description.is_empty() {
                            "No description available"
                        } else {
                            &pkg.description
                        };

                        // Format license
                        let license = if pkg.license.is_empty() {
                            "N/A"
                        } else {
                            &pkg.license
                        };

                        vec![
                            Line::from(Span::styled(
                                format!("*  {}", pkg.name),
                                Style::default().fg(description_highlight_color),
                            )),
                            Line::from(Span::raw(format!("      Description:   {}", description))),
                            Line::from(Span::raw(format!(
                                "      Latest version available: {}",
                                version_available
                            ))),
                            Line::from(Span::raw(if is_installed {
                                if version_installed == "unknown" {
                                    "      Installed: unknown version".to_string()
                                } else {
                                    format!("      Latest version installed: {}", version_installed)
                                }
                            } else {
                                "      Latest version installed: [ Not Installed ]".to_string()
                            })),
                            Line::from(Span::raw(format!("      Size of files: {}", size_str))),
                            Line::from(Span::raw(format!("      Homepage:      {}", homepage))),
                            Line::from(Span::raw(format!("      License:       {}", license))),
                        ]
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            };

            let description_text_color = app
                .config
                .parse_color(&app.config.text_colours.description_unit_text);
            let description_paragraph = Paragraph::new(description_text)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(border_type)
                        .border_style(Style::default().fg(description_border_color))
                        .title(Span::styled(
                            " Description ",
                            Style::default().fg(title_color),
                        )),
                )
                .style(Style::default().fg(description_text_color))
                .wrap(Wrap { trim: false });

            f.render_widget(description_paragraph, left_chunks[2]);

            // Right column: Installed packages
            let installed_height = main_chunks[1].height.saturating_sub(2) as usize;
            let visible_installed: Vec<ListItem> = app
                .installed_shown_indices
                .iter()
                .skip(app.installed_scroll_offset)
                .take(installed_height)
                .map(|&pkg_idx| {
                    let pkg = &app.installed_packages[pkg_idx];
                    let version_str = pkg
                        .version
                        .as_ref()
                        .map(|v| format!(" ({})", v))
                        .unwrap_or_default();
                    let pkg_key = format!("{}:{}", pkg.name, pkg.manager);
                    let prefix = if app.packages_to_remove.contains(&pkg_key) {
                        Line::from(vec![
                            Span::styled("[-] ", Style::default().fg(Color::Green)),
                            Span::raw(format!("{}{} [{}]", pkg.name, version_str, pkg.manager)),
                        ])
                    } else {
                        Line::from(vec![
                            Span::raw("[=] "),
                            Span::raw(format!("{}{} [{}]", pkg.name, version_str, pkg.manager)),
                        ])
                    };
                    ListItem::new(prefix)
                })
                .collect();

            let installed_border_color = if app.focus_installed {
                focused_border_color
            } else {
                app.config
                    .parse_color(&app.config.border_colours.installed_list_unit)
            };
            let installed_text_color = app
                .config
                .parse_color(&app.config.text_colours.installed_list_unit_text);

            let installed_title = if app.installed_search_active {
                format!(" Installed (search: {}) ", app.installed_filter)
            } else if !app.installed_filter.is_empty() {
                format!(" Installed (filter: {}) ", app.installed_filter)
            } else {
                " Installed (/ search · Ctrl+Space remove) ".to_string()
            };

            let installed_list = List::new(visible_installed)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(border_type)
                        .border_style(Style::default().fg(installed_border_color))
                        .title(Span::styled(
                            installed_title,
                            Style::default().fg(title_color),
                        )),
                )
                .style(Style::default().fg(installed_text_color))
                .highlight_style(
                    Style::default()
                        .fg(highlight_color)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("> ");

            let mut installed_state = ListState::default();
            let installed_visible_selection = app.installed_selected_index.and_then(|sel| {
                if sel >= app.installed_scroll_offset
                    && sel < app.installed_scroll_offset + installed_height
                {
                    Some(sel - app.installed_scroll_offset)
                } else {
                    None
                }
            });
            installed_state.select(installed_visible_selection);

            f.render_stateful_widget(installed_list, main_chunks[1], &mut installed_state);
        })?;

        // Load installed packages after first draw (for instant startup)
        // Only once per launch - no periodic refresh
        if first_draw && !installed_loaded {
            app.load_installed_packages();
            installed_loaded = true;
            first_draw = false;
        }

        match input.next()? {
            Event::Key(key) => {
                // Allow navigation keys even when search is active
                let is_nav_key = matches!(
                    key.code,
                    KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::PageUp
                        | KeyCode::PageDown
                        | KeyCode::Home
                        | KeyCode::End
                ) || matches!(key.code, KeyCode::Char('k' | 'j'))
                    && key.modifiers.contains(KeyModifiers::ALT)
                    || matches!(key.code, KeyCode::Char('u' | 'd'))
                        && key.modifiers.contains(KeyModifiers::CONTROL);

                if app.focus_installed && app.installed_search_active && !is_nav_key {
                    match key.code {
                        KeyCode::Esc => {
                            app.installed_search_active = false;
                            app.installed_filter.clear();
                            app.refresh_installed_view();
                        }
                        KeyCode::Enter => {
                            app.installed_search_active = false;
                        }
                        KeyCode::Backspace => {
                            app.installed_filter.pop();
                            app.refresh_installed_view();
                        }
                        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.installed_filter.push(c);
                            app.refresh_installed_view();
                        }
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(None)
                    }
                    // Removed standalone 'q' key so it can be typed in search
                    // Use Esc or Ctrl+C to quit
                    KeyCode::Char(' ') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.toggle_selection();
                    }
                    KeyCode::Enter => {
                        // If nothing selected, select current item
                        if app.selected_packages.is_empty() && app.packages_to_remove.is_empty() {
                            app.toggle_selection();
                        }

                        if let Some(cmd) = app.get_install_command() {
                            return Ok(Some(cmd));
                        }
                    }
                    // Arrow key navigation (always works)
                    KeyCode::Up => {
                        if app.focus_installed {
                            let visible_height = terminal.size()?.height.saturating_sub(2) as usize;
                            app.move_installed_selection(-1, visible_height.max(1));
                        } else {
                            app.move_selection(-1);
                            if let Some(sel) = app.selected_index {
                                if sel < app.scroll_offset {
                                    app.scroll_offset = sel;
                                }
                            }
                        }
                    }
                    KeyCode::Down => {
                        if app.focus_installed {
                            let visible_height = terminal.size()?.height.saturating_sub(2) as usize;
                            app.move_installed_selection(1, visible_height.max(1));
                        } else {
                            app.move_selection(1);
                            if let Some(sel) = app.selected_index {
                                // Calculate actual results panel visible height (accounts for input + description)
                                let term_height = terminal.size()?.height;
                                let input_height = app.config.layout.input_field_height;
                                let description_height = app.config.layout.description_unit_height;
                                // Results panel height = term_height - input_height - description_height - 2 (borders)
                                let visible_height = term_height
                                    .saturating_sub(input_height)
                                    .saturating_sub(description_height)
                                    .saturating_sub(2) as usize;
                                if sel >= app.scroll_offset + visible_height {
                                    app.scroll_offset = sel.saturating_sub(visible_height - 1);
                                }
                            }
                        }
                    }
                    // Vim-style navigation (with Alt modifier so j/k can be typed in search)
                    KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::ALT) => {
                        if app.focus_installed {
                            let visible_height = terminal.size()?.height.saturating_sub(2) as usize;
                            app.move_installed_selection(-1, visible_height.max(1));
                        } else {
                            app.move_selection(-1);
                            if let Some(sel) = app.selected_index {
                                if sel < app.scroll_offset {
                                    app.scroll_offset = sel;
                                }
                            }
                        }
                    }
                    KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::ALT) => {
                        if app.focus_installed {
                            let visible_height = terminal.size()?.height.saturating_sub(2) as usize;
                            app.move_installed_selection(1, visible_height.max(1));
                        } else {
                            app.move_selection(1);
                            if let Some(sel) = app.selected_index {
                                // Calculate actual results panel visible height (accounts for input + description)
                                let term_height = terminal.size()?.height;
                                let input_height = app.config.layout.input_field_height;
                                let description_height = app.config.layout.description_unit_height;
                                // Results panel height = term_height - input_height - description_height - 2 (borders)
                                let visible_height = term_height
                                    .saturating_sub(input_height)
                                    .saturating_sub(description_height)
                                    .saturating_sub(2) as usize;
                                if sel >= app.scroll_offset + visible_height {
                                    app.scroll_offset = sel.saturating_sub(visible_height - 1);
                                }
                            }
                        }
                    }
                    // Tab to switch focus between results and installed
                    KeyCode::Tab => {
                        app.installed_search_active = false;
                        app.focus_installed = !app.focus_installed;
                    }
                    KeyCode::Char('/') if app.focus_installed => {
                        app.installed_search_active = true;
                        app.installed_filter.clear();
                        app.refresh_installed_view();
                    }
                    // Fast scroll
                    KeyCode::PageUp | KeyCode::Char('u')
                        if key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        // Calculate actual results panel visible height (accounts for input + description)
                        let term_height = terminal.size()?.height;
                        let input_height = app.config.layout.input_field_height;
                        let description_height = app.config.layout.description_unit_height;
                        // Results panel height = term_height - input_height - description_height - 2 (borders)
                        let visible_height = term_height
                            .saturating_sub(input_height)
                            .saturating_sub(description_height)
                            .saturating_sub(2) as usize;
                        app.move_selection(-(visible_height as isize / 2));
                        if let Some(sel) = app.selected_index {
                            app.scroll_offset = sel.saturating_sub(visible_height / 2);
                        }
                    }
                    KeyCode::PageDown | KeyCode::Char('d')
                        if key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        // Calculate actual results panel visible height (accounts for input + description)
                        let term_height = terminal.size()?.height;
                        let input_height = app.config.layout.input_field_height;
                        let description_height = app.config.layout.description_unit_height;
                        // Results panel height = term_height - input_height - description_height - 2 (borders)
                        let visible_height = term_height
                            .saturating_sub(input_height)
                            .saturating_sub(description_height)
                            .saturating_sub(2) as usize;
                        app.move_selection(visible_height as isize / 2);
                        if let Some(sel) = app.selected_index {
                            if sel >= app.scroll_offset + visible_height {
                                app.scroll_offset = sel.saturating_sub(visible_height - 1);
                            }
                        }
                    }
                    // Jump to start/end (only with Home/End keys, not g/G)
                    KeyCode::Home => {
                        app.selected_index = Some(0);
                        app.scroll_offset = 0;
                    }
                    KeyCode::End => {
                        if !app.shown_indices.is_empty() {
                            app.selected_index = Some(app.shown_indices.len() - 1);
                            // Calculate actual results panel visible height (accounts for input + description)
                            let term_height = terminal.size()?.height;
                            let input_height = app.config.layout.input_field_height;
                            let description_height = app.config.layout.description_unit_height;
                            // Results panel height = term_height - input_height - description_height - 2 (borders)
                            let visible_height = term_height
                                .saturating_sub(input_height)
                                .saturating_sub(description_height)
                                .saturating_sub(2) as usize;
                            app.scroll_offset =
                                app.shown_indices.len().saturating_sub(visible_height);
                        }
                    }
                    // Clear search with Ctrl+L
                    KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.query.clear();
                        app.filter();
                    }
                    // Regular character input
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.query.push(c);
                        app.parse_query_filters();
                        app.filter();
                    }
                    KeyCode::Backspace => {
                        app.query.pop();
                        app.parse_query_filters();
                        app.filter();
                    }
                    _ => {}
                }
            }
            Event::Mouse(mouse) => {
                let mouse_row = mouse.row;
                let term_size = terminal.size()?;

                // Calculate panel boundaries - must match the layout calculations
                let right_width = app.config.layout.right_column_width_percent;
                let left_width = 100 - right_width;
                let left_column_width = (term_size.width as f32 * left_width as f32 / 100.0) as u16;

                // Results panel is in top left
                // Calculate the same way as in the draw loop
                let input_height = app.config.layout.input_field_height;
                let description_height = app.config.layout.description_unit_height;
                // Results panel starts at row 1 (after top border) and ends before input field
                let results_start = 1; // After border
                // The results panel takes up: term_size.height - input_height - description_height
                // But we need to account for borders: top border (1) + bottom border (1) = 2
                // So results_end = term_size.height - input_height - description_height - 1 (for bottom border)
                let results_end = term_size.height.saturating_sub(input_height + description_height + 1);

                // Installed panel is on the right - spans full height
                // The installed panel uses main_chunks[1] which spans full height
                // Content area is main_chunks[1].height - 2 (top + bottom borders)
                // Mouse coordinates: row 0 = top border, row 1 = first content row
                // The last content row is height - 2 (height - 1 is bottom border)
                let installed_start = 1; // After top border (row 0 is border, row 1 is first content)
                // installed_end should be exclusive, so height - 1 means we can click up to height - 2
                // But we need to make sure we can click the last visible row, so use height (exclusive)
                let _installed_end = term_size.height; // Exclusive: can click up to height - 1 (which is the bottom border, but we check < installed_end)
                let installed_column_start = left_column_width;

                match mouse.kind {
                    MouseEventKind::Moved => {
                        // Hover highlighting
                        if mouse.column < installed_column_start {
                            // In results panel
                            if mouse_row >= results_start && mouse_row < results_end {
                                let row_in_content = (mouse_row - results_start) as usize;
                                let new_selection = app.scroll_offset + row_in_content;
                                if new_selection < app.shown_indices.len() {
                                    app.selected_index = Some(new_selection);
                                    app.focus_installed = false;
                                }
                            }
                        } else {
                            // In installed panel - update selection to follow mouse
                            app.focus_installed = true;
                            // Allow clicking up to the last visible row (height - 2, since height - 1 is bottom border)
                            if mouse_row >= installed_start && mouse_row < term_size.height.saturating_sub(1) {
                                // Account for top border (1 line) - mouse_row is 0-indexed from terminal
                                // installed_start is 1 (after border), so row_in_content = mouse_row - installed_start
                                let row_in_content = (mouse_row - installed_start) as usize;
                                let new_selection = app.installed_scroll_offset + row_in_content;
                                if new_selection < app.installed_shown_indices.len() {
                                    app.installed_selected_index = Some(new_selection);
                                }
                            }
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        if mouse.column < installed_column_start {
                            // Click in results panel
                            if mouse_row >= results_start && mouse_row < results_end {
                                let row_in_content = (mouse_row - results_start) as usize;
                                let clicked_index = app.scroll_offset + row_in_content;
                                if clicked_index < app.shown_indices.len() {
                                    app.selected_index = Some(clicked_index);
                                    app.toggle_selection();
                                }
                            }
                        } else {
                            // Click in installed panel
                            app.focus_installed = true;
                            // Allow clicking up to the last visible row (height - 2, since height - 1 is bottom border)
                            if mouse_row >= installed_start && mouse_row < term_size.height.saturating_sub(1) {
                                // Account for top border (1 line) - mouse_row is 0-indexed from terminal
                                // installed_start is 1 (after border), so row_in_content = mouse_row - installed_start
                                let row_in_content = (mouse_row - installed_start) as usize;
                                let clicked_index =
                                    app.installed_scroll_offset + row_in_content;
                                if clicked_index < app.installed_shown_indices.len() {
                                    app.installed_selected_index = Some(clicked_index);
                                    app.toggle_selection();
                                }
                            }
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        if mouse.column < installed_column_start {
                            // Scroll results
                            if !app.shown_indices.is_empty() && app.scroll_offset > 0 {
                                app.scroll_offset = app.scroll_offset.saturating_sub(1);
                                // Update selection to match mouse position after scrolling (like fsel)
                                if mouse_row >= results_start && mouse_row < results_end {
                                    let row_in_content = (mouse_row - results_start) as usize;
                                    let new_selection = app.scroll_offset + row_in_content;
                                    if new_selection < app.shown_indices.len() {
                                        app.selected_index = Some(new_selection);
                                    }
                                }
                                // Ensure selection stays visible
                                if let Some(sel) = app.selected_index {
                                    let visible_height = (results_end - results_start) as usize;
                                    if sel >= app.scroll_offset + visible_height {
                                        app.selected_index = Some(app.scroll_offset);
                                    }
                                }
                            }
                        } else {
                            // Scroll installed
                            if !app.installed_shown_indices.is_empty() && app.installed_scroll_offset > 0 {
                                app.installed_scroll_offset =
                                    app.installed_scroll_offset.saturating_sub(1);
                                // Update selection to match mouse position after scrolling (like fsel)
                                if mouse_row >= installed_start && mouse_row < term_size.height.saturating_sub(1) {
                                    let row_in_content = (mouse_row - installed_start) as usize;
                                    let new_selection = app.installed_scroll_offset + row_in_content;
                                    if new_selection < app.installed_shown_indices.len() {
                                        app.installed_selected_index = Some(new_selection);
                                    }
                                }
                                // Ensure selection stays visible
                                if let Some(sel) = app.installed_selected_index {
                                    // visible_height = (height - 1) - 1 = height - 2 (content area)
                                    let visible_height = (term_size.height.saturating_sub(1) - installed_start) as usize;
                                    if sel >= app.installed_scroll_offset + visible_height {
                                        app.installed_selected_index = Some(app.installed_scroll_offset);
                                    }
                                }
                            }
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        if mouse.column < installed_column_start {
                            // Scroll results
                            if !app.shown_indices.is_empty() {
                                let visible_height = (results_end - results_start) as usize;
                                let max_scroll =
                                    app.shown_indices.len().saturating_sub(visible_height);
                                if app.scroll_offset < max_scroll {
                                    app.scroll_offset = (app.scroll_offset + 1).min(max_scroll);
                                    // Update selection to match mouse position after scrolling (like fsel)
                                    if mouse_row >= results_start && mouse_row < results_end {
                                        let row_in_content = (mouse_row - results_start) as usize;
                                        let new_selection = app.scroll_offset + row_in_content;
                                        if new_selection < app.shown_indices.len() {
                                            app.selected_index = Some(new_selection);
                                        }
                                    }
                                    // Ensure selection stays visible
                                    if let Some(sel) = app.selected_index {
                                        if sel < app.scroll_offset {
                                            app.selected_index = Some(app.scroll_offset);
                                        }
                                    }
                                }
                            }
                        } else {
                            // Scroll installed
                            // visible_height = (height - 1) - 1 = height - 2 (content area)
                            let visible_height = (term_size.height.saturating_sub(1) - installed_start) as usize;
                            if !app.installed_shown_indices.is_empty() {
                                let max_scroll = app
                                    .installed_shown_indices
                                    .len()
                                    .saturating_sub(visible_height);
                                if app.installed_scroll_offset < max_scroll {
                                    app.installed_scroll_offset =
                                        (app.installed_scroll_offset + 1).min(max_scroll);
                                    // Update selection to match mouse position after scrolling (like fsel)
                                    if mouse_row >= installed_start && mouse_row < term_size.height.saturating_sub(1) {
                                        let row_in_content = (mouse_row - installed_start) as usize;
                                        let new_selection = app.installed_scroll_offset + row_in_content;
                                        if new_selection < app.installed_shown_indices.len() {
                                            app.installed_selected_index = Some(new_selection);
                                        }
                                    }
                                    // Ensure selection stays visible
                                    if let Some(sel) = app.installed_selected_index {
                                        if sel < app.installed_scroll_offset {
                                            app.installed_selected_index = Some(app.installed_scroll_offset);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Tick => {}
            Event::Resize(_, _) => {
                // Ratatui handles redraws automatically on resize
            }
        }
    }
}
