use lexopt::prelude::*;

#[derive(Debug)]
pub enum Command {
    Tui(TuiOpts),
    Sync(SyncOpts),
    Query(QueryOpts),
}

#[derive(Debug)]
pub struct TuiOpts {
    pub search_string: Option<String>,
    pub filter_managers: Vec<String>,
}

#[derive(Debug)]
pub struct SyncOpts {
    pub refresh: bool,
    pub force_refresh: bool,
    pub upgrade: bool,
    pub packages: Vec<String>,
}

#[derive(Debug)]
pub struct QueryOpts {
    pub info: bool,
    pub packages: Vec<String>,
}

pub fn parse_args() -> eyre::Result<Command> {
    let mut parser = lexopt::Parser::from_env();

    // Check for pacman-style flags
    let mut sync_mode = false;
    let mut query_mode = false;
    let mut refresh = false;
    let mut force_refresh = false;
    let mut upgrade = false;
    let mut info = false;
    let mut search_string = None;
    let mut filter_managers = Vec::new();
    let mut packages = Vec::new();

    while let Some(arg) = parser.next()? {
        match arg {
            Short('S') => sync_mode = true,
            Short('Q') => query_mode = true,
            Short('y') => {
                if refresh {
                    force_refresh = true;
                }
                refresh = true;
            }
            Short('u') => upgrade = true,
            Short('i') => info = true,
            Short('s') => {
                // -Ss for search
                if let Ok(val) = parser.value() {
                    search_string = Some(val.string()?);
                }
            }
            Short('h') | Long("help") => {
                print_help();
                std::process::exit(0);
            }
            Value(val) => {
                let s = val.string()?;
                // Check for PM filters like @aur, @pacman, etc. (@ instead of * to avoid shell globbing)
                if s.starts_with('@') {
                    filter_managers.push(s[1..].to_lowercase());
                } else if s.starts_with('*') {
                    // Still support * but warn about shell issues
                    filter_managers.push(s[1..].to_lowercase());
                } else {
                    packages.push(s);
                }
            }
            _ => return Err(arg.unexpected().into()),
        }
    }

    // Determine command
    if sync_mode {
        // -Ss means search in TUI, not install
        if search_string.is_some() {
            Ok(Command::Tui(TuiOpts {
                search_string,
                filter_managers,
            }))
        } else {
            Ok(Command::Sync(SyncOpts {
                refresh,
                force_refresh,
                upgrade,
                packages,
            }))
        }
    } else if query_mode {
        Ok(Command::Query(QueryOpts { info, packages }))
    } else {
        // Default to TUI mode
        // If packages are provided without -S, treat as search string
        let final_search = if search_string.is_some() {
            search_string
        } else if !packages.is_empty() {
            Some(packages.join(" "))
        } else {
            None
        };

        Ok(Command::Tui(TuiOpts {
            search_string: final_search,
            filter_managers,
        }))
    }
}

fn print_help() {
    println!(
        "pmux - Universal package manager TUI

USAGE:
    pmux [OPTIONS] [SEARCH...]
    pmux [@PM_FILTER...] [SEARCH]

PACMAN-STYLE OPERATIONS:
    -S <packages>           Install packages
    -Sy                     Sync package databases
    -Syu                    Sync databases and upgrade system
    -Syy                    Force refresh databases
    -Ss <search>            Search for packages (opens TUI)
    -Q                      Query installed packages
    -Qi <package>           Query package info

TUI MODE (default):
    pmux                    Open TUI browser
    pmux firefox            Open TUI with 'firefox' search
    pmux -Ss firefox        Same as above (pacman-style)
    pmux @aur               Filter to AUR only
    pmux @aur firefox       Filter to AUR + search 'firefox'

PM FILTERS (use @ to avoid shell globbing):
    @aur, @paru             AUR packages
    @pacman                 Pacman packages
    @dnf                    DNF packages
    @nix                    Nix packages
    @emerge, @gentoo        Portage packages

    Note: * also works but shells may expand it (use quotes: '*aur')

KEYBINDS:
    Ctrl+Space              Select/deselect package
    Enter                   Install selected packages
    Tab                     Switch focus (results ↔ installed)
    Esc/q                   Exit
    Alt+j/Alt+k, Up/Down    Navigate
    Ctrl+U/Ctrl+D           Half page scroll
    Home/End                Jump to start/end
    Ctrl+L                  Clear search
    Mouse hover             Highlight items
    Mouse click             Select package
    Mouse scroll            Scroll panels

EXAMPLES:
    pmux -Sy                Sync package databases
    pmux -Syu               Update system
    pmux -Ss firefox        Search for firefox (TUI)
    pmux firefox            Same as above
    pmux @aur vim           Browse AUR packages for vim
    pmux '@aur' vim         Same (quoted to avoid shell issues)
    pmux -S firefox         Install firefox directly
    pmux                    Open TUI browser

CONFIG:
    ~/.config/pmux/config.toml
    See config.example.toml for all options
"
    );
}
