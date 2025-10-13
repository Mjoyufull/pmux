use crate::cache::CacheManager;
use crate::config::Config;
use crate::input::{Event, Input};
use crate::pm::{detect_available_managers, Package, PackageManager};
use crate::search_index::SearchIndex;
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
    installed_set: AHashSet<String>, // Fast lookup for installed packages
    selected_packages: AHashSet<String>, // Format: "name:manager"
    packages_to_remove: AHashSet<String>,
    selected_index: Option<usize>,
    query: String,
    last_query: String, // Track last query for incremental filtering
    scroll_offset: usize,
    installed_scroll_offset: usize,
    matcher: SkimMatcherV2,
    search_index: SearchIndex, // Pre-built search index
    managers: Vec<Box<dyn PackageManager>>,
    filter_managers: Vec<String>,
    config: Config,
    focus_installed: bool,
    hostpm: String,
    loaded_managers: AHashSet<String>, // Track which PMs have been loaded
}

impl App {
    fn new(filter_managers: Vec<String>, config: Config) -> Result<Self> {
        let hostpm = config.pm.hostpm.clone();
        
        // Determine which managers to actually use
        let managers = detect_available_managers(&hostpm, &config.pm.enabled_pm);
        
        if managers.is_empty() {
            return Err(eyre::eyre!("No supported package managers found"));
        }
        
        // Default to showing only host PM packages
        let initial_filters = if filter_managers.is_empty() {
            vec![hostpm.clone()]
        } else {
            filter_managers
        };
        
        Ok(Self {
            all_packages: Vec::with_capacity(500_000),
            shown_indices: Vec::with_capacity(10_000),
            installed_packages: Vec::with_capacity(5_000),
            installed_set: AHashSet::with_capacity(5_000),
            selected_packages: AHashSet::new(),
            packages_to_remove: AHashSet::new(),
            selected_index: Some(0),
            query: String::new(),
            last_query: String::new(),
            scroll_offset: 0,
            installed_scroll_offset: 0,
            matcher: SkimMatcherV2::default(),
            search_index: SearchIndex::new(),
            managers,
            filter_managers: initial_filters,
            config,
            focus_installed: false,
            hostpm: hostpm.clone(),
            loaded_managers: AHashSet::from_iter(vec![hostpm]),
        })
    }
    
    fn load_packages(&mut self) -> Result<()> {
        use rayon::prelude::*;
        use std::time::Instant;
        use crate::logger::{log_timing, log_info, log_debug};
        
        let total_start = Instant::now();
        log_info("Starting package load");
        
        let cache_start = Instant::now();
        let cache = CacheManager::new()?;
        log_timing("Cache init", cache_start.elapsed());
        
        // OPTIMIZATION: Only load host PM packages initially for instant startup
        // Other PMs will be loaded on-demand when user filters to them
        let priority_managers: Vec<_> = self.managers.iter()
            .filter(|m| m.name() == self.hostpm)
            .collect();
        
        log_info(&format!("Loading {} priority managers", priority_managers.len()));
        
        // Load priority (host PM) packages first
        let load_start = Instant::now();
        let results: Vec<_> = priority_managers.par_iter()
            .map(|manager| {
                let manager_start = Instant::now();
                let cache_key = format!("{}_all", manager.name());
                
                // Try cache first (1 hour TTL)
                let packages = if let Ok(false) = cache.is_stale(&cache_key, 3600) {
                    let cache_load_start = Instant::now();
                    let pkgs = cache.get(&cache_key).ok().flatten().unwrap_or_default();
                    log_debug(&format!("{} cache load: {:?} ({} packages)", 
                        manager.name(), cache_load_start.elapsed(), pkgs.len()));
                    pkgs
                } else {
                    // Parse from disk
                    let parse_start = Instant::now();
                    let pkgs = manager.list_all().unwrap_or_default();
                    log_debug(&format!("{} parse from disk: {:?} ({} packages)", 
                        manager.name(), parse_start.elapsed(), pkgs.len()));
                    
                    let cache_write_start = Instant::now();
                    let _ = cache.set(&cache_key, pkgs.clone());
                    log_debug(&format!("{} cache write: {:?}", 
                        manager.name(), cache_write_start.elapsed()));
                    pkgs
                };
                
                log_timing(&format!("{} total", manager.name()), manager_start.elapsed());
                (manager.name().to_string(), packages)
            })
            .collect();
        log_timing("Priority load total", load_start.elapsed());
        
        // Merge priority packages
        let merge_start = Instant::now();
        for (_name, packages) in results {
            self.all_packages.extend(packages);
        }
        log_timing("Merge packages", merge_start.elapsed());
        
        // Load installed packages from ALL managers in parallel
        // CACHE installed packages since they rarely change
        let installed_start = Instant::now();
        let installed_results: Vec<_> = self.managers.par_iter()
            .map(|manager| {
                let start = Instant::now();
                let cache_key = format!("{}_installed", manager.name());
                
                // Try cache first (24 hour TTL for installed packages)
                let pkgs = if let Ok(Some(cached)) = cache.get_installed(&cache_key) {
                    log_debug(&format!("{} installed (cached): {:?} ({} packages)", 
                        manager.name(), start.elapsed(), cached.len()));
                    cached
                } else {
                    let pkgs = manager.list_installed().unwrap_or_default();
                    log_debug(&format!("{} installed (fresh): {:?} ({} packages)", 
                        manager.name(), start.elapsed(), pkgs.len()));
                    let _ = cache.set_installed(&cache_key, pkgs.clone());
                    pkgs
                };
                
                pkgs
            })
            .collect();
        
        for installed in installed_results {
            if !installed.is_empty() {
                // Build fast lookup set
                for pkg in &installed {
                    self.installed_set.insert(format!("{}:{}", pkg.name, pkg.manager));
                }
                self.installed_packages.extend(installed);
            }
        }
        log_timing("Load installed", installed_start.elapsed());
        
        // Skip search index - too slow for 80k+ packages
        // Direct filtering is fast enough
        log_debug("Skipping search index build (using direct filtering)");
        
        // Skip initial filter - just show first 10k packages
        // Filter will happen on first keystroke
        self.shown_indices = (0..self.all_packages.len().min(10_000)).collect();
        if !self.shown_indices.is_empty() {
            self.selected_index = Some(0);
        }
        log_debug("Skipped initial filter for instant startup");
        
        log_timing("TOTAL LOAD TIME", total_start.elapsed());
        log_info(&format!("Loaded {} packages total", self.all_packages.len()));
        
        // Skip background loading - it blocks for 800ms!
        // Other PMs will be lazy-loaded when user filters to them with @pm
        log_debug("Skipping background load - using lazy loading instead");
        
        Ok(())
    }
    
    #[allow(dead_code)]
    fn build_search_index(&mut self) {
        // Build index of package names for O(k) search
        let package_names: Vec<(usize, String)> = self.all_packages
            .iter()
            .enumerate()
            .map(|(idx, pkg)| (idx, pkg.name.clone()))
            .collect();
        
        self.search_index.build(&package_names);
    }
    
    fn background_load_remaining_managers(&mut self) {
        use rayon::prelude::*;
        
        let cache = match CacheManager::new() {
            Ok(c) => c,
            Err(_) => return,
        };
        
        // Find managers that haven't been loaded yet
        let to_load: Vec<_> = self.managers.iter()
            .filter(|m| !self.loaded_managers.contains(m.name()))
            .collect();
        
        if to_load.is_empty() {
            return;
        }
        
        // Load in parallel in background
        let results: Vec<_> = to_load.par_iter()
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
        
        // Merge results and rebuild search index
        let start_idx = self.all_packages.len();
        for (name, packages) in results {
            self.all_packages.extend(packages);
            self.loaded_managers.insert(name);
        }
        
        // Incrementally update search index with new packages
        let new_packages: Vec<(usize, String)> = self.all_packages[start_idx..]
            .iter()
            .enumerate()
            .map(|(i, pkg)| (start_idx + i, pkg.name.clone()))
            .collect();
        
        if !new_packages.is_empty() {
            self.search_index.build(&new_packages);
        }
    }
    
    fn parse_query_filters(&mut self) {
        // Extract @pm or *pm filters from query without modifying the query string
        let parts: Vec<&str> = self.query.split_whitespace().collect();
        let mut new_filters = Vec::new();
        
        for part in parts {
            if part.starts_with('@') || part.starts_with('*') {
                new_filters.push(part[1..].to_lowercase());
            }
        }
        
        // Update filter_managers if we found any
        if !new_filters.is_empty() {
            self.filter_managers = new_filters.clone();
            
            // Lazy load packages for newly requested managers
            self.lazy_load_managers(&new_filters);
        } else if self.query.is_empty() {
            // Clear filters if query is empty
            self.filter_managers.clear();
        }
    }
    
    fn lazy_load_managers(&mut self, requested_managers: &[String]) {
        use rayon::prelude::*;
        use crate::logger::{log_info, log_timing};
        use std::time::Instant;
        
        let cache = CacheManager::new().ok();
        if cache.is_none() {
            return;
        }
        let cache = cache.unwrap();
        
        // Find managers that need to be loaded
        let to_load: Vec<_> = self.managers.iter()
            .filter(|m| {
                let name = m.name().to_lowercase();
                requested_managers.iter().any(|req| {
                    let req_lower = req.to_lowercase();
                    name == req_lower && !self.loaded_managers.contains(&name)
                })
            })
            .collect();
        
        if to_load.is_empty() {
            return;
        }
        
        log_info(&format!("Lazy loading {} managers", to_load.len()));
        
        // Load in parallel
        let results: Vec<_> = to_load.par_iter()
            .map(|manager| {
                let start = Instant::now();
                let cache_key = format!("{}_all", manager.name());
                let name = manager.name().to_string();
                
                // Try cache first
                let packages = if let Ok(false) = cache.is_stale(&cache_key, 3600) {
                    let pkgs = cache.get(&cache_key).ok().flatten().unwrap_or_default();
                    log_timing(&format!("{} lazy load (cached)", name), start.elapsed());
                    pkgs
                } else {
                    let pkgs = manager.list_all().unwrap_or_default();
                    log_timing(&format!("{} lazy load (parse)", name), start.elapsed());
                    let _ = cache.set(&cache_key, pkgs.clone());
                    pkgs
                };
                
                (name, packages)
            })
            .collect();
        
        // Merge results and trigger re-filter
        let start_idx = self.all_packages.len();
        for (name, packages) in results {
            log_info(&format!("Loaded {} packages from {}", packages.len(), name));
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
        
        // Skip search index for now - direct filtering is faster for initial load
        let candidate_indices: Vec<usize> = if has_query {
            if query_lower == self.last_query {
                // No change, skip filtering
                return;
            }
            
            self.last_query = query_lower.clone();
            
            // Direct filtering - fast enough for most cases
            (0..self.all_packages.len()).collect()
        } else {
            // No query - use all packages
            (0..self.all_packages.len()).collect()
        };
        
        // Fast scoring without parallel overhead for small candidate sets
        let mut scored: Vec<(usize, i64)> = candidate_indices
            .into_iter()
            .filter_map(|idx| {
                if idx >= self.all_packages.len() {
                    return None;
                }
                
                let pkg = &self.all_packages[idx];
                
                // PM filter
                if !self.filter_managers.is_empty() {
                    let pkg_manager_lower = pkg.manager.to_lowercase();
                    let matches = self.filter_managers.iter().any(|pm| {
                        let pm_lower = pm.to_lowercase();
                        pkg_manager_lower == pm_lower || 
                        (pm_lower == "aur" && pkg_manager_lower == "aur") ||
                        (pm_lower == "paru" && pkg_manager_lower == "aur") ||
                        (pm_lower == "yay" && pkg_manager_lower == "aur") ||
                        (pm_lower == "emerge" && pkg_manager_lower == "emerge") ||
                        (pm_lower == "gentoo" && pkg_manager_lower == "emerge") ||
                        (pm_lower == "portage" && pkg_manager_lower == "emerge")
                    });
                    if !matches {
                        return None;
                    }
                }
                
                // Search query scoring
                if has_query {
                    let name_lower = pkg.name.to_lowercase();
                    
                    // Exact match
                    if name_lower == query_lower {
                        return Some((idx, 1_000_000));
                    }
                    
                    // Starts with
                    if name_lower.starts_with(&query_lower) {
                        return Some((idx, 900_000));
                    }
                    
                    // Contains
                    if name_lower.contains(&query_lower) {
                        return Some((idx, 800_000));
                    }
                    
                    // Fuzzy match as fallback
                    if let Some(score) = self.matcher.fuzzy_match(&pkg.name, &search_query) {
                        return Some((idx, score));
                    }
                    
                    None
                } else {
                    Some((idx, 0))
                }
            })
            .collect();
        
        // Sort by score, then prioritize host PM
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
            scored.truncate(5_000);
        } else {
            scored.sort_unstable_by(|a, b| {
                let a_is_host = self.all_packages[a.0].manager == self.hostpm;
                let b_is_host = self.all_packages[b.0].manager == self.hostpm;
                b_is_host.cmp(&a_is_host)
            });
            scored.truncate(10_000);
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
        if let Some(idx) = self.selected_index {
            if idx < self.shown_indices.len() {
                let pkg_idx = self.shown_indices[idx];
                let pkg = &self.all_packages[pkg_idx];
                let pkg_key = format!("{}:{}", pkg.name, pkg.manager);
                
                // Fast O(1) lookup
                let is_installed = self.installed_set.contains(&pkg_key);
                
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
        if self.selected_packages.is_empty() {
            return None;
        }
        
        // Group packages by manager
        let mut commands = vec![];
        let sudo_cmd = &self.config.main.sudoers;
        
        for manager in &self.managers {
            let manager_name = manager.name();
            let pkgs: Vec<&Package> = self
                .all_packages
                .iter()
                .filter(|p| {
                    // Only match packages from THIS specific manager
                    let pkg_key = format!("{}:{}", p.name, p.manager);
                    p.manager == manager_name && self.selected_packages.contains(&pkg_key)
                })
                .collect();
            
            if !pkgs.is_empty() {
                let mut cmd = manager.install_command(&pkgs);
                // Replace "sudo" with configured sudo command
                if cmd.starts_with("sudo ") {
                    cmd = cmd.replacen("sudo", sudo_cmd, 1);
                }
                commands.push(cmd);
            }
        }
        
        if commands.is_empty() {
            None
        } else {
            Some(commands.join(" && "))
        }
    }
}

pub fn run<B: Backend>(terminal: &mut Terminal<B>, opts: crate::cli::TuiOpts, config: Config) -> Result<Option<String>> {
    use std::time::Instant;
    use crate::logger::log_timing;
    
    let app_start = Instant::now();
    let mut app = App::new(opts.filter_managers.clone(), config.clone())?;
    log_timing("App::new", app_start.elapsed());
    
    // Pre-fill search if provided
    if let Some(search) = opts.search_string {
        app.query = search;
    }
    
    // Load packages silently - no loading screen, just do it
    let load_start = Instant::now();
    app.load_packages()?;
    log_timing("app.load_packages", load_start.elapsed());
    
    let input = Input::new();
    let mut list_state = ListState::default();
    
    let mut first_draw = true;
    
    loop {
        let draw_start = if first_draw { Some(Instant::now()) } else { None };
        
        terminal.draw(|f| {
            let size = f.size();
            
            // Main layout: left column and right column (configurable)
            let right_width = app.config.layout.right_column_width_percent;
            let left_width = 100 - right_width;
            let main_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(left_width), Constraint::Percentage(right_width)])
                .split(size);
            
            // Left column: Results, Input, Description
            let input_height = app.config.layout.input_field_height;
            let left_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(10),
                    Constraint::Length(input_height),
                    Constraint::Length(5),
                ])
                .split(main_chunks[0]);
            
            // Results panel
            let results_height = left_chunks[0].height.saturating_sub(2) as usize;
            let visible_results: Vec<ListItem> = app.shown_indices
                .iter()
                .skip(app.scroll_offset)
                .take(results_height)
                .map(|&idx| {
                    let pkg = &app.all_packages[idx];
                    // Fast O(1) lookup
                    let pkg_key = format!("{}:{}", pkg.name, pkg.manager);
                    let is_installed = app.installed_set.contains(&pkg_key);
                    
                    let pkg_key = format!("{}:{}", pkg.name, pkg.manager);
                    
                    if is_installed {
                        // Installed package
                        if app.packages_to_remove.contains(&pkg_key) {
                            let line = Line::from(vec![
                                Span::styled("[-] ", Style::default().fg(Color::Red)),
                                Span::raw(format!("{} [{}]", pkg.name, pkg.manager)),
                            ]);
                            ListItem::new(line)
                        } else {
                            let line = Line::from(vec![
                                Span::raw("[=] "),
                                Span::raw(format!("{} [{}]", pkg.name, pkg.manager)),
                            ]);
                            ListItem::new(line)
                        }
                    } else {
                        // Not installed
                        if app.selected_packages.contains(&pkg_key) {
                            let line = Line::from(vec![
                                Span::styled("[+] ", Style::default().fg(Color::Green)),
                                Span::raw(format!("{} [{}]", pkg.name, pkg.manager)),
                            ]);
                            ListItem::new(line)
                        } else {
                            let line = Line::from(vec![
                                Span::raw("[ ] "),
                                Span::raw(format!("{} [{}]", pkg.name, pkg.manager)),
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
            
            let highlight_color = app.config.parse_color(&app.config.text_colours.results_unit_highlight_text);
            let focused_border_color = app.config.parse_color(&app.config.border_colours.focused_border);
            let results_border_color = if !app.focus_installed {
                focused_border_color
            } else {
                app.config.parse_color(&app.config.border_colours.results_unit)
            };
            let text_color = app.config.parse_color(&app.config.text_colours.results_unit_text);
            
            let title_color = app.config.parse_color(&app.config.text_colours.unit_title_text);
            
            let results_list = List::new(visible_results)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(border_type)
                        .border_style(Style::default().fg(results_border_color))
                        .title(Span::styled(" Results ", Style::default().fg(title_color))),
                )
                .style(Style::default().fg(text_color))
                .highlight_style(Style::default().fg(highlight_color).add_modifier(Modifier::BOLD))
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
            let input_border_color = app.config.parse_color(&app.config.border_colours.results_unit);
            
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
            let description_border_color = app.config.parse_color(&app.config.border_colours.description_unit);
            let description_highlight_color = app.config.parse_color(&app.config.text_colours.description_unit_highlight_text);
            
            let description_text = {
                if let Some(idx) = app.selected_index {
                    if idx < app.shown_indices.len() {
                        let pkg_idx = app.shown_indices[idx];
                        let pkg = &app.all_packages[pkg_idx];
                        vec![
                            Line::from(Span::styled(&pkg.name, Style::default().fg(description_highlight_color))),
                            Line::from(Span::raw(&pkg.description)),
                        ]
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            };
            
            let description_text_color = app.config.parse_color(&app.config.text_colours.description_unit_text);
            let description_paragraph = Paragraph::new(description_text)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(border_type)
                        .border_style(Style::default().fg(description_border_color))
                        .title(Span::styled(" Description ", Style::default().fg(title_color))),
                )
                .style(Style::default().fg(description_text_color))
                .wrap(Wrap { trim: false });
            
            f.render_widget(description_paragraph, left_chunks[2]);
            
            // Right column: Installed packages
            let installed_height = main_chunks[1].height.saturating_sub(2) as usize;
            let visible_installed: Vec<ListItem> = app
                .installed_packages
                .iter()
                .skip(app.installed_scroll_offset)
                .take(installed_height)
                .map(|pkg| {
                    let version_str = pkg.version.as_ref().map(|v| format!(" ({})", v)).unwrap_or_default();
                    let text = format!("{}{} [{}]", pkg.name, version_str, pkg.manager);
                    ListItem::new(text)
                })
                .collect();
            
            let installed_border_color = if app.focus_installed {
                focused_border_color
            } else {
                app.config.parse_color(&app.config.border_colours.installed_list_unit)
            };
            let installed_text_color = app.config.parse_color(&app.config.text_colours.installed_list_unit_text);
            
            let installed_list = List::new(visible_installed)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(border_type)
                        .border_style(Style::default().fg(installed_border_color))
                        .title(Span::styled(" Installed ", Style::default().fg(title_color))),
                )
                .style(Style::default().fg(installed_text_color));
            
            f.render_widget(installed_list, main_chunks[1]);
        })?;
        
        if let Some(start) = draw_start {
            log_timing("First draw", start.elapsed());
            first_draw = false;
        }
        
        match input.next()? {
            Event::Key(key) => {
                match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('q') => return Ok(None),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(None),
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
                    // Vim-style navigation
                    KeyCode::Up | KeyCode::Char('k') => {
                        if app.focus_installed {
                            // Scroll installed list
                            app.installed_scroll_offset = app.installed_scroll_offset.saturating_sub(1);
                        } else {
                            app.move_selection(-1);
                            if let Some(sel) = app.selected_index {
                                if sel < app.scroll_offset {
                                    app.scroll_offset = sel;
                                }
                            }
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if app.focus_installed {
                            // Scroll installed list
                            let visible_height = terminal.size()?.height.saturating_sub(2) as usize;
                            let max_scroll = app.installed_packages.len().saturating_sub(visible_height);
                            app.installed_scroll_offset = (app.installed_scroll_offset + 1).min(max_scroll);
                        } else {
                            app.move_selection(1);
                            if let Some(sel) = app.selected_index {
                                let visible_height = terminal.size()?.height.saturating_sub(10) as usize;
                                if sel >= app.scroll_offset + visible_height {
                                    app.scroll_offset = sel.saturating_sub(visible_height - 1);
                                }
                            }
                        }
                    }
                    // Tab to switch focus between results and installed
                    KeyCode::Tab => {
                        app.focus_installed = !app.focus_installed;
                    }
                    // Fast scroll
                    KeyCode::PageUp | KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        let visible_height = terminal.size()?.height.saturating_sub(10) as usize;
                        app.move_selection(-(visible_height as isize / 2));
                        if let Some(sel) = app.selected_index {
                            app.scroll_offset = sel.saturating_sub(visible_height / 2);
                        }
                    }
                    KeyCode::PageDown | KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        let visible_height = terminal.size()?.height.saturating_sub(10) as usize;
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
                            let visible_height = terminal.size()?.height.saturating_sub(10) as usize;
                            app.scroll_offset = app.shown_indices.len().saturating_sub(visible_height);
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
                
                // Calculate panel boundaries
                let right_width = app.config.layout.right_column_width_percent;
                let left_width = 100 - right_width;
                let left_column_width = (term_size.width as f32 * left_width as f32 / 100.0) as u16;
                
                // Results panel is in top left
                let results_start = 1; // After border
                let input_height = app.config.layout.input_field_height;
                let results_end = term_size.height.saturating_sub(input_height + 5 + 1); // Before input panel
                
                // Installed panel is on the right
                let installed_start = 1;
                let installed_end = term_size.height.saturating_sub(1);
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
                            // In installed panel
                            app.focus_installed = true;
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
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        if mouse.column < installed_column_start {
                            // Scroll results
                            if !app.shown_indices.is_empty() && app.scroll_offset > 0 {
                                app.scroll_offset = app.scroll_offset.saturating_sub(1);
                                // Update selection to stay visible
                                if let Some(sel) = app.selected_index {
                                    if sel >= app.scroll_offset + (results_end - results_start) as usize {
                                        app.selected_index = Some(app.scroll_offset);
                                    }
                                }
                            }
                        } else {
                            // Scroll installed
                            app.installed_scroll_offset = app.installed_scroll_offset.saturating_sub(1);
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        if mouse.column < installed_column_start {
                            // Scroll results
                            if !app.shown_indices.is_empty() {
                                let visible_height = (results_end - results_start) as usize;
                                let max_scroll = app.shown_indices.len().saturating_sub(visible_height);
                                app.scroll_offset = (app.scroll_offset + 1).min(max_scroll);
                                // Update selection to stay visible
                                if let Some(sel) = app.selected_index {
                                    if sel < app.scroll_offset {
                                        app.selected_index = Some(app.scroll_offset);
                                    }
                                }
                            }
                        } else {
                            // Scroll installed
                            let visible_height = (installed_end - installed_start) as usize;
                            let max_scroll = app.installed_packages.len().saturating_sub(visible_height);
                            app.installed_scroll_offset = (app.installed_scroll_offset + 1).min(max_scroll);
                        }
                    }
                    _ => {}
                }
            }
            Event::Tick => {}
        }
    }
}
