mod cli;
mod pm;
mod cache;
mod binary_cache;
mod search_index;
mod tui;
mod input;
mod config;
mod sync;
mod logger;

use std::io;
use std::process;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

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
    let _ = io::stderr().execute(DisableMouseCapture);
    let _ = io::stderr().execute(LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

fn real_main() -> eyre::Result<()> {
    // Initialize performance logger
    logger::init_logger();
    logger::log_info("pmux starting");
    
    let command = cli::parse_args()?;
    let config = config::Config::load()?;
    
    match command {
        cli::Command::Sync(opts) => {
            // Handle -Sy, -Syu, etc.
            handle_sync(opts, &config)?;
        }
        cli::Command::Query(opts) => {
            // Handle -Q, -Qi, etc.
            handle_query(opts, &config)?;
        }
        cli::Command::Tui(opts) => {
            use std::time::Instant;
            
            let setup_start = Instant::now();
            setup_terminal()?;
            logger::log_timing("Terminal setup", setup_start.elapsed());
            
            let backend_start = Instant::now();
            let backend = CrosstermBackend::new(io::stderr());
            let mut terminal = Terminal::new(backend)?;
            terminal.hide_cursor()?;
            terminal.clear()?;
            logger::log_timing("Terminal init", backend_start.elapsed());
            
            // Run TUI and get install command if any
            let tui_start = Instant::now();
            let install_cmd = tui::run(&mut terminal, opts, config.clone())?;
            logger::log_timing("TUI run", tui_start.elapsed());
            
            // Clean shutdown - restore terminal BEFORE running command
            drop(terminal);
            shutdown_terminal();
            
            // Execute install command if we got one
            if let Some(cmd) = install_cmd {
                println!("Running: {}", cmd);
                println!();
                
                // Run command in the restored terminal
                let status = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .status()?;
                
                if !status.success() {
                    std::process::exit(status.code().unwrap_or(1));
                }
            }
        }
    }
    
    Ok(())
}

fn handle_sync(opts: cli::SyncOpts, config: &config::Config) -> eyre::Result<()> {
    if opts.refresh {
        let syncer = sync::RepoSync::new()?;
        syncer.sync_all(opts.force_refresh)?;
        
        // Build binary cache after syncing repos
        println!(":: Building package cache...");
        build_package_cache(config)?;
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
        println!(":: Installing packages: {}", opts.packages.join(" "));
        
        // Use host PM for installation
        let hostpm = &config.pm.hostpm;
        let sudo_cmd = &config.main.sudoers;
        
        match hostpm.as_str() {
            "pacman" => {
                let mut cmd = std::process::Command::new(sudo_cmd);
                cmd.arg("pacman").arg("-S");
                cmd.args(&opts.packages);
                cmd.status()?;
            }
            "dnf" => {
                let mut cmd = std::process::Command::new(sudo_cmd);
                cmd.arg("dnf").arg("install");
                cmd.args(&opts.packages);
                cmd.status()?;
            }
            "emerge" => {
                let mut cmd = std::process::Command::new(sudo_cmd);
                cmd.arg("emerge");
                cmd.args(&opts.packages);
                cmd.status()?;
            }
            _ => {
                println!(":: Installation not implemented for {}", hostpm);
            }
        }
    }
    
    Ok(())
}

fn build_package_cache(config: &config::Config) -> eyre::Result<()> {
    use pm::detect_available_managers;
    use cache::CacheManager;
    use rayon::prelude::*;
    use std::time::Instant;
    
    let managers = detect_available_managers(&config.pm.hostpm, &config.pm.enabled_pm);
    let cache = CacheManager::new()?;
    
    println!("   Building cache for {} package managers...", managers.len());
    
    // Build cache in parallel - FORCE rebuild
    managers.par_iter().for_each(|manager| {
        let start = Instant::now();
        let cache_key = format!("{}_all", manager.name());
        
        println!("   -> Building {} cache...", manager.name());
        if let Ok(packages) = manager.list_all() {
            println!("      Parsed {} packages in {:?}", packages.len(), start.elapsed());
            let write_start = Instant::now();
            if let Ok(_) = cache.set(&cache_key, packages) {
                println!("      Wrote cache in {:?}", write_start.elapsed());
            }
        }
        
        // Also cache installed packages
        let installed_key = format!("{}_installed", manager.name());
        if let Ok(packages) = manager.list_installed() {
            let _ = cache.set_installed(&installed_key, packages);
        }
    });
    
    println!(":: Package cache built successfully");
    Ok(())
}

fn handle_query(opts: cli::QueryOpts, config: &config::Config) -> eyre::Result<()> {
    use pm::detect_available_managers;
    
    let managers = detect_available_managers(&config.pm.hostpm, &config.pm.enabled_pm);
    
    if opts.info && !opts.packages.is_empty() {
        // Query package info
        for pkg_name in &opts.packages {
            println!(":: Package information for {}", pkg_name);
            
            // Search in all managers
            for manager in &managers {
                if let Ok(installed) = manager.list_installed() {
                    if let Some(pkg) = installed.iter().find(|p| p.name == *pkg_name) {
                        println!("Name            : {}", pkg.name);
                        println!("Version         : {}", pkg.version.as_ref().unwrap_or(&"unknown".to_string()));
                        println!("Description     : {}", pkg.description);
                        println!("Repository      : {}", pkg.repo);
                        println!("Package Manager : {}", pkg.manager);
                        println!();
                        break;
                    }
                }
            }
        }
    } else {
        // List all installed packages
        println!(":: Installed packages:");
        
        for manager in &managers {
            if let Ok(installed) = manager.list_installed() {
                for pkg in installed {
                    let version = pkg.version.as_ref().map(|v| format!(" {}", v)).unwrap_or_default();
                    println!("{}{} [{}]", pkg.name, version, pkg.manager);
                }
            }
        }
    }
    
    Ok(())
}
