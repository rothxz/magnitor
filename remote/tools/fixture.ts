// Stands in for Magnitor's sharing (Magnitor's data/src/share.rs) while the tests run:
//   bun tools/fixture.ts   (127.0.0.1:47899, code TEST-CODE)
// Its Mac, "Studio", has three processes; ?sort=memory puts Docker first, ?q= searches by name.
// A POST to /v1/end or /v1/end-app quits one for ten seconds (then it is back, so the suite can
// run again against the same fixture); launchd (1) refuses.
// Its controls (/v1/controls and what it lists) answer TEST-CODE; VIEW-ONLY reads Activity Monitor
// but finds control off (403), as a Mac whose Settings don't allow it. Opening an app it doesn't
// list fails, as the Mac refuses any other path. Its pointer starts at 500,400 on a 1000 × 800
// screen; /v1/pointer answers with where it is and what it did ("at 520,410 · left 1 · right 0 ·
// scrolled 0,0 · up"), which only the tests read, as the Mac answers "".
const all = [
  { pid: 501, name: 'Xcode', detail: '6 processes', cpu: '48.2', memory: '2.1 GB', group: true, count: 6, app: '/Applications/Xcode.app', mine: true },
  { pid: 502, name: 'Docker', detail: '4 processes', cpu: '12.0', memory: '3.4 GB', group: true, count: 4, app: '/Applications/Docker.app', mine: true },
  { pid: 503, name: 'node', detail: 'process 503', cpu: '3.1', memory: '310 MB', group: false, count: 1, app: '', mine: true },
  { pid: 1, name: 'launchd', detail: 'process 1', cpu: '0.2', memory: '12 MB', group: false, count: 1, app: '', mine: false },
];
const quitAt = new Map<number, number>();
const apps = [
  { name: 'Notes', app: '/System/Applications/Notes.app' },
  { name: 'Safari', app: '/Applications/Safari.app' },
  { name: 'Xcode', app: '/Applications/Xcode.app' },
];
const sound = { volume: 40, muted: false };
const CONTROLS = ['/v1/controls', '/v1/volume', '/v1/mute', '/v1/open', '/v1/display-off', '/v1/lock', '/v1/pointer'];
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
Bun.serve({
  hostname: '127.0.0.1',
  port: 47899,
  fetch(req) {
    const url = new URL(req.url);
    const q = url.searchParams;
    const code = req.headers.get('authorization')?.replace(/^Bearer /, '').replace('-', '').toUpperCase();
    if (code !== 'TESTCODE' && code !== 'VIEWONLY') return Response.json({ error: 'code' }, { status: 401 });
    if (code === 'VIEWONLY' && CONTROLS.includes(url.pathname)) return Response.json({ error: 'Control is off.' }, { status: 403 });
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
        return Response.json({ hasVolume: true, volume: sound.volume, muted: sound.muted, apps });
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
  },
});
console.log('fixture: http://127.0.0.1:47899');
