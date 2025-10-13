# pmux

A fast package manager multiplexer with a TUI. Works across pacman, dnf, nix, emerge, and AUR.

## What it does

pmux lets you browse and install packages from multiple package managers in one interface. It's built for speed - sub-200ms startup times and instant search across tens of thousands of packages.

Works great on Bedrock Linux or any system with multiple package managers installed.

## Building from source

You'll need Rust installed. Then:

```bash
git clone https://github.com/yourusername/pmux
cd pmux
cargo build --release
sudo cp target/release/pmux /usr/local/bin/
```

First time setup:
```bash
pmux -Syy  # Sync repos and build cache
```

## Usage

### Interactive mode
```bash
pmux              # Browse all packages
pmux firefox      # Search for something specific
pmux @aur         # Filter to AUR only
pmux @nix firefox # Search firefox in nix packages
```

### Command line
```bash
pmux -S firefox   # Install
pmux -Ss firefox  # Search
pmux -Syu         # Full system upgrade
pmux -Q           # List installed
pmux -Qi firefox  # Package info
```

### Keys

- `j/k` or arrows - move around
- `Ctrl+Space` - select/deselect package
- `Enter` - install selected packages
- `Tab` - switch between results and installed list
- `Ctrl+U/D` - page up/down
- `Home/End` - jump to start/end
- `Ctrl+L` - clear search
- `q` or `Esc` - quit

Mouse support: click to select, scroll to navigate, hover to highlight.

### Filtering by package manager

Type `@` followed by the package manager name:
- `@pacman` - Arch repos
- `@aur` - AUR packages  
- `@dnf` - Fedora/RHEL
- `@nix` - Nix packages
- `@emerge` - Gentoo

## Configuration

Config lives at `~/.config/pmux/config.toml`. Check `config.example.toml` for all the options.

By default, pmux only loads your host package manager. To enable others:
- `enabled_pm = []` - load all available package managers
- `enabled_pm = ["pacman", "nix"]` - load specific ones

You can also customize colors, borders, layout, and more. See the example config for details.

## Supported package managers

- pacman (Arch Linux)
- paru/yay (AUR)
- dnf (Fedora/RHEL/CentOS)
- nix (NixOS/Nix)
- emerge (Gentoo)

Automatically detects Bedrock Linux strata and finds package managers in them.

### Third-party repositories

pmux automatically detects and syncs third-party repos from your system config:

- Pacman: chaotic-aur, archlinuxcn, blackarch, etc. (from `/etc/pacman.conf`)
- DNF: rpmfusion, terra, copr repos (from `/etc/yum.repos.d/*.repo`)
- Gentoo: overlays like guru, gentoo-zh (from `/etc/portage/repos.conf/`)
- Nix: uses nixpkgs unstable channel by default

No manual configuration needed.

## Performance

First run after syncing repos takes a bit while it builds the binary cache. After that:

- Startup: under 200ms
- Search: instant (even across 80k+ packages)
- Cache format: custom binary with memory mapping
- Memory usage: ~50% less than text-based caching

The cache lives in `~/.cache/pmux/`. To rebuild it, run `pmux -Syy`.

## How it works

pmux reads package databases directly instead of spawning commands where possible:

- Pacman: reads `/var/lib/pacman/sync/*.db` directly
- DNF: uses libdnf5 C++ API for native database access
- Nix: parses JSON from `nix-env` and `nix search`
- Emerge: reads Portage VDB and repo metadata

It uses a custom binary cache format with memory mapping for instant loads. Only your primary package manager loads on startup - others load on-demand when you filter to them.

Performance logging goes to `/tmp/pmux_performance.log` if you want to see what's taking time.

## License

GPL-3.0
