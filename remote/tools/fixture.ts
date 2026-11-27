// Stands in for Magnitor's sharing (Magnitor's data/src/share.rs) while the tests run:
//   bun tools/fixture.ts   (127.0.0.1:47899, code TEST-CODE)
// Pairing and signing are share.rs's (data/src/guard.rs): a phone proves a code (`/v1/pair`), says
// hello for its session's challenge (`/v1/hello`), and signs every other request with its key.
// Unlike the Mac, its codes work again and again, so every test can pair: TEST-CODE and VIEW-ONLY
// pair at once (as for a phone the Mac already allows); WAIT-CODE waits until the phone's second
// hello, then is allowed; DENY-CODE is refused at its second hello. A phone keeps the control of
// the code it last paired with.
// Its Mac, "Studio", has three processes; ?sort=memory puts Docker first, ?q= searches by name.
// A POST to /v1/end or /v1/end-app quits one for ten seconds (then it is back, so the suite can
// run again against the same fixture); launchd (1) refuses.
// Its controls (/v1/controls and what it lists) answer TEST-CODE; VIEW-ONLY reads Activity Monitor
// but finds control off (403), as a Mac whose Settings don't allow it. Opening an app it doesn't
// list fails, as the Mac refuses any other path. Its pointer starts at 500,400 on a 1000 × 800
// screen; /v1/pointer answers with where it is and what it did ("at 520,410 · left 1 · right 0 ·
// scrolled 0,0 · up"), which only the tests read, as the Mac answers "".
// /v1/icons answers a tile in each app's colour (a 144-pixel PNG, as the Mac's are), except for
// Legacy Tool, which has none, as an app whose icon couldn't be drawn.
import { createHash, randomBytes } from 'node:crypto';
import { deflateSync } from 'node:zlib';

const sha = (b: Uint8Array | string) => createHash('sha256').update(b).digest();
const fromB64url = (s: string) => new Uint8Array(Buffer.from(s.replace(/-/g, '+').replace(/_/g, '/'), 'base64'));
/** A key's id and its six digits, as guard.rs's `device_id` and `check_digits`. */
function idOf(x: string, y: string) {
  const h = sha(new Uint8Array([4, ...fromB64url(x), ...fromB64url(y)]));
  const n = h.readUInt32BE(8) % 1_000_000;
  return { id: h.subarray(0, 8).toString('hex'), check: `${String(Math.floor(n / 1000)).padStart(3, '0')} ${String(n % 1000).padStart(3, '0')}` };
}
type Phone = { key: CryptoKey; control: boolean };
const phones = new Map<string, Phone>();
const waiting = new Map<string, { hellos: number; allow: boolean; x: string; y: string }>();
const refused = new Set<string>();
const sessions = new Map<string, { challenge: string; top: number; seen: Set<number> }>();
const CODES: Record<string, { control: boolean; wait?: boolean; allow?: boolean }> = {
  TESTCODE: { control: true }, VIEWONLY: { control: false }, WAITCODE: { control: true, wait: true, allow: true }, DENYCODE: { control: true, wait: true, allow: false },
};
const keyOf = (x: string, y: string) => crypto.subtle.importKey('jwk', { kty: 'EC', crv: 'P-256', x, y }, { name: 'ECDSA', namedCurve: 'P-256' }, true, ['verify']);
async function pair(q: URLSearchParams): Promise<Response> {
  const x = q.get('x') ?? '', y = q.get('y') ?? '', proof = q.get('proof') ?? '';
  const code = Object.keys(CODES).find((c) => sha(`magnitor-pair\n${c}\n${x}\n${y}`).toString('hex') === proof);
  if (!code) return Response.json({ status: 'code', error: 'That code doesn’t match the one in Magnitor’s Settings on your Mac.' }, { status: 401 });
  const { id, check } = idOf(x, y);
  const c = CODES[code];
  refused.delete(id);
  if (c.wait) {
    phones.delete(id);
    waiting.set(id, { hellos: 0, allow: c.allow === true, x, y });
    return Response.json({ status: 'pending', check });
  }
  phones.set(id, { key: await keyOf(x, y), control: c.control });
  return Response.json({ status: 'paired', check });
}
async function hello(id: string): Promise<Response> {
  const w = waiting.get(id);
  if (w && ++w.hellos >= 2) {
    waiting.delete(id);
    if (w.allow) phones.set(id, { key: await keyOf(w.x, w.y), control: true });
    else refused.add(id);
  } else if (w) return Response.json({ status: 'pending', check: idOf(w.x, w.y).check });
  if (!phones.has(id)) return Response.json({ status: refused.has(id) ? 'denied' : 'unpaired' });
  let s = sessions.get(id);
  if (!s) sessions.set(id, (s = { challenge: randomBytes(16).toString('hex'), top: 0, seen: new Set() }));
  return Response.json({ status: 'paired', challenge: s.challenge, seq: s.top });
}
/** The phone that signed `req`, or the refusal share.rs answers. */
async function signer(req: Request, url: URL): Promise<Phone | Response> {
  const fields = Object.fromEntries((req.headers.get('authorization') ?? '').replace(/^Magnitor /, '').split(',').map((f) => f.trim().split('=') as [string, string]));
  const phone = phones.get(fields.id ?? '');
  if (!phone) return Response.json({ reason: 'unpaired', error: 'This iPhone isn’t paired with this Mac.' }, { status: 401 });
  const s = sessions.get(fields.id), seq = Number(fields.seq);
  if (!s || fields.ch !== s.challenge || !(seq > 0) || s.seen.has(seq) || seq <= s.top - 64) return Response.json({ reason: 'stale', error: 'The session ended. Try again.' }, { status: 401 });
  const signed = `magnitor-v1\n${s.challenge}\n${seq}\n${req.method}\n${url.pathname}?${url.search.slice(1)}`;
  const ok = await crypto.subtle.verify({ name: 'ECDSA', hash: 'SHA-256' }, phone.key, fromB64url(fields.sig ?? ''), new TextEncoder().encode(signed));
  if (!ok) return Response.json({ reason: 'signature', error: 'This request wasn’t signed by a paired iPhone.' }, { status: 401 });
  s.seen.add(seq);
  s.top = Math.max(s.top, seq);
  return phone;
}

const all = [
  { pid: 501, name: 'Xcode', detail: '6 processes', cpu: '48.2', memory: '2.1 GB', group: true, count: 6, app: '/Applications/Xcode.app', mine: true },
  { pid: 502, name: 'Docker', detail: '4 processes', cpu: '12.0', memory: '3.4 GB', group: true, count: 4, app: '/Applications/Docker.app', mine: true },
  { pid: 503, name: 'node', detail: '503', cpu: '3.1', memory: '310 MB', group: false, count: 1, app: '', mine: true },
  { pid: 1, name: 'launchd', detail: '1 · root', cpu: '0.2', memory: '12 MB', group: false, count: 1, app: '', mine: false },
];
const quitAt = new Map<number, number>();
const apps = [
  { name: 'Calendar', app: '/System/Applications/Calendar.app', stamp: 1700000000, color: [235, 77, 61] },
  { name: 'Legacy Tool', app: '/Applications/Legacy Tool.app', stamp: 1700000000, color: [0, 0, 0] },
  { name: 'Mail', app: '/System/Applications/Mail.app', stamp: 1700000000, color: [52, 120, 246] },
  { name: 'Maps', app: '/System/Applications/Maps.app', stamp: 1700000000, color: [101, 196, 102] },
  { name: 'Music', app: '/System/Applications/Music.app', stamp: 1700000000, color: [250, 45, 85] },
  { name: 'Notes', app: '/System/Applications/Notes.app', stamp: 1700000000, color: [247, 206, 70] },
  { name: 'Photos', app: '/System/Applications/Photos.app', stamp: 1700000000, color: [240, 145, 60] },
  { name: 'Safari', app: '/Applications/Safari.app', stamp: 1700000000, color: [30, 144, 255] },
  { name: 'Terminal', app: '/System/Applications/Utilities/Terminal.app', stamp: 1700000000, color: [40, 40, 40] },
  { name: 'Xcode', app: '/Applications/Xcode.app', stamp: 1700000000, color: [20, 110, 230] },
];

/** A `size`-pixel PNG: a rounded square in `rgb`, lighter at its top, on transparency. */
function tile(rgb: number[], size = 144): Uint8Array {
  const crcTable = Array.from({ length: 256 }, (_, n) => { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; return c >>> 0; });
  const crc = (b: Uint8Array) => { let c = 0xffffffff; for (const x of b) c = crcTable[(c ^ x) & 0xff] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; };
  const chunk = (type: string, data: Uint8Array) => {
    const out = new Uint8Array(12 + data.length), v = new DataView(out.buffer);
    v.setUint32(0, data.length); out.set(new TextEncoder().encode(type), 4); out.set(data, 8);
    v.setUint32(8 + data.length, crc(out.subarray(4, 8 + data.length)));
    return out;
  };
  const raw = new Uint8Array(size * (size * 4 + 1));
  const r = size * 0.225, inset = size * 0.08, lo = inset, hi = size - inset;
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0;
    for (let x = 0; x < size; x++) {
      const cx = Math.min(Math.max(x + 0.5, lo + r), hi - r), cy = Math.min(Math.max(y + 0.5, lo + r), hi - r);
      const inside = x + 0.5 >= lo && x + 0.5 <= hi && y + 0.5 >= lo && y + 0.5 <= hi && Math.hypot(x + 0.5 - cx, y + 0.5 - cy) <= r;
      const light = 1.25 - 0.4 * (y / size);
      const o = y * (size * 4 + 1) + 1 + x * 4;
      raw.set(inside ? [...rgb.map((c) => Math.min(255, Math.round(c * light))), 255] : [0, 0, 0, 0], o);
    }
  }
  const head = new Uint8Array(13), hv = new DataView(head.buffer);
  hv.setUint32(0, size); hv.setUint32(4, size); head.set([8, 6, 0, 0, 0], 8);
  const parts = [new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', head), chunk('IDAT', deflateSync(raw, { level: 9 })), chunk('IEND', new Uint8Array())];
  const png = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  parts.reduce((at, p) => (png.set(p, at), at + p.length), 0);
  return png;
}
const tiles = new Map(apps.filter((a) => a.name !== 'Legacy Tool').map((a) => [a.app, Buffer.from(tile(a.color)).toString('base64')]));
const sound = { volume: 40, muted: false };
const CONTROLS = ['/v1/controls', '/v1/volume', '/v1/mute', '/v1/open', '/v1/display-off', '/v1/lock', '/v1/pointer', '/v1/icons'];
const mouse = { x: 500, y: 400, left: 0, right: 0, sx: 0, sy: 0, held: false };
const within = (v: number, max: number) => Math.min(max - 1, Math.max(0, v));
/** share.rs's `/v1/pointer`: the ops in order, or false when one is malformed. */
function point(ops: string): boolean {
  const steps = ops.split(';').filter(Boolean);
  if (steps.length === 0) return false;
  for (const op of steps) {
    const [k, v = ''] = op.split(':');
    const [a, b] = v.split(',').map(Number);
    if (k === 'm' && Number.isFinite(a) && Number.isFinite(b)) { mouse.x = within(mouse.x + a, 1000); mouse.y = within(mouse.y + b, 800); }
    else if (k === 's' && Number.isInteger(a) && Number.isInteger(b)) { mouse.sx += a; mouse.sy += b; }
    else if (op === 'c:l') mouse.left += 1;
    else if (op === 'c:r') mouse.right += 1;
    else if (op === 'd') { mouse.held = true; mouse.left += 1; }
    else if (op === 'u') mouse.held = false;
    else return false;
  }
  return true;
}
const where = () => `at ${Math.round(mouse.x)},${Math.round(mouse.y)} · left ${mouse.left} · right ${mouse.right} · scrolled ${mouse.sx},${mouse.sy} · ${mouse.held ? 'held' : 'up'}`;
const live = () => all.filter((p) => !(Date.now() - (quitAt.get(p.pid) ?? -Infinity) < 10_000));
const mb = (m: string) => parseFloat(m) * (m.endsWith('GB') ? 1000 : 1);
const history = (from: number, step: number) => Array.from({ length: 12 }, (_, i) => Math.round((from + i * step) * 10) / 10);
// The web host's page is another origin, so the fixture answers its preflight and allows it.
const CORS = { 'access-control-allow-origin': '*', 'access-control-allow-headers': 'authorization', 'access-control-allow-methods': 'GET, POST' };
Bun.serve({
  hostname: '127.0.0.1',
  port: 47899,
  async fetch(req) {
    if (req.method === 'OPTIONS') return new Response(null, { status: 204, headers: CORS });
    const r = await answer(req);
    for (const [k, v] of Object.entries(CORS)) r.headers.set(k, v);
    return r;
  },
});
async function answer(req: Request): Promise<Response> {
    const url = new URL(req.url);
    const q = url.searchParams;
    if (req.method === 'POST' && url.pathname === '/v1/pair') return pair(q);
    if (req.method === 'GET' && url.pathname === '/v1/hello') return hello(q.get('id') ?? '');
    const phone = await signer(req, url);
    if (phone instanceof Response) return phone;
    if (!phone.control && CONTROLS.includes(url.pathname)) return Response.json({ error: 'Control is off.' }, { status: 403 });
    const ended = (ok: boolean, message: string) => Response.json({ ok, message });
    switch (`${req.method} ${url.pathname}`) {
      case 'GET /v1/activity': {
        const found = live().filter((p) => p.name.toLowerCase().includes((q.get('q') ?? '').toLowerCase()));
        const sorted = q.get('sort') === 'memory' ? [...found].sort((a, b) => mb(b.memory) - mb(a.memory)) : found;
        return Response.json({
          name: 'Studio', cpuUser: '12.5', cpuSystem: '6.1', cpuIdle: '81.4',
          userHistory: history(8, 0.5), systemHistory: history(4, 0.2), memoryHistory: history(60, 0.5),
          memoryUsed: '21.3 GB', memoryTotal: '32 GB', memoryShare: 0.66, cached: '6.2 GB', compressed: '1.1 GB', swap: '0 KB',
          pressure: 'Normal', thermal: 'Fair', temperature: '52 °C', count: 312, more: 0, matchCount: q.get('q') ? found.length : 0,
          processes: sorted, error: '',
        });
      }
      case 'GET /v1/process': {
        const p = live().find((x) => x.pid === Number(q.get('pid')));
        if (!p) return Response.json({ pid: Number(q.get('pid')), found: false, name: '', path: '', user: '', parent: '', cpu: '', cpuTime: '', threads: '', memory: '', realMemory: '', running: '', mine: false, own: false });
        return Response.json({ pid: p.pid, found: true, name: p.name, path: `/usr/local/bin/${p.name}`, user: 'ada', parent: 'zsh (480)', cpu: p.cpu, cpuTime: '1:02.5', threads: '14', memory: p.memory, realMemory: '298 MB', running: '2 h 14 min', mine: p.mine, own: false });
      }
      case 'GET /v1/app': {
        const p = live().find((x) => x.app === q.get('app'));
        return Response.json({ app: q.get('app'), found: !!p, name: p?.name ?? '', cpu: p?.cpu ?? '', memory: p?.memory ?? '', count: p?.count ?? 0, main: p?.pid ?? 0, mine: true, own: false,
          processes: p ? [{ ...p, group: false, count: 1, detail: `process ${p.pid}` }, { pid: p.pid + 1000, name: `${p.name} Helper`, detail: `process ${p.pid + 1000}`, cpu: '1.0', memory: '80 MB', group: false, count: 1, app: p.app, mine: true }] : [] });
      }
      case 'POST /v1/end': {
        const pid = Number(q.get('pid'));
        if (pid <= 1) return ended(false, 'launchd is part of macOS and can’t be quit.');
        quitAt.set(pid, Date.now());
        return ended(true, q.get('force') === '1' ? `Force quit ${pid}.` : `Asked ${pid} to quit.`);
      }
      case 'GET /v1/controls':
        return Response.json({ hasVolume: true, volume: sound.volume, muted: sound.muted, apps: apps.map(({ name, app, stamp }) => ({ name, app, stamp })) });
      case 'GET /v1/icons': {
        // As share.rs: one a line, at most 12, only the apps listed.
        const asked = (q.get('apps') ?? '').split('\n').filter(Boolean);
        if (asked.length === 0 || asked.length > 12) return Response.json({ error: 'Bad request.' }, { status: 400 });
        const icons = apps.filter((a) => asked.includes(a.app) && tiles.has(a.app)).map((a) => ({ app: a.app, stamp: a.stamp, png: tiles.get(a.app) }));
        return Response.json({ icons });
      }
      case 'POST /v1/volume':
        sound.volume = Number(q.get('level'));
        return ended(true, `Volume ${sound.volume}%.`);
      case 'POST /v1/mute':
        sound.muted = q.get('on') === '1';
        return ended(true, sound.muted ? 'Muted.' : 'Unmuted.');
      case 'POST /v1/open': {
        const a = apps.find((x) => x.app === q.get('app'));
        return a ? ended(true, `Opened ${a.name}.`) : ended(false, 'Only apps in the Applications folders open from your iPhone.');
      }
      case 'POST /v1/display-off':
        return ended(true, 'The display is off.');
      case 'POST /v1/lock':
        return ended(true, 'Locked.');
      case 'POST /v1/pointer':
        if (!point(q.get('ops') ?? '')) return Response.json({ error: 'Bad request.' }, { status: 400 });
        return ended(true, where());
      case 'POST /v1/end-app': {
        const p = all.find((x) => x.app === q.get('app'));
        if (p) quitAt.set(p.pid, Date.now());
        return ended(true, `Asked ${p?.name} to quit.`);
      }
    }
    return Response.json({ error: 'Not found.' }, { status: 404 });
}
console.log('fixture: http://127.0.0.1:47899');
