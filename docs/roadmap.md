# Magnitor roadmap

What has been discussed for Magnitor but not built yet, with enough of the design and the
findings so far to start on any item without repeating the investigation. Effort is relative:
**S** is an afternoon, **M** a day or two, **L** most of a week.

## Where things stand

Built and tested (branch `feat/developer-features`, 2026-11-19):

- **Ports** tab: the TCP ports this user's processes listen on, read in-process with libproc
  (`data/src/ports.rs`). Development servers first, named for what was started (`vite`,
  `next dev`); open in the browser, copy the URL or command, stop or force stop; a warning on a
  server open to the network. Apps, macOS services and ports in the ephemeral range (49152 and
  up) fold under Other Ports.
- **Developer Files** in Storage: project build folders found beside their marker file, and
  about 25 tool caches (`data/src/devclean.rs`), measured, with a suggested selection, an
  "in use" guard, and Move to Trash through `FileManager.trashItem` (`modules/apple/Trash.swift`),
  so Finder's Put Back works. app.ts refuses any path the scan would not produce (`trashable`).
- **Process alerts**: a notification when one of this user's processes stays over a CPU share
  (100% of a core for five minutes by default) or its footprint keeps rising over ten minutes
  (1 GB and half again, rising on most readings), each at most once in 30 minutes per name
  (`data/src/watchdog.rs`). A light reading every ten seconds while they are on: about 6 ms.
  Clicking an alert opens Activity at that process.
- **Ports follow-ups**: a container's port named for its container (Docker's Engine API over its
  socket, `data/src/docker.rs`), whose Stop stops the container; ↑ ↓, Enter and ⌘K in the list.
- **Developer Files follow-ups**: Elixir, Haskell and CMake build folders; Go's module cache,
  a Docker prune and unavailable simulators freed by their own commands after a confirmation
  (`data/src/devtools.rs`); Delete Now for what Magnitor moved to the Trash (only those items:
  emptying the whole Trash would need Apple Events, which Exact can't declare); folders to add
  to or leave out of the walk (Settings); a monthly reminder when suggested items pass a size.

## Next features

### 1. Abnormal activity alerts — the rest, S to M

Busy processes and growing memory are built (above), on fixed thresholds with a cooldown, which
the other items here can reuse (`procPulse` in `app.contract`'s `watch`). Still open:

- A new listening port that is open to the network (`ports.rs` already knows `exposed`).
- An ignore list ("don't alert for Xcode"), and other users' processes, which the reading
  leaves out today because they can't be quit from Magnitor.
- A baseline learned per process instead of fixed thresholds, if the fixed ones prove noisy.

### 2. Leftover developer processes — M

Find dev processes that outlived their terminal: `node`, `vite`, file watchers, `esbuild`,
language servers whose parent is gone (reparented to launchd, pid 1) or whose project folder no
longer has an editor or terminal open. Offer "Stop all leftovers" in Activity or Ports.

`ports.rs`'s `working_dirs` and `server_name` already give each process's folder and a friendly
name; `tasks.rs` has the parent pid. The risk is false positives (a server started on purpose
with `nohup`), so list them for review rather than ending them automatically.

### 3. Group processes by developer tool — S to M

Extend Activity's app grouping (`tasks.rs`'s `group_apps`) to name what developers see: "VS Code
→ tsserver, eslint", "Docker VM", "Xcode build", "cargo". Today a process outside an app bundle
is its own row.

### 4. Keyboard command bar — M

Act from the keyboard as soon as the popover opens: `kill 3000`, `top cpu`, `free space`,
`quit slack`. The global shortcut already opens the popover; this is a search field that parses
a verb and a target and maps it onto existing actions (`stopPort`, `endProcess`, `endApp`,
opening Developer Files).

### 5. URL scheme, command-line tool and Raycast extension — M

Let other tools drive Magnitor: `magnitor://ports`, `magnitor://stop?port=3000`, and a small CLI
(`magnitor ports`, `magnitor kill :3000`). A Raycast extension on top reaches users where they
already work. Exact handles deep links on the `navigate=` root (`contract vocab navigate`); a
CLI would talk to the running app or reuse the Rust crate directly.

### 6. Build-finished notifications and throttling warnings — S to M

Notice when a long, CPU-heavy `xcodebuild`, `cargo`, `next build` or `gradle` exits and post
"Build finished in 4 m 12 s". Warn when the thermal state (already read for Activity's CPU card)
reaches Serious during a build. Needs the same background reading as 1.

### 7. Short history — M

Keep 24 hours of CPU and memory, with the top process at each sample, to answer "what made the
Mac slow at 2 pm?". Store a compact ring buffer in `app:/data`; draw it as a chart in Activity.
Storage: one sample a minute is 1,440 rows a day.

## Follow-ups to Ports

- **Root's ports.** The kernel describes only this user's processes, so system daemons' ports
  (and a Postgres running as another user) are not listed. Showing them needs a privileged
  helper; probably not worth it.
- **Port conflicts.** When a dev server fails with "address in use", a notification naming the
  holder and offering Stop would close the loop. Magnitor can't see the terminal's error, so it
  needs a signal first: a process that starts, tries a port another holds, and exits at once.
- **Docker, tried for real.** Container names, Stop and the prune are unit-tested against the
  Engine's replies only; try them with Docker Desktop and OrbStack running.

## Follow-ups to Developer Files

- **Delete Now, tried for real.** Under the test driver nothing is deleted. Whether macOS lets
  Magnitor delete from the Trash without Full Disk Access is untried; if it refuses, the notice
  sends the person to Finder.
- **Simulators, tried for real.** `xcrun simctl delete unavailable` has not run on a Mac with
  unavailable simulators.

## Housekeeping

- **Drop the web target.** `app.json` ships `host.web` and `deploy.store.web`, but a menu bar
  app that reads processes, ports and disks shows only errors in a browser. Remove it with
  `bun exact.mjs update --without web`, which also clears the SF Symbol warnings. S.
- **Rename the fixture project.** `tools/home/Projects/web` reads like web support; rename it
  (for example to `Projects/shop`) and update `app.test.contract`. S.
- **Upstream the Exact crash fix.** A status item's popover shown before the menu bar placed the
  item (a launch from a notification's click) crashed the host: `TextSelectionMac.swift` built
  a dictionary from a list that held a paragraph twice. The fix (count each paragraph once) is
  in the local `../exact` checkout, uncommitted, and noted for Exact's feedback. Magnitor avoids
  it on the pinned Exact by opening the popover half a second after the click (`statsOpener`).
  Send it to `expo/exact`, then move `EXACT_REF` in `.github/workflows/macos.yml`. S.
- **A fragile test.** "a search finds Magnitor itself" fails when a second Magnitor is running
  (`bun run dev` beside the test run): the two group into one app row. Either make the test
  open whichever view appears, or have the test run refuse to start beside another instance. S.
- **Exact feedback.** Two notes are recorded locally and not sent (`bun exact.mjs feedback`
  shows them): `buttonStyle` is refused inside a popover though start-here recommends it, and
  `agent macos` cannot drag inside a popover. Send with `bun exact.mjs feedback send --yes`.
