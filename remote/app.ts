// Magnitor Remote's data module: a Mac's Activity Monitor, asked of Magnitor on that Mac
// (Magnitor's data/src/share.rs) over the local network. A Mac Bonjour found is resolved each time
// (its address can change); a typed one is used as written, on Magnitor's port unless it names one.
// A refused code and an unreachable Mac are answers the screen shows, not failures, as is control
// the Mac hasn't allowed ("off").
//
// The Mac sends its last 40 readings of CPU and memory as percentages; the graphs' paths are drawn
// here, in a 100 × 40 box the view stretches to its width (`GRAPH_W`, `GRAPH_H`).
import type { Answer, Result, Sources } from './app.contract.d.ts';
import { resolveService } from '@exact/local-network';

export const grants = [
  'device.local-network purpose.localNetwork',
  'net.dns-sd _magnitor._tcp',
  'net.fetch local-network',
].join('\n');

type Activity = Result<'activity'>;
type Proc = Activity['processes'][number];
type Detail = Result<'detail'>;
type Ended = Result<'endProcess'>;
type Controls = Result<'controls'>;
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

/** A Mac Bonjour resolved, for the trackpad, which asks many times a second; forgotten when a
 * request to it fails. */
const resolved = new Map<string, string>();

/** Where a Mac is: its typed address, else its resolved host and port (`kept`: as last resolved). */
async function where(id: string, kept = false): Promise<string> {
  if (id.startsWith('addr:')) {
    const typed = id.slice(5).replace(/^https?:\/\//, '').replace(/\/+$/, '');
    return `http://${/:\d+$/.test(typed) ? typed : `${typed}:${PORT}`}`;
  }
  const known = resolved.get(id);
  if (kept && known) return known;
  const s = await resolveService({ name: id.slice(4), type: TYPE });
  const host = s.host.includes(':') && !s.host.startsWith('[') ? `[${s.host}]` : s.host;
  const url = `http://${host}:${s.port}`;
  resolved.set(id, url);
  return url;
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

/** The Mac's answer to `path`, or why there is none: "offline" with a message, or "code". */
async function ask(id: string, code: string, path: string, method = 'GET', kept = false): Promise<{ json: Record<string, unknown> } | { status: string; message: string }> {
  const name = nameOf(id);
  let r: Response;
  try {
    r = await fetch(`${await where(id, kept)}${path}`, { method, headers: { authorization: `Bearer ${code}` } });
  } catch {
    resolved.delete(id);
    return { status: 'offline', message: away(name) };
  }
  if (r.status === 401) return { status: 'code', message: '' };
  if (r.status === 403) return { status: 'off', message: `Turn on iPhone › Allow Control from iPhone in Magnitor’s Settings on ${name}.` };
  if (r.status === 404 || r.status === 405) return { status: 'offline', message: `Update Magnitor on ${name} to use this.` };
  if (!r.ok) return { status: 'offline', message: `${name} answered ${r.status}. Update Magnitor on your Mac and try again.` };
  return { json: (await r.json()) as Record<string, unknown> };
}

async function activity(id: string, code: string, sort: string, query: string): Promise<Activity> {
  const by = sort === 'memory' ? 'memory' : 'cpu';
  const a = await ask(id, code, `/v1/activity?sort=${by}&q=${encodeURIComponent(query.trim())}`);
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
async function detail(id: string, code: string, pid: number, segment: string): Promise<Detail> {
  // The app's bundle path comes from its route's segment (`procPath`), which may still be encoded.
  let app = segment;
  try { if (app.includes('%')) app = decodeURIComponent(app); } catch { /* a path with a literal % */ }
  const a = await ask(id, code, app !== '' ? `/v1/app?app=${encodeURIComponent(app)}` : `/v1/process?pid=${pid}`);
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
async function end(id: string, code: string, pid: number, app: string, force: boolean): Promise<Ended> {
  const path = app !== ''
    ? `/v1/end-app?app=${encodeURIComponent(app)}&how=${force ? 'force' : 'quit'}`
    : `/v1/end?pid=${pid}&force=${force ? 1 : 0}`;
  const a = await ask(id, code, path, 'POST');
  if (!('json' in a)) return { ok: false, message: a.status === 'code' ? 'The pairing code no longer matches.' : a.message };
  return { ok: a.json.ok === true, message: text(a.json.message) };
}

const blankControls = (status: string, message: string): Controls => ({ ready: false, hasVolume: false, volume: 0, muted: false, apps: [], status, message });

/** The Mac's sound and the apps it can open, once control is allowed on the Mac ("off" else). */
async function controls(id: string, code: string): Promise<Controls> {
  const a = await ask(id, code, '/v1/controls');
  if (!('json' in a)) return blankControls(a.status, a.status === 'code' ? 'The pairing code no longer matches. Go back and pair again.' : a.message);
  const j = a.json;
  const apps = (Array.isArray(j.apps) ? (j.apps as Record<string, unknown>[]) : []).map((x) => ({ name: text(x.name), app: text(x.app) }));
  return { ready: true, hasVolume: j.hasVolume === true, volume: num(j.volume), muted: j.muted === true, apps, status: '', message: '' };
}

/** One control: "volume" (`value` 0–100), "mute" ("1" or "0"), "open" (an app's bundle path),
 * "display-off" or "lock". */
async function control(id: string, code: string, what: string, value: string): Promise<Ended> {
  const query = what === 'volume' ? `?level=${Math.round(Math.min(100, Math.max(0, Number(value) || 0)))}`
    : what === 'mute' ? `?on=${value === '1' ? 1 : 0}`
    : what === 'open' ? `?app=${encodeURIComponent(value)}`
    : '';
  const a = await ask(id, code, `/v1/${what}${query}`, 'POST');
  if (!('json' in a)) return { ok: false, message: a.status === 'code' ? 'The pairing code no longer matches.' : a.message };
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
async function pump(id: string, code: string, pad: NonNullable<ReturnType<typeof pads.get>>): Promise<Ended> {
  let result: Ended = { ok: true, message: '' };
  try {
    while (pad.ops.length > 0) {
      const body = opsText(pad.ops.splice(0), pad.carry);
      if (body === '') continue;
      const a = await ask(id, code, `/v1/pointer?ops=${encodeURIComponent(body)}`, 'POST', true);
      result = 'json' in a
        ? { ok: a.json.ok === true, message: text(a.json.message) }
        : { ok: false, message: a.status === 'code' ? 'The pairing code no longer matches.' : a.message };
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
function pointer(id: string, code: string, what: string, dx: number, dy: number, speed: number): Promise<Ended> {
  const key = `${id} ${code}`;
  let pad = pads.get(key);
  if (!pad) pads.set(key, (pad = { ops: [], flight: null, carry: [0, 0], failed: null }));
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
  return p.flight ?? (p.flight = Promise.resolve().then(() => pump(id, code, p)));
}

const sources: Sources = {
  activity: ([id, code, sort, query]) => activity(id, code, sort, query),
  detail: ([id, code, pid, app]) => detail(id, code, pid, app),
  endProcess: ([id, code, pid, app, force]) => end(id, code, pid, app, force),
  controls: ([id, code]) => controls(id, code),
  control: ([id, code, what, value]) => control(id, code, what, value),
  pointer: ([id, code, what, dx, dy, speed]) => pointer(id, code, what, dx, dy, speed),
};
export const answer: Answer = (source, args, store, storage, native) => sources[source](args, store, storage, native);
