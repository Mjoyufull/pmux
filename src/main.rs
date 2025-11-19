mod cache;
mod cli;
mod config;
mod input;
mod logger;
mod pm;
mod redb_cache;
mod sd_tui;
mod sync;
mod tui;

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::process;

fn main() {
    if let Err(error) = real_main() {
        eprintln!("{error:?}");
        process::exit(1);
    }
}

fn setup_terminal() -> eyre::Result<()> {
    enable_raw_mode()?;
    io::stderr().execute(EnterAlternateScreen)?;
    io::stderr().execute(EnableMouseCapture)?;
    Ok(())
}

fn shutdown_terminal() {
    use std::io::Write;
    
    // CRITICAL ORDER: 
    // 1. Disable mouse capture first
    // 2. Leave alternate screen (this restores normal screen)
    // 3. Flush IMMEDIATELY to ensure escape codes are sent
    // 4. Small delay to let escape codes process
    // 5. THEN disable raw mode (this must be LAST)
    
    let _ = io::stderr().execute(DisableMouseCapture);
    let _ = io::stderr().execute(LeaveAlternateScreen);
    
    // CRITICAL: Flush stderr IMMEDIATELY after leaving alternate screen
    // This ensures the escape codes are sent before we disable raw mode
    let _ = io::stderr().flush();
    let _ = io::stdout().flush();
    
    // Small delay to let escape codes process
    std::thread::sleep(std::time::Duration::from_millis(50));
    
    // NOW disable raw mode - terminal is already restored to normal screen
    let _ = disable_raw_mode();
    
    // Final flush to ensure everything is sent
    let _ = io::stderr().flush();
    let _ = io::stdout().flush();
}

fn real_main() -> eyre::Result<()> {
    let command = cli::parse_args()?;
    let config = config::Config::load()?;

    match command {
        cli::Command::Sync(opts) => {
            // Handle -Sy, -Syu, etc.
            handle_sync(opts, &config)?;
        }
        cli::Command::Remove(opts) => {
            // Handle -R, -Rd
            handle_remove(opts, &config)?;
        }
        cli::Command::Query(opts) => {
            // Handle -Q, -Qi, etc.
            handle_query(opts, &config)?;
        }
        cli::Command::SoftwareDiscovery(opts) => {
            // CRITICAL: Load packages BEFORE terminal setup for instant startup
            let sd_config = config.sd.with_main_config(&config.main, &config.text_colours, &config.border_colours);
            let mut config_with_sd = config.clone();
            config_with_sd.sd = sd_config;
            let app = sd_tui::load_packages_before_sd(&config_with_sd, opts.search_string.clone())?;
            
            setup_terminal()?;
            let backend = CrosstermBackend::new(io::stderr());
            let mut terminal = Terminal::new(backend)?;
            terminal.hide_cursor()?;
            terminal.clear()?;

            // Run Software Discovery TUI (packages already loaded!)
            match sd_tui::run(&mut terminal, opts, config_with_sd, app)? {
                sd_tui::SdResult::Install(cmd) => {
                    // Clean shutdown - restore terminal BEFORE running command
                    drop(terminal);
                    shutdown_terminal();
                    // Execute install command
                    execute_command(&cmd)?;
                }
                sd_tui::SdResult::TransitionToTui(pkg_name) => {
                    // Animate transition to regular TUI
                    // For now, just switch modes
                    drop(terminal);
                    shutdown_terminal();
                    
                    // Re-open in regular TUI mode with package preselected
                    setup_terminal()?;
                    let backend = CrosstermBackend::new(io::stderr());
                    let mut terminal = Terminal::new(backend)?;
                    terminal.hide_cursor()?;
                    terminal.clear()?;
                    
                    let tui_opts = cli::TuiOpts {
                        search_string: Some(pkg_name.clone()),
                        filter_managers: Vec::new(),
                    };
                    
                    // Load packages before TUI for instant startup
                    let app = tui::load_packages_before_tui(tui_opts.filter_managers.clone(), &config)?;
                    let install_cmd = tui::run(&mut terminal, tui_opts, config.clone(), app)?;
                    drop(terminal);
                    shutdown_terminal();
                    
                    if let Some(cmd) = install_cmd {
                        execute_command(&cmd)?;
                    }
                }
                sd_tui::SdResult::Quit => {
                    drop(terminal);
                    shutdown_terminal();
                }
            }
        }
        cli::Command::Tui(opts) => {
            // CRITICAL: Load packages BEFORE terminal setup for instant startup
            // This prevents the TUI from showing empty before packages load
            let app = tui::load_packages_before_tui(opts.filter_managers.clone(), &config)?;
            
            setup_terminal()?;
            let backend = CrosstermBackend::new(io::stderr());
            let mut terminal = Terminal::new(backend)?;
            terminal.hide_cursor()?;
            terminal.clear()?;

            // Run TUI and get install command if any (packages already loaded!)
            let install_cmd = tui::run(&mut terminal, opts, config.clone(), app)?;

            // Clean shutdown - restore terminal BEFORE running command
            drop(terminal);
            shutdown_terminal();

            // Execute install command if we got one
            if let Some(cmd) = install_cmd {
                execute_command(&cmd)?;
            }
        }
    }

    Ok(())
}

fn handle_sync(opts: cli::SyncOpts, config: &config::Config) -> eyre::Result<()> {
    if opts.refresh {
        let syncer = sync::RepoSync::new()?;
        // Get enabled PMs (same logic as TUI)
        use pm::detect_available_managers;
        let managers = detect_available_managers(&config.pm.hostpm, &config.pm.enabled_pm);
        let enabled_pm_names: Vec<String> = managers.iter().map(|m| m.name().to_string()).collect();
        syncer.sync_all(opts.force_refresh, &enabled_pm_names)?;
        
        // Packages are now synced directly to redb - no secondary cache build needed!
    }

    if opts.upgrade {
        println!(":: Starting full system upgrade...");
        println!(":: Checking for updates...");

        // Use host PM for upgrades
        let hostpm = &config.pm.hostpm;
        println!(":: Using {} as host package manager", hostpm);

        match hostpm.as_str() {
            "pacman" => {
                println!(":: Running: sudo pacman -Syu");
                std::process::Command::new("sudo")
                    .args(&["pacman", "-Syu"])
                    .status()?;
            }
            "dnf" => {
                println!(":: Running: sudo dnf upgrade");
                std::process::Command::new("sudo")
                    .args(&["dnf", "upgrade"])
                    .status()?;
            }
            "emerge" => {
                println!(":: Running: sudo emerge --update --deep --newuse @world");
                std::process::Command::new("sudo")
                    .args(&["emerge", "--update", "--deep", "--newuse", "@world"])
                    .status()?;
            }
            _ => {
                println!(":: Upgrade not implemented for {}", hostpm);
            }
        }
    }

    if !opts.packages.is_empty() {
        println!(":: Resolving packages...");

        // Load all enabled managers
        use pm::detect_available_managers;
        let managers = detect_available_managers(&config.pm.hostpm, &config.pm.enabled_pm);
        
        // Define priority order: HostPM -> Emerge -> Nix -> Paru -> Pacman -> Dnf
        // Note: HostPM is handled dynamically
        let priority_order = ["emerge", "nix", "aur", "pacman", "dnf"];
        
        // Group packages by manager
        let mut packages_by_pm: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        let mut not_found = Vec::new();

        // Load cache once
        let cache = cache::CacheManager::new()?;
        let redb_cache = cache.redb_cache();

        for pkg_name in &opts.packages {
            let mut found_manager = None;

            // 1. Check HostPM first
            if let Some(manager) = managers.iter().find(|m| m.name() == config.pm.hostpm) {
                if let Ok(Some(_)) = redb_cache.get_package(manager.name(), pkg_name) {
                    found_manager = Some(manager.name().to_string());
                }
            }

            // 2. Check others in priority order if not found
            if found_manager.is_none() {
                for pm_name in priority_order {
                    // Skip if this is the host PM (already checked)
                    if pm_name == config.pm.hostpm {
                        continue;
                    }
                    
                    // Check if this PM is enabled/available
                    if let Some(manager) = managers.iter().find(|m| m.name() == pm_name) {
                        if let Ok(Some(_)) = redb_cache.get_package(manager.name(), pkg_name) {
                            found_manager = Some(manager.name().to_string());
                            break;
                        }
                    }
                }
            }
            
            // 3. Fallback: Check any remaining managers not in priority list
            if found_manager.is_none() {
                for manager in &managers {
                    let name = manager.name();
                    if name == config.pm.hostpm || priority_order.contains(&name) {
                        continue;
                    }
                    if let Ok(Some(_)) = redb_cache.get_package(name, pkg_name) {
                        found_manager = Some(name.to_string());
                        break;
                    }
                }
            }

            if let Some(pm) = found_manager {
                packages_by_pm.entry(pm).or_default().push(pkg_name.clone());
            } else {
                not_found.push(pkg_name);
            }
        }

        if !not_found.is_empty() {
            let not_found_str = not_found.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
            eprintln!("error: target not found: {}", not_found_str);
            // Continue installing found packages? Or exit? 
            // Standard behavior is usually to fail if any package is missing
            std::process::exit(1);
        }

        // Execute installs grouped by PM
        for (pm_name, pkgs) in packages_by_pm {
            if let Some(manager) = managers.iter().find(|m| m.name() == pm_name) {
                println!(":: Installing from {}: {}", pm_name, pkgs.join(" "));
                
                // Create dummy package objects for install_command
                let pkg_objs: Vec<pm::Package> = pkgs.iter().map(|name| pm::Package {
                    name: name.clone(),
                    version: None,
                    description: String::new(),
                    repo: String::new(),
                    manager: pm_name.clone(),
                    installed: false,
                    homepage: String::new(),
                    license: String::new(),
                    size: None,
                }).collect();
                
                let pkg_refs: Vec<&pm::Package> = pkg_objs.iter().collect();
                
                let mut cmd = manager.install_command(&pkg_refs);
                if cmd.starts_with("sudo ") {
                    cmd = cmd.replacen("sudo", &config.main.sudoers, 1);
                }
                
                execute_command(&cmd)?;
            }
        }
    }

    Ok(())
}

fn handle_remove(opts: cli::RemoveOpts, config: &config::Config) -> eyre::Result<()> {
    if opts.packages.is_empty() {
        eprintln!("error: no packages specified");
        eprintln!("usage: pmux -R <package>...");
        eprintln!("       pmux -R @manager <package>...");
        eprintln!("       pmux -Rd <package>...  (force remove)");
        std::process::exit(1);
    }

    // Load all enabled managers
    use pm::detect_available_managers;
    let managers = detect_available_managers(&config.pm.hostpm, &config.pm.enabled_pm);
    
    // Group packages by manager
    let mut packages_by_pm: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();

    for pkg_arg in &opts.packages {
        if pkg_arg.starts_with('@') {
            // Handle @manager package syntax
            if let Some(idx) = pkg_arg.find(' ') {
                // This case might be split by shell, so we might get "@manager" and "package" as separate args
                // But here we assume "pmux -R @manager package" results in separate args in opts.packages
                // Wait, CLI parsing splits args. So "@nix" and "nano" are separate elements in opts.packages?
                // No, standard CLI args: pmux -R @nix nano -> packages=["@nix", "nano"]
                // This loop iterates over them. This is tricky.
                // Let's assume the user types: pmux -R @nix nano
                // We need to handle state across iterations or assume syntax like @nix:nano or just simple heuristic
                
                // Actually, let's support "@nix" as a modifier for the NEXT package? 
                // Or maybe the user meant "pmux -R @nix nano" where @nix is a modifier.
                // But opts.packages is just a Vec<String>.
                
                // SIMPLER APPROACH: Check if arg contains ':' like "nix:nano" or just use HostPM default
                // The user asked for "pmux -R @nix nano".
                // This implies we need to parse the args list manually or handle state.
                // But here we are iterating.
                
                // Let's try to parse "manager:package" syntax as well, or just handle the @ syntax if it's a single string?
                // If the shell passes "@nix" "nano", we get two args.
                // If we see "@nix", we set a "current manager" context?
                // But we can't easily change the loop structure here without refactoring.
                
                // Let's assume the user might use "pmux -R @nix:nano" or we handle the stateful parsing.
                // Let's implement a stateful parser for the args.
            }
        }
    }
    
    // Stateful parsing of packages list
    let mut current_manager = config.pm.hostpm.clone();
    
    let mut i = 0;
    while i < opts.packages.len() {
        let arg = &opts.packages[i];
        if arg.starts_with('@') {
            // It's a manager specifier
            let manager = arg[1..].to_string();
            // Check if valid manager
            if managers.iter().any(|m| m.name() == manager) {
                current_manager = manager;
            } else {
                eprintln!("warning: unknown package manager '{}', ignoring", manager);
            }
        } else {
            // It's a package
            packages_by_pm.entry(current_manager.clone()).or_default().push(arg.clone());
            // Reset to hostpm? Or keep sticky? Usually sticky is better for "pmux -R @nix pkg1 pkg2"
        }
        i += 1;
    }

    for (pm_name, pkgs) in packages_by_pm {
        println!(":: Removing from {}: {}", pm_name, pkgs.join(" "));
        
        if let Some(manager) = managers.iter().find(|m| m.name() == pm_name) {
             // Create dummy package objects
            let pkg_objs: Vec<pm::Package> = pkgs.iter().map(|name| pm::Package {
                name: name.clone(),
                version: None,
                description: String::new(),
                repo: String::new(),
                manager: pm_name.clone(),
                installed: true,
                homepage: String::new(),
                license: String::new(),
                size: None,
            }).collect();
            
            let pkg_refs: Vec<&pm::Package> = pkg_objs.iter().collect();
            
            let mut cmd = manager.remove_command(&pkg_refs);
            
            // Handle force flags if needed (manager specific)
            // Note: remove_command usually handles basic remove. 
            // We might need to inject flags manually or update trait.
            // For now, let's append flags if supported/needed, but the trait `remove_command` is simple.
            // We'll stick to the generated command and maybe append flags if the manager allows it?
            // Actually, `handle_remove` previously had a match block to add flags.
            // We should preserve that logic.
            
            // Re-implement flag logic based on manager name
            match pm_name.as_str() {
                "pacman" => {
                    if opts.force {
                        cmd = cmd.replace("-R", "-Rdd");
                    }
                }
                "dnf" => {
                    if opts.force {
                        cmd = cmd.replace("remove", "remove --nodeps");
                    }
                }
                "emerge" => {
                    // emerge -C is already force-ish, but maybe --nodeps?
                }
                _ => {}
            }

            if cmd.starts_with("sudo ") {
                cmd = cmd.replacen("sudo", &config.main.sudoers, 1);
            }
            
            execute_command(&cmd)?;
        }
    }

    Ok(())
}

fn execute_command(cmd: &str) -> eyre::Result<()> {
    // Reset terminal to sane state before executing command
    // This fixes issues with sudo password prompt and input handling
    std::process::Command::new("stty")
        .arg("sane")
        .status()?;

    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    
    Ok(())
}

fn handle_query(opts: cli::QueryOpts, config: &config::Config) -> eyre::Result<()> {
    use pm::detect_available_managers;

    let managers = detect_available_managers(&config.pm.hostpm, &config.pm.enabled_pm);

    if opts.info && !opts.packages.is_empty() {
        // Query package info - search both installed and available
        for pkg_name in &opts.packages {
            let mut found = false;

            for manager in &managers {
                // Check installed first
                if let Ok(installed) = manager.list_installed() {
                    if let Some(pkg) = installed.iter().find(|p| {
                        p.name == *pkg_name || p.name.starts_with(&format!("{}-", pkg_name))
                    }) {
                        found = true;
                        println!("*  {}", pkg.name);
                        println!(
                            "Description:   {}",
                            if pkg.description.is_empty() {
                                "No description available"
                            } else {
                                &pkg.description
                            }
                        );

                        // Get available version info
                        let mut latest_version = None;
                        if let Ok(all_pkgs) = manager.list_all() {
                            if let Some(avail) = all_pkgs.iter().find(|p| p.name == pkg.name) {
                                latest_version = avail.version.clone();
                            }
                        }

                        println!(
                            "Latest version available: {}",
                            latest_version.as_ref().unwrap_or(&"unknown".to_string())
                        );
                        println!(
                            "Latest version installed: {}",
                            pkg.version.as_ref().unwrap_or(&"unknown".to_string())
                        );
                        if let Some(size) = pkg.size {
                            println!("Size of files: {} KiB", size / 1024);
                        } else {
                            println!("Size of files: N/A");
                        }
                        println!(
                            "Homepage:      {}",
                            if pkg.homepage.is_empty() {
                                "N/A"
                            } else {
                                &pkg.homepage
                            }
                        );
                        println!(
                            "License:       {}",
                            if pkg.license.is_empty() {
                                "N/A"
                            } else {
                                &pkg.license
                            }
                        );
                        println!();
                        break;
                    }
                }

                // Check available packages if not installed
                if !found {
                    if let Ok(all_pkgs) = manager.list_all() {
                        if let Some(pkg) = all_pkgs.iter().find(|p| p.name == *pkg_name) {
                            found = true;
                            println!("*  {}", pkg.name);
                            println!(
                                "Description:   {}",
                                if pkg.description.is_empty() {
                                    "No description available"
                                } else {
                                    &pkg.description
                                }
                            );
                            println!(
                                "Latest version available: {}",
                                pkg.version.as_ref().unwrap_or(&"unknown".to_string())
                            );
                            println!("Latest version installed: [ Not Installed ]");
                            if let Some(size) = pkg.size {
                                println!("Size of files: {} KiB", size / 1024);
                            } else {
                                println!("Size of files: N/A");
                            }
                            println!(
                                "Homepage:      {}",
                                if pkg.homepage.is_empty() {
                                    "N/A"
                                } else {
                                    &pkg.homepage
                                }
                            );
                            println!(
                                "License:       {}",
                                if pkg.license.is_empty() {
                                    "N/A"
                                } else {
                                    &pkg.license
                                }
                            );
                            println!();
                            break;
                        }
                    }
                }
            }

            if !found {
                eprintln!("Package '{}' not found", pkg_name);
            }
        }
    } else {
        // List all installed packages
        println!(":: Installed packages:");

        for manager in &managers {
            if let Ok(installed) = manager.list_installed() {
                for pkg in installed {
                    let version = pkg
                        .version
                        .as_ref()
                        .map(|v| format!(" {}", v))
                        .unwrap_or_default();
                    println!("{}{} [{}]", pkg.name, version, pkg.manager);
                }
            }
        }
    }

    Ok(())
}
