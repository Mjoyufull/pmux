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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PmConfig {
    #[serde(default = "default_hostpm")]
    pub hostpm: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_pm: Option<Vec<String>>,
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
    "pacman".to_string()
}
fn default_cyan() -> String {
    "#00ffff".to_string()
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

impl Default for Config {
    fn default() -> Self {
        Self {
            main: MainConfig::default(),
            layout: LayoutConfig::default(),
            border_colours: BorderColorConfig::default(),
            text_colours: TextColorConfig::default(),
            pm: PmConfig::default(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;
        
        if config_path.exists() {
            let content = fs::read_to_string(&config_path)?;
            match toml::from_str::<Config>(&content) {
                Ok(config) => Ok(config),
                Err(e) => {
                    eprintln!("Warning: Failed to parse config file: {}", e);
                    eprintln!("Using default configuration. Please check {}", config_path.display());
                    Ok(Config::default())
                }
            }
        } else {
            // Create default config
            let config = Config::default();
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
        let config_dir = dirs::config_dir()
            .ok_or_else(|| eyre::eyre!("Could not find config directory"))?;
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
