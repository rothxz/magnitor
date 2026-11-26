// Renders tools/icon.svg to assets/icon-1024.png with headless Chrome: 1024 px, opaque (iOS
// passes over an icon with transparent pixels), full bleed (iOS rounds the corners).
//   bun tools/icon.ts
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const here = import.meta.dir;
const chrome = process.env.CHROME ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const dir = mkdtempSync(join(tmpdir(), 'icon-'));
const page = join(dir, 'icon.html');
writeFileSync(page, `<!doctype html><style>html,body{margin:0;background:#1E3A8A}svg{display:block}</style>${readFileSync(join(here, 'icon.svg'), 'utf8')}`);
const png = join(here, '..', 'assets', 'icon-1024.png');
const run = Bun.spawnSync([chrome, '--headless=new', '--disable-gpu', '--hide-scrollbars', '--force-device-scale-factor=1', '--window-size=1024,1024', `--user-data-dir=${join(dir, 'profile')}`, `--screenshot=${png}`, `file://${page}`]);
if (run.exitCode !== 0) throw new Error(run.stderr.toString());
// Chrome writes RGBA; the alpha is dropped so the file is opaque, as iOS asks.
const flat = Bun.spawnSync(['sips', '-s', 'format', 'jpeg', '-s', 'formatOptions', '100', png, '--out', join(dir, 'icon.jpg')]);
const back = Bun.spawnSync(['sips', '-s', 'format', 'png', join(dir, 'icon.jpg'), '--out', png]);
if (flat.exitCode !== 0 || back.exitCode !== 0) throw new Error('sips failed');
console.log(`wrote ${png}`);
