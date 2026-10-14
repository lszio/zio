// Browser-bundle contract harness: loads the wasm engine exactly as
// the emitted HTML does (web-target glue + module bytes) and runs the
// payload through the session, capturing the program's console output.
// Usage: node a01_browser_harness.mjs <glue.js> <wasm> <html>
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const [gluePath, wasmPath, htmlPath] = process.argv.slice(2);
const zio = await import(pathToFileURL(gluePath).href);
await zio.default({ module_or_path: readFileSync(wasmPath) });

const html = readFileSync(htmlPath, 'utf8');
const b64 = /PAYLOAD_B64 = '([^']+)'/.exec(html);
assert.ok(b64, 'the HTML embeds the payload');
const payload = new TextDecoder().decode(
  Uint8Array.from(Buffer.from(b64[1], 'base64')),
);

const lines = [];
const original = console.log;
console.log = (...args) => lines.push(args.join(' '));
let result;
try {
  result = new zio.ZioSession().run_bundle(payload);
} finally {
  console.log = original;
}
assert.deepEqual(lines, ['49', '50'], 'the bundled program prints 49 and 50');
assert.equal(result, 'nil');
original(`PASS browser bundle: ${JSON.stringify(lines)} result=${result}`);
