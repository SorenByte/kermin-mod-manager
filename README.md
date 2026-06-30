# Kermin Mod Manager

Repository: https://github.com/SorenByte/kermin-mod-manager

A lightweight mod manager for **Single Player Tarkov (SPT) 4.0**, built with Tauri
(Rust + a small web UI). Install mods from the Forge by search or list link, drag
in local archives, and manage, enable/disable, update, and uninstall everything
from one place.

> Not affiliated with Battlestate Games or the SPT team. "Escape from Tarkov" is a
> trademark of Battlestate Games. This is a community tool that reads the public,
> read-only Forge API (`https://forge.sp-tarkov.com/api/v0`).

## Features

- **Portable.** Drop the single `.exe` into your SPT game folder and run it. It
  uses its own folder as the SPT root, no install or setup. Delete it whenever.
- **Unified add bar.** Type a mod name to search the Forge, or paste a mod link or
  a mod-list link. Pasting a list link makes modpack installation very easy.
- **Rich search results** with thumbnail, version, size, SPT-version compatibility,
  downloads, author, category, and Fika badge.
- **Install queue** in a floating sidebar. Everything you add (search, list,
  drag-drop) lands here. Install it all in one go, with per-mod progress and an
  Abort button to skip a stuck or unwanted download/install.
- **Automatic dependency resolution.** Missing dependencies are detected and added.
- **Manage tab.** One list of installed mods with client / server / both tags,
  enable-disable toggles (a combo mod's halves move together), update checking with
  changelogs, multi-select uninstall, search and filters, and disk usage.
- **Metadata cache.** Mod icons and details are cached in your SPT folder and only
  refreshed once a day, so the Manage list is fast and works offline.

## Using the release build

1. Download the `.exe`.
2. Put it in your SPT game folder (the one containing `BepInEx` and `SPT`).
3. Run it. If it is not in a valid SPT folder, it will warn you and let you pick one.

## Building from source (NOT FOR NORMAL USERS)

Prerequisites (one time):

1. **Rust** - https://www.rust-lang.org/tools/install
2. **Node.js** v18+ - https://nodejs.org (only used to run the Tauri CLI)
3. On Windows: **WebView2** (preinstalled on Win10/11) and the **MS C++ Build Tools**.
   Full list: https://tauri.app/start/prerequisites/

Then:

```bash
npm install
npm run dev               # run in development
npm run build:portable    # build the portable exe (no installer bundle)
```

The portable exe is written to `src-tauri/target/release/kermin-mod-manager.exe`.

Run the backend unit tests (archive resolver logic):

```bash
cd src-tauri && cargo test
```

## How it works

- The UI is plain HTML/CSS/JS served by Tauri; all file and network work happens in
  Rust commands (`src-tauri/src/lib.rs`).
- Installs record a manifest under `<SPT>/.spt-mod-installer/manifests/` listing the
  exact files written, plus the Forge id and version. That powers clean uninstalls,
  combo-mod handling, update checks, and the metadata cache.
- Enable/disable moves a mod's files to `<SPT>/.spt-mod-installer/disabled/` and back.

## Known limitations

- A small number of mods are packaged with an LZMA variant that the pure-Rust
  archive decoders do not handle yet. Those fail or stall on extraction; you can
  Abort and skip them and install them manually with 7-Zip. Reports welcome.
- Update tracking and rich icons only apply to mods installed through this app (they
  carry a Forge id). Manually added mods still appear, just without those extras.

## Contributing

Issues and pull requests are welcome. The codebase is intentionally small: one Rust
file for the backend commands and three files (`index.html`, `main.js`, `styles.css`)
for the UI.

## License

MIT. See [LICENSE](LICENSE).
