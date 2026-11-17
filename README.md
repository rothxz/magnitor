<div align="center">

<img src="assets/icon-1024.png" alt="Magnitor icon" width="128" height="128" />

# Magnitor

**Your Mac's vitals, one click away in the menu bar.**

Live CPU, memory and thermal readings, a searchable process list, and a disk-space breakdown, all in a native macOS menu bar app.

![macOS 14+](https://img.shields.io/badge/macOS-14%2B-000000?logo=apple&logoColor=white)
![Rust](https://img.shields.io/badge/Rust-data%20engine-B7410E?logo=rust&logoColor=white)
![TypeScript](https://img.shields.io/badge/TypeScript-sources-3178C6?logo=typescript&logoColor=white)
![Bun](https://img.shields.io/badge/Bun-tooling-FBF0DF?logo=bun&logoColor=black)

<br />

<img src="assets/screenshot-popover.png" alt="Magnitor's Activity Monitor tab open from the menu bar, showing CPU, memory and the process list" width="520" />

</div>

---

## Features

### Activity Monitor
- **CPU at a glance:** a live graph split into System and User load, with idle percentage.
- **Thermal state:** CPU temperature and macOS thermal pressure (Normal, Fair, Serious, Critical).
- **Memory breakdown:** used vs. total, with Cached, Compressed and Swap, and a pressure indicator that turns to *Warning* when memory runs low.
- **Process list:** every running process with its app icon, PID, % CPU and memory. Search it, and sort by CPU, Memory or Name.
- **Process details:** open a process to see its parent, threads, CPU time and real memory, then **Quit** or **Force Quit** it.

### Storage
- **Where the space went:** a pie chart of your home folder by size, measured on a thread per core so large folders don't stall the UI.
- **Drill down:** open any folder to see what it holds, or **Show in Finder**.
- **Clear Cache:** move `~/Library/Caches` and Xcode's DerivedData to the Trash in one step. Items that are in use are skipped.

### Always on
- **Menu bar icon:** choose Waveform, Gauge, Chip, Memory, Disk or Pie Chart, and choose what the menu bar item shows.
- **Alerts:** get notified when CPU or memory stays above a threshold for a set time, or when free disk space drops too low.
- **Desktop widget:** small and medium widgets with CPU, memory pressure and free disk space.
- **Global keyboard shortcut:** open Magnitor from anywhere.

## Requirements

| | |
|---|---|
| **OS** | macOS 14 Sonoma or later |
| **Runtime** | [Bun](https://bun.sh) |
| **Toolchain** | Rust (pinned by `rust-toolchain.toml`) and Xcode command-line tools |
| **SDK** | The Exact framework, checked out next to this repo at `../exact` (set `EXACT` to use another path) |

Run `bun ../exact/scripts/exact.mjs setup --check` to see what a native build still needs.

## Getting started

```bash
git clone https://github.com/rothxz/storage.git magnitor
cd magnitor
bun install

# Build and launch the macOS app
bun run dev            # same as: bun exact.mjs mac --run
```

Magnitor runs as a menu bar accessory: look for its icon in the menu bar, not the Dock.

### Other commands

| Command | What it does |
|---|---|
| `bun exact.mjs mac` | Build the macOS app without launching it |
| `bun exact.mjs web` | Web dev loop with reload on save |
| `bun exact.mjs test` | Run the `*.test.contract` UI tests |
| `bun exact.mjs verify` | Build, test, and check every screen in light and dark mode |
| `bun exact.mjs help` | List every available command |

## How it works

Magnitor is built with **Exact**: a declarative view and a typed contract, backed by Rust and TypeScript data sources.

```
app.contract        The UI: menu bar popover, tabs, settings and the widget
app.ts              TypeScript sources: disk size (df), app names (Spotlight),
                    Clear Cache (trash), Show in Finder (open), widget state
data/               Rust crate `magnitor-data`, running on its own worker thread
  src/walk.rs         Parallel folder walk for the Storage tab
  src/tasks.rs        Process list, details and quit/force quit
  src/vitals.rs       CPU, memory and pressure readings
  src/sensors.rs      Thermal state and CPU temperature
  src/icons.rs        App icons for processes
app.test.contract   UI tests
app.json            Manifest: name, bundle id, macOS window, widget
```

The heavy work (walking many gigabytes of folders and sampling processes) runs in Rust on a dedicated worker thread, so the popover stays responsive.

### Privacy

Magnitor runs entirely on your Mac and makes no network requests. Its permissions are declared up front in [`app.ts`](app.ts): it can run only `df`, `trash`, `mdfind`, `mdls` and `open`, read your cache folders, and send notifications.

## Contributing

Issues and pull requests are welcome. Before you open a PR, run:

```bash
bun exact.mjs verify
```

and make sure it reports `verify: clean`.

---

<div align="center">
<sub>Built with Exact · Rust · TypeScript</sub>
</div>
