// How Magnitor Remote proves itself to a Mac (Magnitor's data/src/guard.rs): the bytes each side
// must agree on, kept apart from app.ts so tools/interop.ts can check them against the Mac's Rust.
//
// A phone's P-256 public key is its JWK's `x` and `y` (base64url). The Mac knows it by `keyFacts`:
// an id (the first 8 bytes of the SHA-256 of `04‖x‖y`, in hex) and six digits the person compares
// on both screens. `pairProof` proves the pairing code without sending it; `signedText` is what
// each request's ES256 signature (raw r‖s, base64url) covers.

/** `encodeURIComponent`, and the five marks it leaves that a URL's query may encode on the way,
 * so the URL signed is the one sent. */
export const enc = (v: string) => encodeURIComponent(v).replace(/[!'()*]/g, (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`);
export const utf8 = (s: string) => new TextEncoder().encode(s);
export const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');

const B64URL = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';
export function base64url(b: Uint8Array): string {
  let out = '';
  for (let i = 0; i < b.length; i += 3) {
    const n = (b[i] << 16) | ((b[i + 1] ?? 0) << 8) | (b[i + 2] ?? 0);
    const chars = Math.min(4, Math.ceil(((b.length - i) * 8) / 6));
    for (let k = 0; k < chars; k++) out += B64URL[(n >> (18 - 6 * k)) & 63];
  }
  return out;
}
export function unbase64url(s: string): Uint8Array<ArrayBuffer> {
  const out: number[] = [];
  let acc = 0, bits = 0;
  for (const ch of s.replace(/=+$/, '')) {
    acc = (acc << 6) | B64URL.indexOf(ch.replace('+', '-').replace('/', '_'));
    bits += 6;
    if (bits >= 8) { bits -= 8; out.push((acc >> bits) & 0xff); }
  }
  return new Uint8Array(out);
}
export const sha256 = async (b: BufferSource) => new Uint8Array(await crypto.subtle.digest('SHA-256', b));

/** The key's id and its six digits, "482 913" (guard.rs's `device_id` and `check_digits`). */
export async function keyFacts(x: string, y: string): Promise<{ id: string; check: string }> {
  const key = new Uint8Array(65);
  key[0] = 4;
  key.set(unbase64url(x), 1);
  key.set(unbase64url(y), 33);
  const h = await sha256(key);
  const n = new DataView(h.buffer, h.byteOffset).getUint32(8) % 1_000_000;
  return { id: hex(h.subarray(0, 8)), check: `${String(Math.floor(n / 1000)).padStart(3, '0')} ${String(n % 1000).padStart(3, '0')}` };
}

/** What proves a phone knows `code` (guard.rs's `pair_proof`): case and dashes don't matter. */
export const pairProof = async (code: string, x: string, y: string) =>
  hex(await sha256(utf8(`magnitor-pair\n${code.toUpperCase().replace(/[^A-Z0-9]/g, '')}\n${x}\n${y}`)));

/** What a request's signature covers (guard.rs's `signed_text`): the URL as it goes on the wire. */
export const signedText = (challenge: string, seq: number, method: string, url: URL) =>
  `magnitor-v1\n${challenge}\n${seq}\n${method}\n${url.pathname}?${url.search.slice(1)}`;

/** `text` signed with `key`, as the Mac reads a signature. */
export const sign = async (key: CryptoKey, text: string) =>
  base64url(new Uint8Array(await crypto.subtle.sign({ name: 'ECDSA', hash: 'SHA-256' }, key, utf8(text))));
