mod pacman;
mod paru;
mod dnf;
mod nix;
mod emerge;

use eyre::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub version: Option<String>,
    pub description: String,
    pub repo: String,
    pub manager: String,
    pub installed: bool,
}

pub trait PackageManager: Send + Sync {
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    fn list_all(&self) -> Result<Vec<Package>>;
    fn list_installed(&self) -> Result<Vec<Package>>;
    #[allow(dead_code)]
    fn search(&self, query: &str) -> Result<Vec<Package>>;
    fn install_command(&self, packages: &[&Package]) -> String;
    #[allow(dead_code)]
    fn needs_sudo(&self) -> bool;
}

pub fn detect_available_managers(hostpm: &str, enabled_pm: &Option<Vec<String>>) -> Vec<Box<dyn PackageManager>> {
    let mut managers: Vec<Box<dyn PackageManager>> = vec![];
    
    // Logic:
    // - None = only host PM (default, most users)
    // - Some([]) = all available PMs (power users)
    // - Some([list]) = only those PMs + host PM
    let should_enable = |pm_name: &str| -> bool {
        match enabled_pm {
            None => {
                // Default: only host PM
                pm_name == hostpm
            }
            Some(list) if list.is_empty() => {
                // Empty list = enable all
                true
            }
            Some(list) => {
                // Explicit list: check if in list OR is host PM
                pm_name == hostpm || list.iter().any(|e| e.to_lowercase() == pm_name.to_lowercase())
            }
        }
    };
    
    // Try to add host PM first
    if should_enable(hostpm) {
        match hostpm {
            "pacman" => {
                let pm = pacman::Pacman::new();
                if pm.is_available() {
                    managers.push(Box::new(pm));
                }
            }
            "aur" | "paru" => {
                let pm = paru::Paru::new();
                if pm.is_available() {
                    managers.push(Box::new(pm));
                }
            }
            "dnf" => {
                let pm = dnf::Dnf::new();
                if pm.is_available() {
                    managers.push(Box::new(pm));
                }
            }
            "nix" => {
                let pm = nix::Nix::new();
                if pm.is_available() {
                    managers.push(Box::new(pm));
                }
            }
            "emerge" => {
                let pm = emerge::Emerge::new();
                if pm.is_available() {
                    managers.push(Box::new(pm));
                }
            }
            _ => {}
        }
    }
    
    // Add other enabled PMs (skip if already added as host)
    if should_enable("pacman") && hostpm != "pacman" {
        let pm = pacman::Pacman::new();
        if pm.is_available() {
            managers.push(Box::new(pm));
        }
    }
    if should_enable("paru") && hostpm != "aur" && hostpm != "paru" {
        let pm = paru::Paru::new();
        if pm.is_available() {
            managers.push(Box::new(pm));
        }
    }
    if should_enable("dnf") && hostpm != "dnf" {
        let pm = dnf::Dnf::new();
        if pm.is_available() {
            managers.push(Box::new(pm));
        }
    }
    if should_enable("nix") && hostpm != "nix" {
        let pm = nix::Nix::new();
        if pm.is_available() {
            managers.push(Box::new(pm));
        }
    }
    if should_enable("emerge") && hostpm != "emerge" {
        let pm = emerge::Emerge::new();
        if pm.is_available() {
            managers.push(Box::new(pm));
        }
    }
    
    managers
}
