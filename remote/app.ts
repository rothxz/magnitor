// Magnitor Remote's data module: a Mac's Activity Monitor, asked of Magnitor on that Mac
// (Magnitor's data/src/share.rs) over the local network. A Mac Bonjour found is resolved once and
// its address kept until a request to it fails; then it is resolved again and a read is asked once
// more (its address can change). Resolves go one at a time, as Android's NSD refuses a second
// while one runs. A typed Mac is used as written, on Magnitor's port unless it names one.
// A refused code and an unreachable Mac are answers the screen shows, not failures, as is control
// the Mac hasn't allowed ("off").
//
// The Mac sends its last 40 readings of CPU and memory as percentages; the graphs' paths are drawn
// here, in a 100 × 40 box the view stretches to its width (`GRAPH_W`, `GRAPH_H`).
//
// The Apps grid's icons come from the Mac (`/v1/icons`, a dozen a request) and are kept in
// `app:/cache/icons`, one PNG per app and version of it (its `stamp`), so they are asked for once.
// A Mac whose Magnitor predates icons answers 404, and the grid keeps its letter tiles.
//
// Security (Magnitor's data/src/guard.rs): this phone has a P-256 key pair of its own, kept in the
// keychain (`secret.keep magnitor-device`); the private half never leaves it. Pairing proves the
// Mac's one-time code without sending it (`pairProof`) and waits until the person allows this
// phone on the Mac, comparing the six digits both show (`checkDigits`). Each request after is
// signed (`signedText`) over the session's challenge from `/v1/hello`, a sequence number that only
// grows, the method and the URL as it goes on the wire, so it can be neither forged nor replayed.
// A session the Mac no longer has ("stale") is opened again once, and the request sent again.
import type { Answer, Result, Sources, Storage, Store } from './app.contract.d.ts';
import { resolveService } from '@exact/local-network';
import { enc, keyFacts, pairProof, sign, signedText } from './sign.ts';

export const grants = [
  'device.local-network purpose.localNetwork',
  'net.dns-sd _magnitor._tcp',
  'net.fetch local-network',
  'secret.keep magnitor-device',
  'fs.read app:/cache/icons',
  'fs.write app:/cache/icons',
].join('\n');

type Activity = Result<'activity'>;
type Proc = Activity['processes'][number];
type Detail = Result<'detail'>;
type Ended = Result<'endProcess'>;
type Controls = Result<'controls'>;
type AppIcon = Result<'appIcons'>[number];
type Pairing = Result<'pairMac'>;
const TYPE = '_magnitor._tcp';
/** The port Magnitor tries first (share.rs's `PORT`). */
const PORT = 47820;
/** Readings the Mac keeps (share.rs's and tasks.rs's `HISTORY`), and the graph's box. */
const SLOTS = 40;
const GRAPH_W = 100;
const GRAPH_H = 40;

const blank = (status: string, message: string): Activity => ({
  name: '', cpuUser: '', cpuSystem: '', cpuIdle: '', cpuLoad: 0, cpuPeak: 0, cpuAverage: 0,
  cpuUserArea: '', cpuSystemArea: '', cpuLine: '', memoryArea: '', memoryLine: '', memoryPeak: 0,
  memoryUsed: '', memoryTotal: '', memoryShare: 0, cached: '', compressed: '', swap: '',
  pressure: '', thermal: '', temperature: '', count: 0, more: 0, matchCount: 0, processes: [], status, message,
});

const blankDetail = (status: string, message: string): Detail => ({
  found: false, group: false, pid: 0, name: '', path: '', user: '', parent: '', cpu: '', cpuTime: '', threads: '',
  memory: '', realMemory: '', running: '', count: 0, mine: false, own: false, processes: [], status, message,
});

/** A Mac Bonjour resolved, kept until a request to it fails. */
const resolved = new Map<string, string>();
/** A resolve under way for each Mac, shared by whoever asks meanwhile, and the last one queued:
 * the next waits for it. */
const resolving = new Map<string, Promise<string>>();
let lastResolve: Promise<unknown> = Promise.resolve();

function resolveUrl(id: string): Promise<string> {
  const running = resolving.get(id);
  if (running) return running;
  const p = lastResolve
    .then(() => resolveService({ name: id.slice(4), type: TYPE }))
    .then((s) => {
      const host = s.host.includes(':') && !s.host.startsWith('[') ? `[${s.host}]` : s.host;
      const url = `http://${host}:${s.port}`;
      resolved.set(id, url);
      return url;
    })
    .finally(() => resolving.delete(id));
  lastResolve = p.catch(() => undefined);
  resolving.set(id, p);
  return p;
}

/** Where a Mac is: its typed address, else its resolved host and port (as last resolved, unless
 * `fresh`). */
async function where(id: string, fresh = false): Promise<string> {
  if (id.startsWith('addr:')) {
    const typed = id.slice(5).replace(/^https?:\/\//, '').replace(/\/+$/, '');
    return `http://${/:\d+$/.test(typed) ? typed : `${typed}:${PORT}`}`;
  }
  const known = resolved.get(id);
  if (!fresh && known) return known;
  return resolveUrl(id);
}

const KEY = 'magnitor-device';
/** The name a Mac shows for this phone. */
const DEVICE_NAME = 'iPhone';
type Device = { pair: CryptoKeyPair; x: string; y: string; id: string; check: string };
let device: Promise<Device> | null = null;

/** This phone's key pair, made once and kept in the keychain, and what the Mac knows it by. */
function myDevice(store: Store): Promise<Device> {
  device ??= (async () => {
    let pair = await store.key(KEY);
    if (!pair) {
      pair = (await crypto.subtle.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, false, ['sign', 'verify'])) as CryptoKeyPair;
      await store.keepKey(KEY, pair);
    }
    const jwk = await crypto.subtle.exportKey('jwk', pair.publicKey);
    const x = String(jwk.x), y = String(jwk.y);
    return { pair, x, y, ...(await keyFacts(x, y)) };
  })().catch((e) => { device = null; throw e; });
  return device;
}

/** Each Mac's session: its challenge and the last sequence number this phone used. */
const sessions = new Map<string, { challenge: string; seq: number }>();

/** The Mac's word on this phone (`/v1/hello`): "paired" (and its session kept), "pending",
 * "denied" or "unpaired"; or why it couldn't be asked. */
async function hello(id: string, d: Device): Promise<{ status: string } | { status: 'offline'; message: string }> {
  let r: Response;
  try {
    r = await fetch(`${await where(id, !resolved.has(id))}/v1/hello?id=${d.id}`);
  } catch {
    resolved.delete(id);
    return { status: 'offline', message: away(nameOf(id)) };
  }
  if (r.status === 404 || r.status === 405) return { status: 'offline', message: `Update Magnitor on ${nameOf(id)} to pair this iPhone.` };
  if (r.status === 429) return { status: 'offline', message: LOCKED };
  if (!r.ok) return { status: 'offline', message: `${nameOf(id)} answered ${r.status}. Update Magnitor on your Mac and try again.` };
  const j = (await r.json()) as Record<string, unknown>;
  const status = text(j.status);
  if (status === 'paired') sessions.set(id, { challenge: text(j.challenge), seq: num(j.seq) });
  return { status };
}

const LOCKED = 'Your Mac stopped answering this iPhone after too many failed attempts. Try again in 15 minutes.';

/** Pairs with the Mac by its code: "pending" until the person allows this phone there (it shows
 * `check`), "paired" for a phone it already allows, "code" when the code is wrong or spent. */
async function pairMac(id: string, code: string, store: Store): Promise<Pairing> {
  const name = nameOf(id);
  let d: Device;
  try { d = await myDevice(store); } catch { return { status: 'offline', check: '', message: 'This iPhone couldn’t make its key. Restart Magnitor Remote and try again.' }; }
  const query = `name=${enc(DEVICE_NAME)}&x=${enc(d.x)}&y=${enc(d.y)}&proof=${await pairProof(code, d.x, d.y)}`;
  let r: Response;
  try {
    r = await fetch(`${await where(id, true)}/v1/pair?${query}`, { method: 'POST' });
  } catch {
    resolved.delete(id);
    return { status: 'offline', check: '', message: away(name) };
  }
  sessions.delete(id);
  const j = (await r.json().catch(() => ({}))) as Record<string, unknown>;
  if (r.status === 401) return { status: 'code', check: '', message: text(j.error) };
  if (r.status === 429) return { status: 'offline', check: '', message: LOCKED };
  if (r.status === 404 || r.status === 405) return { status: 'offline', check: '', message: `Update Magnitor on ${name} to pair this iPhone.` };
  if (!r.ok) return { status: 'offline', check: '', message: `${name} answered ${r.status}. Update Magnitor on your Mac and try again.` };
  // The digits are this phone's own, from its key: a Mac shown another key shows others.
  return { status: text(j.status) === 'paired' ? 'paired' : 'pending', check: d.check, message: '' };
}

/** Whether the person has allowed this phone yet, asked while it waits. */
async function pairState(id: string, store: Store): Promise<Pairing> {
  let d: Device;
  try { d = await myDevice(store); } catch { return { status: 'offline', check: '', message: 'This iPhone couldn’t make its key.' }; }
  const h = await hello(id, d);
  return { status: h.status, check: d.check, message: 'message' in h ? h.message : '' };
}

const text = (v: unknown) => (typeof v === 'string' ? v : '');
const num = (v: unknown) => (typeof v === 'number' && Number.isFinite(v) ? v : 0);
const nums = (v: unknown) => (Array.isArray(v) ? v.map(num) : []);
const pct = (s: string) => { const n = parseFloat(s); return Number.isFinite(n) ? n : 0; };
/** "2.1 GB", "310 MB", "12 KB" in bytes, to compare rows by memory. */
const bytes = (s: string) => {
  const m = /^([\d.]+)\s*(KB|MB|GB|TB|B)?/.exec(s.trim());
  if (!m) return 0;
  const scale: Record<string, number> = { B: 1, KB: 1e3, MB: 1e6, GB: 1e9, TB: 1e12 };
  return parseFloat(m[1]) * (scale[m[2] ?? 'B'] ?? 1);
};

const f = (n: number) => String(Math.round(n * 100) / 100);
const x = (i: number, n: number) => ((SLOTS - n + i) * GRAPH_W) / (SLOTS - 1);
const y = (v: number) => GRAPH_H * (1 - Math.min(100, Math.max(0, v)) / 100);
/** One reading is a sliver at the right edge, not a point. */
const widen = (v: number[]) => (v.length === 1 ? [v[0], v[0]] : v);
const xs = (n: number) => Array.from({ length: n }, (_, i) => x(i, n));

/** The band between `bottom` and `top` (percentages, oldest first), newest at the right. */
function area(top: number[], bottom: number[]): string {
  if (top.length === 0) return '';
  const t = widen(top), b = widen(bottom.length ? bottom : top.map(() => 0));
  const at = xs(t.length);
  const upper = at.map((px, i) => `${f(px)} ${f(y(t[i]))}`);
  const lower = at.map((px, i) => `${f(px)} ${f(y(b[i]))}`).reverse();
  return `M ${upper.join(' L ')} L ${lower.join(' L ')} Z`;
}

function line(values: number[]): string {
  if (values.length === 0) return '';
  const v = widen(values);
  return `M ${xs(v.length).map((px, i) => `${f(px)} ${f(y(v[i]))}`).join(' L ')}`;
}

const proc = (p: Record<string, unknown>): Proc => ({
  pid: num(p.pid), name: text(p.name), detail: text(p.detail), cpu: text(p.cpu), memory: text(p.memory),
  group: p.group === true, count: num(p.count), app: text(p.app), mine: p.mine === true, bar: 0,
});

/** Each row's bar: its share of the busiest row's, by what the list is sorted by. */
function withBars(rows: Proc[], sort: string): Proc[] {
  const value = (p: Proc) => (sort === 'memory' ? bytes(p.memory) : pct(p.cpu));
  const top = Math.max(0, ...rows.map(value));
  return rows.map((p) => ({ ...p, bar: top > 0 ? Math.min(1, value(p) / top) : 0 }));
}

const away = (name: string) => `Make sure ${name} is awake, on the same Wi‑Fi, and sharing in Magnitor’s Settings.`;
const nameOf = (id: string) => id.slice(id.indexOf(':') + 1);

/** The Mac's answer to `path`, signed by this phone, or why there is none: "offline" with a
 * message, or "code" when the Mac no longer knows this phone. A read of a Bonjour Mac that fails
 * is asked once more at its address resolved again; a POST isn't, as it may have reached the Mac.
 * A stale session is opened again and the request sent once more: the Mac refused it unread. */
async function ask(id: string, store: Store, path: string, method = 'GET'): Promise<{ json: Record<string, unknown> } | { status: string; message: string }> {
  const name = nameOf(id);
  let d: Device;
  try { d = await myDevice(store); } catch { return { status: 'offline', message: 'This iPhone couldn’t make its key. Restart Magnitor Remote and try again.' }; }
  const send = async (fresh: boolean) => {
    const s = sessions.get(id)!;
    const url = new URL(`${await where(id, fresh)}${path}`);
    const seq = (s.seq += 1);
    const sig = await sign(d.pair.privateKey, signedText(s.challenge, seq, method, url));
    return fetch(url.href, { method, headers: { authorization: `Magnitor id=${d.id},ch=${s.challenge},seq=${seq},sig=${sig}` } });
  };
  let r: Response | null = null;
  for (let attempt = 0; r === null; attempt++) {
    if (!sessions.has(id)) {
      const h = await hello(id, d);
      if (h.status === 'offline') return h as { status: string; message: string };
      if (h.status !== 'paired') return { status: 'code', message: '' };
    }
    try {
      r = await send(false);
    } catch {
      resolved.delete(id);
      if (method !== 'GET' || !id.startsWith('svc:')) return { status: 'offline', message: away(name) };
      try {
        r = await send(true);
      } catch {
        resolved.delete(id);
        return { status: 'offline', message: away(name) };
      }
    }
    if (r.status === 401) {
      const j = (await r.json().catch(() => ({}))) as Record<string, unknown>;
      sessions.delete(id);
      if (j.reason === 'stale' && attempt === 0) r = null;
      else if (j.reason === 'signature') return { status: 'offline', message: `${name} didn’t accept this iPhone’s signature. Pair it again.` };
      else return { status: 'code', message: '' };
    }
  }
  if (r.status === 429) return { status: 'offline', message: LOCKED };
  if (r.status === 403) return { status: 'off', message: `Turn on iPhone › Allow Control from iPhone in Magnitor’s Settings on ${name}.` };
  if (r.status === 404 || r.status === 405) return { status: 'offline', message: `Update Magnitor on ${name} to use this.` };
  if (!r.ok) return { status: 'offline', message: `${name} answered ${r.status}. Update Magnitor on your Mac and try again.` };
  return { json: (await r.json()) as Record<string, unknown> };
}

async function activity(id: string, store: Store, sort: string, query: string): Promise<Activity> {
  const by = sort === 'memory' ? 'memory' : 'cpu';
  const a = await ask(id, store, `/v1/activity?sort=${by}&q=${enc(query.trim())}`);
  if (!('json' in a)) return blank(a.status, a.message);
  const j = a.json;
  if (text(j.error) !== '') return blank('offline', text(j.error));
  const user = nums(j.userHistory), system = nums(j.systemHistory), memory = nums(j.memoryHistory);
  const total = user.map((u, i) => u + (system[i] ?? 0));
  const cpuUser = text(j.cpuUser), cpuSystem = text(j.cpuSystem);
  return {
    name: text(j.name) || nameOf(id),
    cpuUser,
    cpuSystem,
    cpuIdle: text(j.cpuIdle),
    cpuLoad: Math.round((pct(cpuUser) + pct(cpuSystem)) * 10) / 10,
    cpuPeak: Math.round(Math.max(0, ...total)),
    cpuAverage: total.length ? Math.round(total.reduce((s, v) => s + v, 0) / total.length) : 0,
    cpuSystemArea: area(system, []),
    cpuUserArea: area(total, system),
    cpuLine: line(total),
    memoryArea: area(memory, []),
    memoryLine: line(memory),
    memoryPeak: Math.round(Math.max(0, ...memory)),
    memoryUsed: text(j.memoryUsed),
    memoryTotal: text(j.memoryTotal),
    memoryShare: num(j.memoryShare),
    cached: text(j.cached),
    compressed: text(j.compressed),
    swap: text(j.swap),
    pressure: text(j.pressure),
    thermal: text(j.thermal),
    temperature: text(j.temperature),
    count: num(j.count),
    more: num(j.more),
    matchCount: num(j.matchCount),
    processes: withBars((Array.isArray(j.processes) ? (j.processes as Record<string, unknown>[]) : []).map(proc), by),
    status: '',
    message: '',
  };
}

/** A process as the inspector shows it, or an app's processes together when `app` is given. */
async function detail(id: string, store: Store, pid: number, segment: string): Promise<Detail> {
  // The app's bundle path comes from its route's segment (`procPath`), which may still be encoded.
  let app = segment;
  try { if (app.includes('%')) app = decodeURIComponent(app); } catch { /* a path with a literal % */ }
  const a = await ask(id, store, app !== '' ? `/v1/app?app=${enc(app)}` : `/v1/process?pid=${pid}`);
  if (!('json' in a)) return blankDetail(a.status, a.message);
  const j = a.json;
  if (app !== '') {
    return {
      ...blankDetail('', ''),
      found: j.found === true, group: true, pid: num(j.main), name: text(j.name), path: text(j.app), cpu: text(j.cpu),
      memory: text(j.memory), count: num(j.count), mine: j.mine === true, own: j.own === true,
      processes: withBars((Array.isArray(j.processes) ? (j.processes as Record<string, unknown>[]) : []).map(proc), 'cpu'),
    };
  }
  return {
    ...blankDetail('', ''),
    found: j.found === true, pid: num(j.pid), name: text(j.name), path: text(j.path), user: text(j.user), parent: text(j.parent),
    cpu: text(j.cpu), cpuTime: text(j.cpuTime), threads: text(j.threads), memory: text(j.memory), realMemory: text(j.realMemory),
    running: text(j.running), count: 1, mine: j.mine === true, own: j.own === true,
  };
}

/** Quit or Force Quit: a process by its pid, or an app (every process of it) by its bundle. */
async function end(id: string, store: Store, pid: number, app: string, force: boolean): Promise<Ended> {
  const path = app !== ''
    ? `/v1/end-app?app=${enc(app)}&how=${force ? 'force' : 'quit'}`
    : `/v1/end?pid=${pid}&force=${force ? 1 : 0}`;
  const a = await ask(id, store, path, 'POST');
  if (!('json' in a)) return { ok: false, message: a.status === 'code' ? 'This iPhone is no longer paired with the Mac. Pair it again in Settings.' : a.message };
  return { ok: a.json.ok === true, message: text(a.json.message) };
}

const blankControls = (status: string, message: string): Controls => ({ ready: false, hasVolume: false, volume: 0, muted: false, apps: [], status, message });

/** The Mac's sound and the apps it can open, once control is allowed on the Mac ("off" else). */
async function controls(id: string, store: Store): Promise<Controls> {
  const a = await ask(id, store, '/v1/controls');
  if (!('json' in a)) return blankControls(a.status, a.status === 'code' ? 'This iPhone is no longer paired with the Mac. Go back and pair it again.' : a.message);
  const j = a.json;
  const apps = (Array.isArray(j.apps) ? (j.apps as Record<string, unknown>[]) : []).map((x) => ({ name: text(x.name), app: text(x.app), stamp: num(x.stamp) }));
  return { ready: true, hasVolume: j.hasVolume === true, volume: num(j.volume), muted: j.muted === true, apps, status: '', message: '' };
}

const ICONS = 'app:/cache/icons';
const INDEX = `${ICONS}/index.json`;
/** Icons asked for in one request (share.rs's `ICONS_MAX`). */
const ICON_BATCH = 12;

/** FNV-1a of `s`, as hex: an app's file name from its bundle's path. */
function fnv(s: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 0x01000193) >>> 0;
  return h.toString(16).padStart(8, '0');
}
const iconFile = (app: string, stamp: number) => `${ICONS}/${fnv(app)}-${stamp}.png`;

const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
function unbase64(s: string): Uint8Array {
  const clean = s.replace(/[^A-Za-z0-9+/]/g, '');
  const out = new Uint8Array(Math.floor((clean.length * 3) / 4));
  let bits = 0, acc = 0, n = 0;
  for (const ch of clean) {
    acc = (acc << 6) | B64.indexOf(ch);
    bits += 6;
    if (bits >= 8) { bits -= 8; out[n++] = (acc >> bits) & 0xff; }
  }
  return out.subarray(0, n);
}

/** The icon files written so far (`iconFile`'s paths); none before the first. */
async function readIndex(storage: Storage): Promise<Set<string>> {
  try {
    const list = JSON.parse(new TextDecoder().decode(await storage.fs.readFile(INDEX)));
    return new Set(Array.isArray(list) ? list.filter((x): x is string => typeof x === 'string') : []);
  } catch {
    return new Set();
  }
}

/** Each app's icon file, "" for one the Mac has none for (yet). `apps` are "<stamp> <path>". The
 * ones not kept are asked for a dozen at a time; a Mac that can't answer leaves the rest blank. */
async function appIcons(id: string, store: Store, apps: string[], storage: Storage): Promise<AppIcon[]> {
  const wanted = apps.map((a) => ({ app: a.slice(a.indexOf(' ') + 1), stamp: Number(a.slice(0, a.indexOf(' '))) || 0 }));
  const kept = await readIndex(storage);
  const missing = wanted.filter((w) => !kept.has(iconFile(w.app, w.stamp)));
  if (missing.length > 0) {
    try { await storage.fs.mkdir(ICONS); } catch { /* already there */ }
    let wrote = false;
    for (let i = 0; i < missing.length; i += ICON_BATCH) {
      const batch = missing.slice(i, i + ICON_BATCH);
      const a = await ask(id, store, `/v1/icons?apps=${enc(batch.map((b) => b.app).join('\n'))}`);
      if (!('json' in a)) break;
      for (const icon of Array.isArray(a.json.icons) ? (a.json.icons as Record<string, unknown>[]) : []) {
        const w = batch.find((b) => b.app === text(icon.app));
        if (!w || text(icon.png) === '') continue;
        const file = iconFile(w.app, w.stamp);
        await storage.fs.atomicWriteFile(file, unbase64(text(icon.png)));
        kept.add(file);
        wrote = true;
      }
    }
    if (wrote) await storage.fs.atomicWriteFile(INDEX, new TextEncoder().encode(JSON.stringify([...kept])));
  }
  return wanted.map((w) => ({ app: w.app, icon: kept.has(iconFile(w.app, w.stamp)) ? iconFile(w.app, w.stamp) : '' }));
}

/** One control: "volume" (`value` 0–100), "mute" ("1" or "0"), "open" (an app's bundle path),
 * "display-off" or "lock". */
async function control(id: string, store: Store, what: string, value: string): Promise<Ended> {
  const query = what === 'volume' ? `?level=${Math.round(Math.min(100, Math.max(0, Number(value) || 0)))}`
    : what === 'mute' ? `?on=${value === '1' ? 1 : 0}`
    : what === 'open' ? `?app=${enc(value)}`
    : '';
  const a = await ask(id, store, `/v1/${what}${query}`, 'POST');
  if (!('json' in a)) return { ok: false, message: a.status === 'code' ? 'This iPhone is no longer paired with the Mac. Pair it again in Settings.' : a.message };
  return { ok: a.json.ok === true, message: text(a.json.message) };
}

/** The trackpad's ops not yet sent to each Mac, the request carrying the last ones, scroll's
 * fractions of a pixel, and when a request last failed. */
type Op = { k: 'm' | 's'; x: number; y: number } | { k: 'c:l' | 'c:r' | 'd' | 'u' };
const pads = new Map<string, { ops: Op[]; flight: Promise<Ended> | null; carry: [number, number]; failed: { skip: number; result: Ended } | null }>();
/** After a refusal, this many moves (about a second of a moving finger) are dropped before the
 * Mac is asked again, so one without access isn't asked sixty times a second. */
const PAUSE = 60;
/** Pixels of scroll on the Mac for each point the fingers move. */
const SCROLL = 1.6;
const fixed = (n: number) => String(Math.round(n * 100) / 100);

/** A move's gain: `speed` for a slow finger, up to three times it for a quick one, as a
 * trackpad accelerates. */
const gain = (dx: number, dy: number, speed: number) => speed * (1 + Math.min(Math.hypot(dx, dy), 24) / 12);

/** `ops` as the Mac reads them (share.rs's `/v1/pointer`), scroll rounded to whole pixels with
 * the rest kept for the next request. */
function opsText(ops: Op[], carry: [number, number]): string {
  return ops.map((o) => {
    if (o.k === 'm') return `m:${fixed(o.x)},${fixed(o.y)}`;
    if (o.k === 's') {
      const x = o.x + carry[0], y = o.y + carry[1];
      const rx = Math.trunc(x), ry = Math.trunc(y);
      carry[0] = x - rx; carry[1] = y - ry;
      return rx === 0 && ry === 0 ? '' : `s:${rx},${ry}`;
    }
    return o.k;
  }).filter((t) => t !== '').join(';');
}

/** Sends what gathered while the last request was out, until nothing is left: a quick finger's
 * moves go together, one request a round trip. The flag drops in the same turn the last check
 * finds nothing, so an op pushed after it starts a request of its own. */
async function pump(id: string, store: Store, pad: NonNullable<ReturnType<typeof pads.get>>): Promise<Ended> {
  let result: Ended = { ok: true, message: '' };
  try {
    while (pad.ops.length > 0) {
      const body = opsText(pad.ops.splice(0), pad.carry);
      if (body === '') continue;
      const a = await ask(id, store, `/v1/pointer?ops=${enc(body)}`, 'POST');
      result = 'json' in a
        ? { ok: a.json.ok === true, message: text(a.json.message) }
        : { ok: false, message: a.status === 'code' ? 'This iPhone is no longer paired with the Mac. Pair it again in Settings.' : a.message };
      if (!result.ok) {
        pad.ops.length = 0;
        pad.failed = { skip: PAUSE, result };
        break;
      }
      pad.failed = null;
    }
  } catch (e) {
    result = { ok: false, message: e instanceof Error ? e.message : String(e) };
  } finally {
    pad.flight = null;
  }
  return result;
}

/** One thing the trackpad did: "move" or "scroll" by the finger's points, "left" or "right" (a
 * click), "down" or "up" (the left button held, for a drag). */
function pointer(id: string, store: Store, what: string, dx: number, dy: number, speed: number): Promise<Ended> {
  let pad = pads.get(id);
  if (!pad) pads.set(id, (pad = { ops: [], flight: null, carry: [0, 0], failed: null }));
  // While the Mac refuses, moves are dropped; a press asks again at once.
  if (pad.failed && pad.failed.skip > 0 && (what === 'move' || what === 'scroll')) {
    pad.failed.skip -= 1;
    return Promise.resolve(pad.failed.result);
  }
  const last = pad.ops.at(-1);
  if (what === 'move' || what === 'scroll') {
    const k = what === 'move' ? 'm' : 's';
    const g = k === 'm' ? gain(dx, dy, speed) : SCROLL;
    if (last && last.k === k) { last.x += dx * g; last.y += dy * g; }
    else pad.ops.push({ k, x: dx * g, y: dy * g });
  } else {
    const k = ({ left: 'c:l', right: 'c:r', down: 'd', up: 'u' } as const)[what as 'left'];
    if (!k) return Promise.resolve({ ok: false, message: `Unknown pointer action “${what}”.` });
    pad.ops.push({ k });
  }
  // Started a turn later, so the flag is set before the pump can drop it.
  const p = pad;
  return p.flight ?? (p.flight = Promise.resolve().then(() => pump(id, store, p)));
}

// `code` marks a Mac as paired here (the view's `pairings`); requests are signed with this
// phone's key, which `store` keeps.
const sources: Sources = {
  activity: ([id, _code, sort, query], store) => activity(id, store, sort, query),
  detail: ([id, _code, pid, app], store) => detail(id, store, pid, app),
  endProcess: ([id, _code, pid, app, force], store) => end(id, store, pid, app, force),
  controls: ([id], store) => controls(id, store),
  appIcons: ([id, _code, apps], store, storage) => appIcons(id, store, apps, storage),
  control: ([id, _code, what, value], store) => control(id, store, what, value),
  pointer: ([id, _code, what, dx, dy, speed], store) => pointer(id, store, what, dx, dy, speed),
  pairMac: ([id, code], store) => pairMac(id, code, store),
  pairState: ([id], store) => pairState(id, store),
};
export const answer: Answer = (source, args, store, storage, native) => sources[source](args, store, storage, native);
