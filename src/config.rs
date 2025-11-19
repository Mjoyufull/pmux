use eyre::Result;
use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub main: MainConfig,
    #[serde(default)]
    pub layout: LayoutConfig,
    #[serde(default)]
    pub border_colours: BorderColorConfig,
    #[serde(default)]
    pub text_colours: TextColorConfig,
    #[serde(default)]
    pub pm: PmConfig,
    #[serde(default)]
    pub sd: SdConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MainConfig {
    #[serde(default = "default_sudoers")]
    pub sudoers: String,
    #[serde(default = "default_rounded_borders")]
    pub rounded_borders: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutConfig {
    #[serde(default = "default_right_column_width_percent")]
    pub right_column_width_percent: u16,
    #[serde(default = "default_input_field_height")]
    pub input_field_height: u16,
    #[serde(default = "default_description_unit_height")]
    pub description_unit_height: u16,
    #[serde(default = "default_installed_list_percent")]
    pub installed_list_percent: u16,
    #[serde(default = "default_terminal_percent")]
    pub terminal_percent: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BorderColorConfig {
    #[serde(default = "default_white")]
    pub installed_list_unit: String,
    #[serde(default = "default_white")]
    pub terminal_unit: String,
    #[serde(default = "default_white")]
    pub description_unit: String,
    #[serde(default = "default_white")]
    pub results_unit: String,
    #[serde(default = "default_cyan")]
    pub focused_border: String,
    #[serde(default = "default_white")]
    pub software_discovery: String, // Border color for SD mode (can override per-section)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextColorConfig {
    #[serde(default = "default_white")]
    pub results_unit_text: String,
    #[serde(default = "default_green")]
    pub description_unit_highlight_text: String,
    #[serde(default = "default_green")]
    pub results_unit_highlight_text: String,
    #[serde(default = "default_green")]
    pub terminal_unit_highlight_text: String,
    #[serde(default = "default_green")]
    pub installed_list_unit_highlight_text: String,
    #[serde(default = "default_white")]
    pub terminal_unit_text: String,
    #[serde(default = "default_white")]
    pub installed_list_unit_text: String,
    #[serde(default = "default_white")]
    pub description_unit_text: String,
    #[serde(default = "default_white")]
    pub unit_title_text: String,
    // SD mode specific text colors
    #[serde(default = "default_white")]
    pub sd_results_text: String,
    #[serde(default = "default_green")]
    pub sd_results_highlight_text: String,
    #[serde(default = "default_white")]
    pub sd_details_text: String,
    #[serde(default = "default_green")]
    pub sd_details_highlight_text: String,
    #[serde(default = "default_white")]
    pub sd_pm_text: String,
    #[serde(default = "default_green")]
    pub sd_pm_highlight_text: String,
    #[serde(default = "default_white")]
    pub sd_title_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PmConfig {
    #[serde(default = "default_hostpm")]
    pub hostpm: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_pm: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdConfig {
    #[serde(default = "default_white")]
    pub results_border: String,
    #[serde(default = "default_white")]
    pub details_border: String,
    #[serde(default = "default_white")]
    pub pm_selector_border: String,
    #[serde(default = "default_cyan")]
    pub focused_border: String,
    #[serde(default = "default_white")]
    pub text_color: String,
    #[serde(default = "default_green")]
    pub highlight_color: String,
    #[serde(default = "default_white")]
    pub title_color: String,
    #[serde(default = "default_false")]
    pub transparent_background: bool,
    #[serde(default = "default_5")]
    pub results_height: u16,
    #[serde(default = "default_green")]
    pub installed_color: String,
    #[serde(default = "default_red")]
    pub not_installed_color: String,
    #[serde(default = "default_onedark")]
    pub background_color: String,
    #[serde(default = "default_white")]
    pub pm_highlight_text: String, // For highlighting typed word in PM section
}

// Defaults
fn default_sudoers() -> String {
    "sudo".to_string()
}
fn default_rounded_borders() -> bool {
    false
}
fn default_right_column_width_percent() -> u16 {
    30
}
fn default_input_field_height() -> u16 {
    3
}
fn default_description_unit_height() -> u16 {
    5
}
fn default_installed_list_percent() -> u16 {
    50
}
fn default_terminal_percent() -> u16 {
    50
}
fn default_white() -> String {
    "#ffffff".to_string()
}
fn default_green() -> String {
    "#00ff00".to_string()
}
fn default_hostpm() -> String {
    detect_host_pm()
}

// Auto-detect the host package manager by checking for database files
fn detect_host_pm() -> String {
    use std::path::Path;

    // Check for package manager databases in order of preference

    // Arch Linux - pacman
    if Path::new("/var/lib/pacman").exists() {
        return "pacman".to_string();
    }

    // Fedora/RHEL/CentOS - dnf/yum
    if Path::new("/var/lib/rpm").exists() || Path::new("/var/lib/dnf").exists() {
        return "dnf".to_string();
    }

    // Gentoo - emerge/portage
    if Path::new("/var/db/pkg").exists() || Path::new("/etc/portage").exists() {
        return "emerge".to_string();
    }

    // NixOS - nix
    if Path::new("/nix/store").exists() || Path::new("/nix/var/nix").exists() {
        return "nix".to_string();
    }

    // Debian/Ubuntu - apt (not supported yet, but detect for future)
    if Path::new("/var/lib/dpkg").exists() {
        // For now, default to pacman since apt isn't implemented
        return "pacman".to_string();
    }

    // Default fallback
    "pacman".to_string()
}

// Check if a package manager is actually available on the system
fn is_hostpm_available(pm: &str) -> bool {
    use std::path::Path;
    
    match pm {
        "pacman" => Path::new("/var/lib/pacman").exists(),
        "dnf" => Path::new("/var/lib/rpm").exists() || Path::new("/var/lib/dnf").exists() || Path::new("/usr/lib/sysimage/rpm").exists(),
        "emerge" => Path::new("/var/db/pkg").exists() || Path::new("/etc/portage").exists(),
        "nix" => Path::new("/nix/store").exists() || Path::new("/nix/var/nix").exists(),
        _ => false,
    }
}
fn default_cyan() -> String {
    "#00ffff".to_string()
}
fn default_false() -> bool {
    false
}
fn default_5() -> u16 {
    5
}
fn default_red() -> String {
    "#ff5555".to_string()
}
fn default_onedark() -> String {
    "#282c34".to_string()
}

impl Default for MainConfig {
    fn default() -> Self {
        Self {
            sudoers: default_sudoers(),
            rounded_borders: default_rounded_borders(),
        }
    }
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            right_column_width_percent: default_right_column_width_percent(),
            input_field_height: default_input_field_height(),
            description_unit_height: default_description_unit_height(),
            installed_list_percent: default_installed_list_percent(),
            terminal_percent: default_terminal_percent(),
        }
    }
}

impl Default for BorderColorConfig {
    fn default() -> Self {
        Self {
            installed_list_unit: default_white(),
            terminal_unit: default_white(),
            description_unit: default_white(),
            results_unit: default_white(),
            focused_border: default_cyan(),
            software_discovery: default_white(),
        }
    }
}

impl Default for TextColorConfig {
    fn default() -> Self {
        Self {
            results_unit_text: default_white(),
            description_unit_highlight_text: default_green(),
            results_unit_highlight_text: default_green(),
            terminal_unit_highlight_text: default_green(),
            installed_list_unit_highlight_text: default_green(),
            terminal_unit_text: default_white(),
            installed_list_unit_text: default_white(),
            description_unit_text: default_white(),
            unit_title_text: default_white(),
            sd_results_text: default_white(),
            sd_results_highlight_text: default_green(),
            sd_details_text: default_white(),
            sd_details_highlight_text: default_green(),
            sd_pm_text: default_white(),
            sd_pm_highlight_text: default_green(),
            sd_title_text: default_white(),
        }
    }
}

impl Default for PmConfig {
    fn default() -> Self {
        Self {
            hostpm: default_hostpm(),
            enabled_pm: None, // None = only host PM (default)
        }
    }
}

impl Default for SdConfig {
    fn default() -> Self {
        Self {
            results_border: default_white(),
            details_border: default_white(),
            pm_selector_border: default_white(),
            focused_border: default_cyan(),
            text_color: default_white(),
            highlight_color: default_green(),
            title_color: default_white(),
            transparent_background: default_false(),
            results_height: default_5(),
            installed_color: default_green(),
            not_installed_color: default_red(),
            background_color: default_onedark(),
            pm_highlight_text: default_white(),
        }
    }
}

impl SdConfig {
    // Inherit from main config if not set
    pub fn with_main_config(&self, _main: &MainConfig, text: &TextColorConfig, border: &BorderColorConfig) -> Self {
        Self {
            // Border colors: inherit from border_colours if not set
            results_border: if self.results_border == default_white() { 
                border.software_discovery.clone()
            } else { 
                self.results_border.clone() 
            },
            details_border: if self.details_border == default_white() { 
                border.software_discovery.clone() 
            } else { 
                self.details_border.clone() 
            },
            pm_selector_border: if self.pm_selector_border == default_white() { 
                border.software_discovery.clone() 
            } else { 
                self.pm_selector_border.clone() 
            },
            focused_border: if self.focused_border == default_cyan() { 
                border.focused_border.clone() 
            } else { 
                self.focused_border.clone() 
            },
            // Text colors: inherit from text_colours if not set
            text_color: if self.text_color == default_white() { 
                text.sd_details_text.clone()
            } else { 
                self.text_color.clone() 
            },
            highlight_color: if self.highlight_color == default_green() { 
                text.sd_details_highlight_text.clone() 
            } else { 
                self.highlight_color.clone() 
            },
            title_color: if self.title_color == default_white() { 
                text.sd_title_text.clone() 
            } else { 
                self.title_color.clone() 
            },
            transparent_background: self.transparent_background,
            results_height: self.results_height,
            installed_color: self.installed_color.clone(),
            not_installed_color: self.not_installed_color.clone(),
            background_color: self.background_color.clone(),
            pm_highlight_text: if self.pm_highlight_text == default_white() {
                text.sd_pm_highlight_text.clone()
            } else {
                self.pm_highlight_text.clone()
            },
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            main: MainConfig::default(),
            layout: LayoutConfig::default(),
            border_colours: BorderColorConfig::default(),
            text_colours: TextColorConfig::default(),
            pm: PmConfig::default(),
            sd: SdConfig::default(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;

        if config_path.exists() {
            let content = fs::read_to_string(&config_path)?;
            match toml::from_str::<Config>(&content) {
                Ok(config) => {
                    // Check if hostpm is valid
                    if !is_hostpm_available(&config.pm.hostpm) {
                        eprintln!("\nWarning: Configured host package manager '{}' is not available on this system.", config.pm.hostpm);
                        let detected = detect_host_pm();
                        eprintln!("Auto-detected package manager: {}", detected);
                        eprintln!("Please update your config at: {}\n", config_path.display());
                    }
                    Ok(config)
                },
                Err(e) => {
                    eprintln!("Warning: Failed to parse config file: {}", e);
                    eprintln!(
                        "Using default configuration. Please check {}",
                        config_path.display()
                    );
                    Ok(Config::default())
                }
            }
        } else {
            // Create default config - first time run
            let config = Config::default();
            let detected_pm = &config.pm.hostpm;
            
            eprintln!("\nFirst run detected!");
            eprintln!("Auto-detected host package manager: {}", detected_pm);
            eprintln!("Config will be saved to: {}", config_path.display());
            
            if !is_hostpm_available(detected_pm) {
                eprintln!("\nWarning: Could not detect a supported package manager.");
                eprintln!("Please configure your host PM in the config file.");
            }
            
            if let Err(e) = config.save() {
                eprintln!("Warning: Failed to save default config: {}", e);
            }
            Ok(config)
        }
    }

    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path()?;

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(self)?;
        fs::write(&config_path, content)?;

        Ok(())
    }

    fn config_path() -> Result<PathBuf> {
        let config_dir =
            dirs::config_dir().ok_or_else(|| eyre::eyre!("Could not find config directory"))?;
        Ok(config_dir.join("pmux").join("config.toml"))
    }

    pub fn parse_color(&self, color_str: &str) -> Color {
        let color_lower = color_str.trim().to_lowercase();

        // Named colors (case-insensitive)
        match color_lower.as_str() {
            "black" => Color::Black,
            "red" => Color::Red,
            "green" => Color::Green,
            "yellow" => Color::Yellow,
            "blue" => Color::Blue,
            "magenta" => Color::Magenta,
            "cyan" => Color::Cyan,
            "gray" | "grey" => Color::Gray,
            "darkgray" | "darkgrey" => Color::DarkGray,
            "lightred" => Color::LightRed,
            "lightgreen" => Color::LightGreen,
            "lightyellow" => Color::LightYellow,
            "lightblue" => Color::LightBlue,
            "lightmagenta" => Color::LightMagenta,
            "lightcyan" => Color::LightCyan,
            "white" => Color::White,
            _ => {
                // Try RGB hex (#RRGGBB)
                let trimmed = color_str.trim();
                if trimmed.starts_with('#') && trimmed.len() == 7 {
                    if let (Ok(r), Ok(g), Ok(b)) = (
                        u8::from_str_radix(&trimmed[1..3], 16),
                        u8::from_str_radix(&trimmed[3..5], 16),
                        u8::from_str_radix(&trimmed[5..7], 16),
                    ) {
                        return Color::Rgb(r, g, b);
                    }
                }
                Color::White
            }
        }
    }
}
