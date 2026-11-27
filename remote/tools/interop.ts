// Writes the vector Magnitor's data/src/guard.rs tests the phone's bytes against
// (`a_phones_bytes_match_the_macs`): a P-256 key from WebCrypto, as the phone makes it, its id
// and digits, a pairing proof and a signed request, all by sign.ts, which app.ts uses.
//   bun tools/interop.ts > ../data/src/interop.json
// A search with marks a query may encode (an apostrophe, a space, a slash, a curly quote) shows
// that what is signed is what goes on the wire.
import { enc, keyFacts, pairProof, sign, signedText } from '../sign.ts';

const pair = (await crypto.subtle.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, true, ['sign', 'verify'])) as CryptoKeyPair;
const jwk = await crypto.subtle.exportKey('jwk', pair.publicKey);
const x = String(jwk.x), y = String(jwk.y);
const challenge = '00112233445566778899aabbccddeeff';
const url = new URL(`http://192.168.1.20:47820/v1/activity?sort=cpu&q=${enc("Ada's /tmp “x” (1)!")}`);
const text = signedText(challenge, 7, 'GET', url);
console.log(JSON.stringify({
  x, y, ...(await keyFacts(x, y)),
  code: 'abcd-efgh', proof: await pairProof('abcd-efgh', x, y),
  challenge, seq: 7, method: 'GET', target: `${url.pathname}${url.search}`, sig: await sign(pair.privateKey, text),
}, null, 2));
