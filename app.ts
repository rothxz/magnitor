// Magnitor: the quick sources. The Rust crate in data/ answers `usage` and `contents`, walking the
// folders on many threads; this answers the disk's size with `df`, the apps' names from
// Spotlight, Clear Cache with `trash` and Show in Finder with `open`, and keeps the desktop
// widget's reading (`saveWidget`, which the widget's `widgetVitals` reads). Every child runs with
// the home directory as its working directory, so paths here are relative to it.
import type { Result, Storage, TypeScriptAnswer, TypeScriptSources } from './app.contract.d.ts';
import { execFile } from '@exact/child-process';

// The widget's last reading, written by the app and read by the widget's refresh.
const WIDGET_FILE = 'app:/data/widget.json';

export const grants = [
  'process.spawn /bin/df',
  'process.spawn /usr/bin/trash',
  'process.spawn /usr/bin/mdfind',
  'process.spawn /usr/bin/mdls',
  'process.spawn /usr/bin/open',
  'fs.read home:/Library/Caches',
  'fs.read home:/Library/Developer/Xcode/DerivedData',
  'fs.read app:/data/widget.json',
  'fs.write app:/data/widget.json',
  'device.notifications purpose.notifications',
  'device.global-shortcuts',
].join('\n');

// The cache folders Clear Cache empties. This app's own cache stays: it is in use.
const CACHE_DIRS = ['Library/Caches', 'Library/Developer/Xcode/DerivedData'];
const OWN_CACHE = 'com.example.magnitor-addab7';

// Finder's decimal units: 511 KB, 211.2 MB, 2 GB (data/src/lib.rs's `format_bytes`).
export function formatBytes(bytes: number): string {
  if (bytes < 1000) return `${bytes} bytes`;
  if (bytes < 1e6) return `${Math.round(bytes / 1e3)} KB`;
  const [n, unit] = bytes < 1e9 ? [bytes / 1e6, 'MB'] : bytes < 1e12 ? [bytes / 1e9, 'GB'] : [bytes / 1e12, 'TB'];
  return `${(Math.round(n * 10) / 10).toFixed(1).replace(/\.0$/, '')} ${unit}`;
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

// The names inside each cache folder, as paths from home; a folder that is missing is empty.
async function cacheEntries(storage: Storage): Promise<string[]> {
  const entries: string[] = [];
  for (const dir of CACHE_DIRS) {
    try {
      for (const name of await storage.fs.readdir(`home:/${dir}`)) if (name !== OWN_CACHE) entries.push(`${dir}/${name}`);
    } catch (e) {
      if ((e as { code?: string }).code !== 'ENOENT') throw e;
    }
  }
  return entries;
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
  clearCache: async (_args, _store, storage): Promise<Result<'clearCache'>> => {
    try {
      const entries = await cacheEntries(storage);
      if (entries.length === 0) return { ok: true, message: 'The caches are already empty.' };
      try {
        await execFile('/usr/bin/trash', entries, { encoding: 'utf8', timeout: 180_000 });
      } catch (e) {
        // Some items can't be moved (one in use or protected); the rest were.
        const stderr = String((e as { stderr?: string }).stderr ?? '').trim();
        const failures = stderr === '' ? 0 : stderr.split('\n').length;
        if (failures >= entries.length) throw e;
        return { ok: true, message: `Moved ${entries.length - failures} of ${entries.length} items to the Trash. The rest are in use or protected.` };
      }
      return { ok: true, message: `Moved ${entries.length} ${entries.length === 1 ? 'item' : 'items'} to the Trash.` };
    } catch (e) {
      return { ok: false, message: `Couldn’t clear the cache: ${(e as Error).message}` };
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
