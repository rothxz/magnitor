// Magnitor: the quick sources. The Rust crate in data/ answers `usage` and `contents`, walking the
// folders on many threads; this answers the disk's size with `df`, the apps' names from
// Spotlight, Show in Finder and a local server's page with `open`, moves Developer Files to the
// Trash (modules/apple's `trash`, Finder's own, so Put Back works) and deletes for good what it
// moved there when asked (`purge`), resolves a folder Settings' picker chose, and keeps the desktop
// widget's reading (`saveWidget`, which the widget's `widgetVitals` reads). Every child runs with
// the home directory as its working directory, so paths here are relative to it.
import type { Result, TypeScriptAnswer, TypeScriptSources } from './app.contract.d.ts';
import { execFile } from '@exact/child-process';

// The widget's last reading, written by the app and read by the widget's refresh.
const WIDGET_FILE = 'app:/data/widget.json';

export const grants = [
  'process.spawn /bin/df',
  'process.spawn /usr/bin/mdfind',
  'process.spawn /usr/bin/mdls',
  'process.spawn /usr/bin/open',
  'fs.read app:/data/widget.json',
  'fs.write app:/data/widget.json',
  // A folder Settings' picker chose, as its path on disk (`folderPath`).
  'fs.read doc:/',
  'device.notifications purpose.notifications',
  'device.global-shortcuts',
  // Sharing with Magnitor Remote: the Rust crate announces this Mac by Bonjour (data/src/share.rs).
  'device.local-network purpose.localNetwork',
  'net.dns-sd _magnitor._tcp',
].join('\n');

// Finder's decimal units: 511 KB, 211.2 MB, 2 GB (data/src/lib.rs's `format_bytes`).
export function formatBytes(bytes: number): string {
  if (bytes < 1000) return `${bytes} bytes`;
  if (bytes < 1e6) return `${Math.round(bytes / 1e3)} KB`;
  const [n, unit] = bytes < 1e9 ? [bytes / 1e6, 'MB'] : bytes < 1e12 ? [bytes / 1e9, 'GB'] : [bytes / 1e12, 'TB'];
  return `${(Math.round(n * 10) / 10).toFixed(1).replace(/\.0$/, '')} ${unit}`;
}

// What Developer Files may move to the Trash, kept in step with data/src/devclean.rs: a
// project folder by its name (RULES), or a tool's cache by its path from home (CACHES). The view
// sends only what the scan found; this refuses anything else, whatever asks.
const PROJECT_FOLDERS = new Set([
  'node_modules', '.next', '.nuxt', '.svelte-kit', '.turbo', '.parcel-cache', '.angular', 'target', '.build', 'Pods',
  'build', '.gradle', '.dart_tool', '.venv', 'venv', '.zig-cache', '_build', 'deps', '.stack-work', 'dist-newstyle',
]);
// RULES' `cmake-build-*`: CLion's build folders, by their start.
const PROJECT_FOLDER_STARTS = ['cmake-build-'];

function projectFolder(name: string): boolean {
  return PROJECT_FOLDERS.has(name) || PROJECT_FOLDER_STARTS.some((s) => name.startsWith(s) && name.length > s.length);
}
const CACHES = new Set([
  'Library/Developer/Xcode/DerivedData', 'Library/Caches/com.apple.dt.Xcode', 'Library/Developer/CoreSimulator/Caches',
  'Library/Developer/Xcode/iOS DeviceSupport', 'Library/Developer/Xcode/watchOS DeviceSupport',
  'Library/Developer/Xcode/tvOS DeviceSupport', 'Library/Developer/Xcode/visionOS DeviceSupport',
  'Library/Developer/Xcode/Archives', '.npm/_cacache', 'Library/Caches/Yarn', 'Library/pnpm/store', '.bun/install/cache',
  'Library/Caches/Homebrew', 'Library/Caches/CocoaPods', '.gradle/caches', '.m2/repository', '.cargo/registry',
  'Library/Caches/pip', 'Library/Caches/go-build', 'Library/Caches/node-gyp', 'Library/Caches/typescript',
  'Library/Caches/deno', 'Library/Caches/JetBrains', 'Library/Caches/ms-playwright', 'Library/Caches/Cypress',
  '.cache/puppeteer',
]);

// A path from home that Developer Files may move: a known cache, or a project folder inside a
// project the scan walks (not under Library or a hidden folder, as devclean.rs's walk skips).
// An absolute path is a project folder inside one of the folders Settings adds to the walk.
export function trashable(path: string, roots: string[] = []): boolean {
  if (path.startsWith('/')) {
    const root = roots.find((r) => r.startsWith('/') && r.length > 1 && path.startsWith(r.endsWith('/') ? r : `${r}/`));
    if (!root) return false;
    const inner = path.slice(root.length).replace(/^\//, '');
    const parts = inner.split('/');
    return parts.length >= 2 && !parts.some((p) => p === '' || p === '.' || p === '..') && projectFolder(parts[parts.length - 1]);
  }
  const parts = path.split('/');
  if (path === '' || parts.some((p) => p === '' || p === '.' || p === '..')) return false;
  if (CACHES.has(path)) return true;
  return parts.length >= 2 && parts[0] !== 'Library' && !parts[0].startsWith('.') && projectFolder(parts[parts.length - 1]);
}

const items = (n: number) => `${n} ${n === 1 ? 'item' : 'items'}`;

// What the Trash took, as the view's notice says it.
export function trashMessage(asked: number, moved: number, bytes: number): string {
  if (moved === 0) return asked === 1 ? 'Couldn’t move it to the Trash. It may be in use or protected.' : 'Couldn’t move them to the Trash. They may be in use or protected.';
  if (moved < asked) return `Moved ${moved} of ${items(asked)} to the Trash. The rest are in use or protected.`;
  return `Moved ${items(moved)} to the Trash. Delete them from the Trash to free ${formatBytes(bytes)}.`;
}

// What deleting the moved items did, as the view's notice says it.
export function purgeMessage(asked: number, deleted: number, bytes: number, denied: boolean): string {
  if (deleted === 0) {
    return denied
      ? 'macOS didn’t let Magnitor into the Trash. Empty it in Finder, or give Magnitor Full Disk Access in System Settings › Privacy & Security.'
      : 'Nothing was deleted: the items are no longer in the Trash.';
  }
  if (deleted < asked) return `Deleted ${deleted} of ${items(asked)}. The rest are no longer in the Trash.`;
  return `Deleted ${items(deleted)} for good, freeing ${formatBytes(bytes)}.`;
}

// A local server's page: Ports opens nothing else.
export function localUrl(url: string): boolean {
  return /^http:\/\/localhost:\d{1,5}$/.test(url);
}

// A program that ran and exited with a status answers its output; not starting, a signal
// or the timeout throws.
async function run(program: string, args: string[]): Promise<string> {
  try {
    return (await execFile(program, args, { encoding: 'utf8', timeout: 60_000, maxBuffer: 4 << 20 })).stdout;
  } catch (e) {
    const { code, stdout, killed } = e as { code?: unknown; stdout?: unknown; killed?: boolean };
    if (typeof code === 'number' && !killed && typeof stdout === 'string') return stdout;
    throw e;
  }
}

// `df -k .`: the second line's fields are filesystem, size, used, available (KiB).
export function parseDf(out: string): { size: number; free: number } {
  const fields = (out.trim().split('\n')[1] ?? '').trim().split(/\s+/);
  return { size: Number(fields[1] ?? 0) * 1024, free: Number(fields[3] ?? 0) * 1024 };
}

// Each installed app's name by its bundle id, lowercased, asked of Spotlight once and kept.
// With no answer from Spotlight the folders keep their own names.
let installed: Promise<Map<string, string>> | undefined;
async function installedApps(): Promise<Map<string, string>> {
  const names = new Map<string, string>();
  try {
    const apps = (await run('/usr/bin/mdfind', ["kMDItemContentType == 'com.apple.application-bundle'"])).split('\n').filter((p) => p.endsWith('.app'));
    if (apps.length === 0) return names;
    // `-raw` prints each app's two values in the order named, NUL after each but the last.
    const fields = (await run('/usr/bin/mdls', ['-name', 'kMDItemCFBundleIdentifier', '-name', 'kMDItemDisplayName', '-raw', ...apps])).split('\0');
    for (let i = 0; i + 1 < fields.length; i += 2) {
      const id = fields[i].trim().toLowerCase(), name = fields[i + 1].trim().replace(/\.app$/, '');
      if (id && id !== '(null)' && name && name !== '(null)' && !names.has(id)) names.set(id, name);
    }
  } catch {
    // Spotlight off or refused: folder names stand.
  }
  return names;
}

// The sources Rust does not answer (data/src/lib.rs's RUST_SOURCES).
const sources: TypeScriptSources = {
  disk: async ([_generation, _minute]): Promise<Result<'disk'>> => {
    try {
      const { size, free } = parseDf(await run('/bin/df', ['-k', '.']));
      return { size, free: formatBytes(free), freeBytes: free };
    } catch {
      return { size: 0, free: '', freeBytes: 0 };
    }
  },
  // The name of the app that keeps each cache folder, in the folders' order ("" for none):
  // many are named by the bundle id of their app (`com.apple.Safari`).
  appNamesFor: async ([folders]): Promise<Result<'appNamesFor'>> => {
    const names = await (installed ??= installedApps());
    return folders.map((folder) => ({ id: folder, name: names.get(folder.toLowerCase()) ?? '' }));
  },
  // Finder, with the item selected in its folder.
  reveal: async ([path]): Promise<Result<'reveal'>> => {
    try {
      await execFile('/usr/bin/open', ['-R', path], { encoding: 'utf8', timeout: 10_000 });
      return { ok: true };
    } catch {
      return { ok: false };
    }
  },
  // A server's page in the browser, from Ports.
  openUrl: async ([url]): Promise<Result<'openUrl'>> => {
    if (!localUrl(url)) return { ok: false };
    try {
      await execFile('/usr/bin/open', [url], { encoding: 'utf8', timeout: 10_000 });
      return { ok: true };
    } catch {
      return { ok: false };
    }
  },
  // Developer Files to the Trash, by the native module (FileManager's `trashItem`, as Finder
  // moves them). `bytes` is what the view chose, for the notice.
  trashItems: async ([paths, bytes, roots], _store, _storage, native): Promise<Result<'trashItems'>> => {
    const asked = paths.filter((p) => trashable(p, roots));
    if (asked.length === 0) return { ok: false, moved: [], message: 'Nothing here can be moved to the Trash.' };
    if (!native?.available) return { ok: false, moved: [], message: 'The Mac app moves files to the Trash.' };
    try {
      const reply = (await native.later({ op: 'trash', paths: asked })) as { moved?: unknown };
      const moved = Array.isArray(reply.moved) ? asked.filter((p) => (reply.moved as unknown[]).includes(p)) : [];
      // Refused paths count as not moved.
      return { ok: moved.length > 0, moved, message: trashMessage(paths.length, moved.length, bytes) };
    } catch (e) {
      return { ok: false, moved: [], message: `Couldn’t move to the Trash: ${(e as Error).message}` };
    }
  },
  // What Developer Files moved to the Trash this session, deleted for good: the native module
  // removes only the items its own moves put there, nothing else in the Trash.
  purgeTrashed: async ([paths, bytes], _store, _storage, native): Promise<Result<'purgeTrashed'>> => {
    if (!native?.available) return { ok: false, moved: [], message: 'The Mac app empties what it moved.' };
    try {
      const reply = (await native.later({ op: 'purge', paths })) as { deleted?: unknown; denied?: unknown };
      const deleted = Array.isArray(reply.deleted) ? paths.filter((p) => (reply.deleted as unknown[]).includes(p)) : [];
      return { ok: deleted.length > 0, moved: deleted, message: purgeMessage(paths.length, deleted.length, bytes, reply.denied === true) };
    } catch (e) {
      return { ok: false, moved: [], message: `Couldn’t delete them: ${(e as Error).message}` };
    }
  },
  // A folder Settings' picker chose (`doc:/1/Work`), as the walk reads it: its path on disk.
  folderPath: async ([doc], _store, storage): Promise<Result<'folderPath'>> => {
    try {
      return { path: await storage.fs.realpath(doc) };
    } catch {
      return { path: '' };
    }
  },
  // The app's reading for the widget, kept until the next; the widget's timeline reloads after it.
  saveWidget: async ([shows, cpu, memory, pressure, free, freeShare, at], _store, storage): Promise<Result<'saveWidget'>> => {
    const reading: WidgetReading = { shows, cpu, memory, pressure, free, freeShare, at };
    try {
      await storage.fs.atomicWriteFile(WIDGET_FILE, new TextEncoder().encode(JSON.stringify(reading)));
      return { ok: true };
    } catch {
      return { ok: false };
    }
  },
  // Updates, from modules/apple/Updater.swift: the latest GitHub release, and installing it.
  // Hosts without the native module (the web) have nothing to update.
  checkUpdate: async (_args, _store, _storage, native): Promise<Result<'checkUpdate'>> =>
    native?.available ? (await native.later({ op: 'check' })) as Result<'checkUpdate'> : NO_UPDATE,
  installUpdate: async ([url, version], _store, _storage, native): Promise<Result<'installUpdate'>> =>
    native?.available
      ? (await native.later({ op: 'install', url, version })) as Result<'installUpdate'>
      : { ok: false, message: 'Updates are installed by the Mac app.' },
  // The desktop widget's one entry: the app's last reading, or a note to open the app.
  widgetVitals: async ([date], _store, storage): Promise<Result<'widgetVitals'>> => {
    let props: WidgetProps = NO_READING;
    try {
      props = widgetProps(JSON.parse(new TextDecoder().decode(await storage.fs.readFile(WIDGET_FILE))));
    } catch {
      // No reading yet ('bake', the build, passes here too).
    }
    return [{ date, props }];
  },
};

const NO_UPDATE: Result<'checkUpdate'> = {
  current: '', latest: '', available: false, notes: '', page: '', download: '', canInstall: false, error: '',
};

type WidgetProps = Result<'widgetVitals'>[number]['props'];
interface WidgetReading { shows: string; cpu: number; memory: number; pressure: string; free: string; freeShare: number; at: number }

const NO_READING: WidgetProps = {
  shows: 'all', cpu: 0, memory: 0, pressure: '', free: '', freeShare: 0,
  updated: 'Open Magnitor to start', ready: false,
};

// The reading as the widget shows it, with the time it was taken.
export function widgetProps(r: WidgetReading): WidgetProps {
  const time = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' }).format(new Date(r.at));
  return {
    shows: r.shows, cpu: r.cpu, memory: r.memory, pressure: r.pressure, free: r.free, freeShare: r.freeShare,
    updated: `Updated ${time}`, ready: true,
  };
}

export const answer: TypeScriptAnswer = (source, args, store, storage, native) =>
  sources[source](args, store, storage, native);
