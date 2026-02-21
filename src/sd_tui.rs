// Software Discovery TUI - Focused package exploration mode
use crate::cache::CacheManager;
use crate::config::Config;
use crate::input::{Event, Input};
use crate::pm::{detect_available_managers, Package, PackageManager};
use ahash::AHashSet;
use crossterm::event::{KeyCode, KeyModifiers};
use eyre::Result;
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use ratatui::{
    backend::Backend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph, Wrap},
    Terminal,
};

enum Focus {
    Results,
    PmSelector,
}

pub struct SdApp {
    query: String,
    all_packages: Vec<Package>,
    shown_indices: Vec<usize>, // Store indices instead of cloning packages
    selected_index: usize,
    pm_selector_index: usize,
    pm_options: Vec<String>,
    focus: Focus,
    config: Config,
    matcher: SkimMatcherV2,
    managers: Vec<Box<dyn PackageManager>>,
    installed_set: AHashSet<String>,
    hostpm: String,
    requested_pm: Option<String>, // Store requested PM from @ syntax
}

impl SdApp {
    fn new(config: Config, initial_search: Option<String>) -> Result<Self> {
        let hostpm = config.pm.hostpm.clone();
        let managers = detect_available_managers(&hostpm, &config.pm.enabled_pm);

        if managers.is_empty() {
            return Err(eyre::eyre!("No supported package managers found"));
        }

        Ok(Self {
            query: initial_search.unwrap_or_default(),
            all_packages: Vec::new(),
            shown_indices: Vec::new(), // Use indices instead of cloning
            selected_index: 0, // Always start at 0 (top item, most accurate)
            pm_selector_index: 0,
            pm_options: Vec::new(),
            focus: Focus::Results,
            config,
            matcher: SkimMatcherV2::default(),
            managers,
            installed_set: AHashSet::new(),
            hostpm,
            requested_pm: None,
        })
    }

    fn load_packages(&mut self) -> Result<()> {
        use rayon::prelude::*;

        let cache = CacheManager::new()?;
        let redb_cache = cache.redb_cache();

        // Load packages from all enabled PMs in PARALLEL using optimized range queries
        let results: Vec<_> = self
            .managers
            .par_iter()
            .map(|manager| {
                // Direct redb range query with bincode - INSTANT!
                let packages = redb_cache.get_all_packages(manager.name()).unwrap_or_default();
                (manager.name().to_string(), packages)
            })
            .collect();
        
        // Pre-allocate total capacity
        let total_capacity: usize = results.iter().map(|(_, pkgs)| pkgs.len()).sum();
        self.all_packages.reserve(total_capacity);

        // Host PM first
        for (name, packages) in &results {
            if name == &self.hostpm {
                self.all_packages.extend(packages.clone());
            }
        }

        // Other PMs
        for (name, packages) in results {
            if name != self.hostpm {
                self.all_packages.extend(packages);
            }
        }

        // Load installed packages from ALL managers
        let installed_results: Vec<_> = self
            .managers
            .par_iter()
            .map(|manager| {
                let cache_key = format!("{}_installed", manager.name());
                // Try cache first, then fallback to direct query
                if let Ok(Some(cached)) = cache.get_installed(&cache_key) {
                    cached
                } else {
                    // Direct query if cache miss
                    manager.list_installed().unwrap_or_default()
                }
            })
            .collect();

        for installed in installed_results {
            for pkg in &installed {
                // Pre-compute all lookup keys (same as main TUI)
                let pkg_name_normalized = pkg.name.trim().to_lowercase();
                let pkg_name_original = pkg.name.trim().to_string();
                let manager_lower = pkg.manager.to_lowercase();
                
                // Store multiple formats for robust matching
                self.installed_set.insert(format!("{}:{}", pkg_name_normalized, manager_lower));
                self.installed_set.insert(format!("{}:{}", pkg_name_original, pkg.manager));
                self.installed_set.insert(format!("{}:{}", pkg_name_normalized, pkg.manager));
                self.installed_set.insert(pkg_name_normalized.clone());
                self.installed_set.insert(pkg_name_original);
            }
        }

        Ok(())
    }

    fn search(&mut self) {
        if self.query.is_empty() {
            self.shown_indices.clear();
            self.selected_index = 0;
            return;
        }

        // Handle @manager syntax
        let (actual_query, requested_pm) = if self.query.starts_with('@') {
            if let Some(idx) = self.query.find(' ') {
                let pm = self.query[1..idx].to_string();
                let q = self.query[idx+1..].to_string();
                (q, Some(pm))
            } else {
                // Just "@manager" - treat as search for everything? Or wait for space?
                // For now, treat entire string as query if no space
                (self.query.clone(), None)
            }
        } else {
            (self.query.clone(), None)
        };
        
        let requested_pm_clone = requested_pm.clone();
        self.requested_pm = requested_pm;
        let query_lower = actual_query.to_lowercase();
        
        // If query is empty after stripping @manager, clear results
        if actual_query.is_empty() {
             self.shown_indices.clear();
             self.selected_index = 0;
             return;
        }
        // OPTIMIZED: Use indices instead of cloning packages
        let mut scored: Vec<(usize, i64)> = self
            .all_packages
            .iter()
            .enumerate()
            .filter_map(|(idx, pkg)| {
                // If a manager was requested, filter by manager first
                if let Some(ref requested_pm) = requested_pm_clone {
                    if !pkg.manager.eq_ignore_ascii_case(requested_pm) {
                        return None;
                    }
                }
                
                let name = &pkg.name;
                
                // Use case-insensitive comparison WITHOUT allocating lowercase strings
                // Exact match (case-insensitive)
                if name.len() == query_lower.len() && name.eq_ignore_ascii_case(&query_lower) {
                    return Some((idx, 1_000_000));
                }

                // Starts with (case-insensitive) - fast path
                if name.len() >= query_lower.len() && name[..query_lower.len()].eq_ignore_ascii_case(&query_lower) {
                    return Some((idx, 900_000));
                }

                // Contains (case-insensitive) - only allocate if needed
                if query_lower.len() >= 3 {
                    let name_lower = name.to_ascii_lowercase();
                    if name_lower.contains(&query_lower) {
                        return Some((idx, 800_000));
                    }
                }

                // Fuzzy match for longer queries
                if query_lower.len() >= 4 {
                    if let Some(score) = self.matcher.fuzzy_match(name, &actual_query) {
                        if score > 0 {
                            return Some((idx, score.max(100)));
                        }
                    }
                }

                None
            })
            .collect();

        // Sort by score, then by host PM priority
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

        // Deduplicate by name (keep first of each - highest score)
        let mut seen_names = AHashSet::new();
        self.shown_indices = scored
            .into_iter()
            .filter_map(|(idx, _)| {
                let pkg_name = &self.all_packages[idx].name;
                if seen_names.insert(pkg_name.clone()) {
                    Some(idx)
                } else {
                    None
                }
            })
            .take(100)
            .collect();

        // Always start at index 0 (most accurate match at top)
        self.selected_index = 0;
        self.update_pm_options();
    }

    fn update_pm_options(&mut self) {
        self.pm_options.clear();
        self.pm_selector_index = 0;

        if self.shown_indices.is_empty() {
            return;
        }

        // Ensure selected_index is valid
        if self.selected_index >= self.shown_indices.len() {
            self.selected_index = 0;
        }

        let selected_idx = self.shown_indices[self.selected_index];
        let selected_pkg = &self.all_packages[selected_idx];

        // Find all PMs that have this package
        for pkg in &self.all_packages {
            if pkg.name == selected_pkg.name && !self.pm_options.contains(&pkg.manager) {
                self.pm_options.push(pkg.manager.clone());
            }
        }

        // Sort with host PM first
        self.pm_options.sort_by(|a, b| {
            let a_is_host = a == &self.hostpm;
            let b_is_host = b == &self.hostpm;
            b_is_host.cmp(&a_is_host)
        });
        
        // Auto-select requested PM if available
        if let Some(req) = &self.requested_pm {
            if let Some(idx) = self.pm_options.iter().position(|pm| pm.eq_ignore_ascii_case(req)) {
                self.pm_selector_index = idx;
            }
        }
    }

    fn get_selected_package(&self) -> Option<&Package> {
        if self.shown_indices.is_empty() || self.pm_options.is_empty() {
            return None;
        }

        let selected_idx = self.shown_indices[self.selected_index];
        let pkg_name = &self.all_packages[selected_idx].name;
        let pm = &self.pm_options[self.pm_selector_index];

        // Find the package from the selected PM
        self.all_packages
            .iter()
            .find(|p| p.name == *pkg_name && p.manager == *pm)
    }

    fn is_package_installed(&self, pkg: &Package) -> bool {
        // OPTIMIZED: Use same lookup logic as main TUI - pre-computed keys
        // IMPORTANT: Only matches packages with the SAME manager to avoid cross-PM false positives
        let pkg_name_normalized = pkg.name.trim().to_lowercase();
        let pkg_name_original = pkg.name.trim();
        let manager_lower = pkg.manager.to_lowercase();
        
        // Try most common format first (normalized:normalized) - O(1) lookup
        if self.installed_set.contains(&format!("{}:{}", pkg_name_normalized, manager_lower)) {
            return true;
        }
        // Try normalized name with original manager - O(1) lookup
        if self.installed_set.contains(&format!("{}:{}", pkg_name_normalized, pkg.manager)) {
            return true;
        }
        // Try original case formats (less common, but needed for exact matches)
        if self.installed_set.contains(&format!("{}:{}", pkg_name_original, pkg.manager)) {
            return true;
        }
        
        // DO NOT check name-only matches - they cause cross-PM false positives
        // (e.g., hyprland from dnf would match hyprland from nix)
        
        false
    }

    fn get_install_command(&self) -> Option<String> {
        let pkg = self.get_selected_package()?;
        let manager = self.managers.iter().find(|m| m.name() == pkg.manager)?;

        let mut cmd = manager.install_command(&[pkg]);
        if cmd.starts_with("sudo ") {
            cmd = cmd.replacen("sudo", &self.config.main.sudoers, 1);
        }

        Some(cmd)
    }
}

pub enum SdResult {
    Install(String),
    TransitionToTui(String), // Package name to search in regular TUI
    Quit,
}

/// Load packages BEFORE terminal setup for instant startup
pub fn load_packages_before_sd(
    config: &Config,
    initial_search: Option<String>,
) -> Result<SdApp> {
    let mut app = SdApp::new(config.clone(), initial_search)?;
    app.load_packages()?;
    Ok(app)
}

pub fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    _opts: crate::cli::SdOpts,
    _config: Config,
    mut app: SdApp,
) -> Result<SdResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    // If query provided, search immediately
    if !app.query.is_empty() {
        app.search();
    }

    let input = Input::new();

    loop {
        terminal.draw(|f| {
            let size = f.area();

            let results_height = app.config.sd.results_height;
            let center_height = size.height.saturating_sub(results_height + 2 + 7);

            // Main layout: top results, center details, bottom PM selector
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(results_height + 2),
                    Constraint::Length(center_height),
                    Constraint::Length(7),
                ])
                .split(size);

            let border_type = if app.config.main.rounded_borders {
                BorderType::Rounded
            } else {
                BorderType::Plain
            };

            let focused_color = app.config.parse_color(&app.config.sd.focused_border);
            let text_color = app.config.parse_color(&app.config.sd.text_color);
            let highlight_color = app.config.parse_color(&app.config.sd.highlight_color);
            let title_color = app.config.parse_color(&app.config.sd.title_color);
            let pm_highlight_color = app.config.parse_color(&app.config.sd.pm_highlight_text);
            let bg_color = if app.config.sd.transparent_background {
                Color::Reset
            } else {
                app.config.parse_color(&app.config.sd.background_color)
            };

            // Top: Results list with center-aligned scrolling and clock gradient
            let results_border_color = match app.focus {
                Focus::Results => focused_color,
                _ => app.config.parse_color(&app.config.sd.results_border),
            };

            // Center-aligned scrolling: cursor ALWAYS stays at center visually
            // The most accurate match (index 0) should appear at the center
            let visible_count = results_height as usize;
            let total_count = app.shown_indices.len();
            let center_pos = visible_count / 2;
            
            // Ensure selected_index is valid
            if app.selected_index >= total_count && total_count > 0 {
                app.selected_index = total_count - 1;
            }
            
            // Calculate which items to show so selected item is always at center_pos
            let start_idx = if total_count == 0 {
                0
            } else {
                // Always center the selected item
                if app.selected_index < center_pos {
                    0
                } else if app.selected_index >= total_count.saturating_sub(center_pos) {
                    total_count.saturating_sub(visible_count)
                } else {
                    app.selected_index - center_pos
                }
            };
            
            let end_idx = (start_idx + visible_count).min(total_count);
            
            // Calculate gradient brightness for clock effect
            // Dark at edges, bright at center
            let get_brightness = |pos: usize, center: usize, total: usize| -> u8 {
                if total <= 1 {
                    return 255;
                }
                let distance_from_center = (pos as i32 - center as i32).abs() as usize;
                let max_distance = center.max(total - center - 1);
                if max_distance == 0 {
                    return 255;
                }
                // Brightness: 100 (dark) at edges, 255 (bright) at center
                let brightness = 100 + ((255 - 100) * (max_distance - distance_from_center)) / max_distance;
                brightness.min(255).max(100) as u8
            };

            let mut result_items: Vec<ListItem> = Vec::new();
            
            // Add empty lines at top to keep selected item at center
            // When selected_index is 0, we want it at center_pos, so we need center_pos empty lines
            let items_to_show = (end_idx - start_idx).min(visible_count);
            // Calculate how many empty lines needed to center the selected item
            let selected_visible_pos = if app.selected_index < start_idx {
                0
            } else if app.selected_index >= end_idx {
                items_to_show - 1
            } else {
                app.selected_index - start_idx
            };
            let empty_top = center_pos.saturating_sub(selected_visible_pos);
            
            for _ in 0..empty_top {
                result_items.push(ListItem::new(""));
            }
            
            // Add actual items with clock spacing
            for (display_idx, &pkg_idx) in app.shown_indices.iter().skip(start_idx).take(items_to_show).enumerate() {
                let pkg = &app.all_packages[pkg_idx];
                let actual_idx = start_idx + display_idx;
                let is_selected = actual_idx == app.selected_index;
                
                // Calculate position in visible list (including empty lines)
                let visible_pos = empty_top + display_idx;
                let brightness = get_brightness(visible_pos, center_pos, visible_count);
                let item_color = Color::Rgb(brightness, brightness, brightness);
                
                // Calculate spacing based on distance from center (clock effect)
                let distance_from_center = (visible_pos as i32 - center_pos as i32).abs() as usize;
                let max_distance = center_pos.max(visible_count - center_pos - 1);
                let spacing = if is_selected {
                    0 // No space for highlighted item
                } else if max_distance == 0 {
                    0
                } else {
                    // 1 space for darkest (edges), 0 for everything else
                    let ratio = distance_from_center as f32 / max_distance as f32;
                    if ratio >= 0.8 {
                        1 // Darkest items at edges - only 1 space
                    } else {
                        0 // Everything else - no spacing
                    }
                };
                
                let spacing_str = " ".repeat(spacing);
                let prefix = if is_selected { "> " } else { "  " };
                let text = format!("{}{}{} [{}]", spacing_str, prefix, pkg.name, pkg.manager);
                
                if is_selected {
                    // Selected item is brightest
                    result_items.push(
                        ListItem::new(text)
                            .style(Style::default().fg(highlight_color).add_modifier(Modifier::BOLD))
                    );
                } else {
                    // Gradient based on distance from center
                    result_items.push(
                        ListItem::new(text)
                            .style(Style::default().fg(item_color))
                    );
                }
            }
            
            // Add empty lines at bottom if needed
            let remaining = visible_count.saturating_sub(result_items.len());
            for _ in 0..remaining {
                result_items.push(ListItem::new(""));
            }

            let results_list = List::new(result_items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(border_type)
                        .border_style(Style::default().fg(results_border_color))
                        .title(Span::styled(
                            format!(" Found ({}) ", app.shown_indices.len()),
                            Style::default().fg(title_color),
                        )),
                )
                .style(Style::default().fg(text_color));

            f.render_widget(results_list, chunks[0]);

            // Center: Package details (NO BORDERS)
            let installed_color = app.config.parse_color(&app.config.sd.installed_color);
            let not_installed_color = app.config.parse_color(&app.config.sd.not_installed_color);
            
            let details_text = if let Some(pkg) = app.get_selected_package() {
                let is_installed = app.is_package_installed(pkg);

                let version_available = pkg
                    .version
                    .as_ref()
                    .map(|v| v.as_str())
                    .unwrap_or("unknown");
                
                let installed_text = if is_installed {
                    Span::styled(
                        "[ Installed ]",
                        Style::default().fg(installed_color)
                    )
                } else {
                    Span::styled(
                        "[ Not Installed ]",
                        Style::default().fg(not_installed_color)
                    )
                };

                let size_str = if let Some(size) = pkg.size {
                    format!("{} KiB", size / 1024)
                } else {
                    "N/A".to_string()
                };

                vec![
                    Line::from(vec![
                        Span::styled("*  ", Style::default().fg(highlight_color)),
                        Span::styled(
                            format!("({})/", pkg.manager),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(&pkg.name, Style::default().fg(highlight_color).add_modifier(Modifier::BOLD)),
                    ]),
                    Line::from(""),
                    Line::from(Span::raw(format!("      Latest version available: {}", version_available))),
                    Line::from(""),
                    Line::from(Span::raw(format!(
                        "      Description:   {}",
                        if pkg.description.is_empty() {
                            "No description available"
                        } else {
                            &pkg.description
                        }
                    ))),
                    Line::from(""),
                    Line::from(vec![
                        Span::raw("      Installed: "),
                        installed_text,
                    ]),
                    Line::from(""),
                    Line::from(Span::raw(format!("      Size of files: {}", size_str))),
                    Line::from(""),
                    Line::from(Span::raw(format!(
                        "      Homepage:      {}",
                        if pkg.homepage.is_empty() { "N/A" } else { &pkg.homepage }
                    ))),
                    Line::from(""),
                    Line::from(Span::raw(format!(
                        "      License:       {}",
                        if pkg.license.is_empty() { "N/A" } else { &pkg.license }
                    ))),
                ]
            } else if app.query.is_empty() {
                vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "Enter a package name to search",
                        Style::default().fg(Color::Gray).add_modifier(Modifier::ITALIC),
                    )),
                    Line::from(""),
                    Line::from(Span::raw("Type to begin searching...")),
                ]
            } else {
                vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        format!("No packages found for '{}'", app.query),
                        Style::default().fg(Color::Red),
                    )),
                ]
            };

            // Details MUST NOT HAVE ANY BORDERS, but has background (or transparent)
            let details_block = Paragraph::new(details_text)
                .style(Style::default().fg(text_color).bg(bg_color))
                .alignment(Alignment::Left)
                .wrap(Wrap { trim: false });

            f.render_widget(details_block, chunks[1]);

            // Bottom: PM selector with highlighted typed word
            let pm_selector_border_color = match app.focus {
                Focus::PmSelector => focused_color,
                _ => app.config.parse_color(&app.config.sd.pm_selector_border),
            };

            let pm_items: Vec<ListItem> = app
                .pm_options
                .iter()
                .enumerate()
                .map(|(idx, pm)| {
                    let is_selected = idx == app.pm_selector_index;
                    let prefix = if is_selected { "> " } else { "  " };
                    let text = format!("{}{}", prefix, pm);
                    
                    if is_selected {
                        ListItem::new(text).style(Style::default().fg(highlight_color).add_modifier(Modifier::BOLD))
                    } else {
                        ListItem::new(text)
                    }
                })
                .collect();

            // Build title with highlighted query - Block::title accepts Line which can have multiple Spans
            let pm_title = if app.query.is_empty() {
                Line::from(Span::styled(
                    format!(" Package Manager ({}) ", app.pm_options.len()),
                    Style::default().fg(title_color),
                ))
            } else {
                // Build title with highlighted query using Line with multiple Spans
                let base = format!(" Package Manager ({}) - {} >> ", app.pm_options.len(), app.selected_index + 1);
                let mut spans = vec![Span::raw(base)];
                // Highlight the query
                spans.push(Span::styled(
                    app.query.clone(),
                    Style::default().fg(pm_highlight_color).add_modifier(Modifier::BOLD)
                ));
                spans.push(Span::raw(" "));
                Line::from(spans)
            };

            let pm_list = List::new(pm_items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(border_type)
                        .border_style(Style::default().fg(pm_selector_border_color))
                        .title(pm_title),
                )
                .style(Style::default().fg(text_color));

            f.render_widget(pm_list, chunks[2]);
        })?;

        match input.next()? {
            Event::Key(key) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    // Break out of loop immediately - terminal restoration happens in main.rs
                    break;
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    // Break out of loop immediately - terminal restoration happens in main.rs
                    break;
                }
                KeyCode::Tab => {
                    // Switch focus
                    app.focus = match app.focus {
                        Focus::Results => Focus::PmSelector,
                        Focus::PmSelector => Focus::Results,
                    };
                }
                KeyCode::Up => match app.focus {
                    Focus::Results => {
                        if app.selected_index > 0 {
                            app.selected_index -= 1;
                            app.update_pm_options();
                        }
                    }
                    Focus::PmSelector => {
                        if app.pm_selector_index > 0 {
                            app.pm_selector_index -= 1;
                        }
                    }
                },
                KeyCode::Down => match app.focus {
                    Focus::Results => {
                        if app.selected_index < app.shown_indices.len().saturating_sub(1) {
                            app.selected_index += 1;
                            app.update_pm_options();
                        }
                    }
                    Focus::PmSelector => {
                        if app.pm_selector_index < app.pm_options.len().saturating_sub(1) {
                            app.pm_selector_index += 1;
                        }
                    }
                },
                KeyCode::Enter => {
                    // Install from ANYWHERE - Enter works from any focus
                    if let Some(cmd) = app.get_install_command() {
                        // Break out of loop with install command
                        return Ok(SdResult::Install(cmd));
                    } else {
                        // If no package selected, quit
                        break;
                    }
                }
                KeyCode::Char(' ') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    // Animate transition to regular TUI mode
                    if let Some(pkg) = app.get_selected_package() {
                        // Break out of loop with transition command
                        return Ok(SdResult::TransitionToTui(pkg.name.clone()));
                    }
                }
                KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::ALT) => {
                    // Alt+R: Clear search and start fresh
                    app.query.clear();
                    app.search();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) 
                    && !key.modifiers.contains(KeyModifiers::ALT) => {
                    app.query.push(c);
                    app.search();
                }
                KeyCode::Backspace => {
                    app.query.pop();
                    app.search();
                }
                _ => {}
            },
            Event::Tick => {}
            Event::Resize(_width, _height) => {
                // Terminal was resized - redraw will happen automatically on next loop
            }
            Event::Mouse(mouse) => {
                use crossterm::event::{MouseButton, MouseEventKind};
                
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        match app.focus {
                            Focus::Results => {
                                if app.selected_index > 0 {
                                    app.selected_index -= 1;
                                    app.update_pm_options();
                                }
                            }
                            Focus::PmSelector => {
                                if app.pm_selector_index > 0 {
                                    app.pm_selector_index -= 1;
                                }
                            }
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        match app.focus {
                            Focus::Results => {
                                if app.selected_index < app.shown_indices.len().saturating_sub(1) {
                                    app.selected_index += 1;
                                    app.update_pm_options();
                                }
                            }
                            Focus::PmSelector => {
                                if app.pm_selector_index < app.pm_options.len().saturating_sub(1) {
                                    app.pm_selector_index += 1;
                                }
                            }
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        // Click to switch focus or install
                        app.focus = match app.focus {
                            Focus::Results => Focus::PmSelector,
                            Focus::PmSelector => Focus::Results,
                        };
                    }
                    _ => {}
                }
            }
        }
    }
    
    // If we broke out of loop (Esc/q/Ctrl+C), return Quit
    Ok(SdResult::Quit)
}
