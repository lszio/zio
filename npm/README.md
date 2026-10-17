# @lszio/zio-core

The Zio language engine compiled to WebAssembly — bytecode VM, macros,
ZOS objects and the self-hosting compiler frontend, for browser and Node
embedding. The native CLI runs the same core, but the WASM package is a
separate host surface: it exposes a session API, not the native host
(file access, processes, QuickJS bindings). Capability status is tracked
in the [feature matrix](https://github.com/lszio/zio/blob/main/docs/feature-matrix.md),
which is the authority on what runs where.

Published on every `v*` release; the npm version matches the Zio tag.

## Install the alpha preview

```sh
npm install @lszio/zio-core@alpha
```

Version 0.2.2 is an alpha preview. Both npm packages use the `alpha` tag;
`latest` still resolves to 0.2.1. Use `@0.2.2` to pin this exact preview.

## Node (>= 18)

```js
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import init, { ZioSession } from '@lszio/zio-core';

// The web-target glue fetches by URL; in Node, pass the module bytes.
const require = createRequire(import.meta.url);
await init({ module_or_path: readFileSync(
  require.resolve('@lszio/zio-core/zio_core_bg.wasm')) });

const session = new ZioSession();
session.eval('(defn sq [x] (* x x)) (sq 7)'); // => "49"
```

## Browser

```js
import init, { ZioSession } from '@lszio/zio-core';
await init(); // fetches zio_core_bg.wasm next to the glue
const session = new ZioSession();
```

The `.wasm` file is a static asset: in a bundler build, make sure
`zio_core_bg.wasm` is emitted and served alongside the JS (or load it
from your own URL via `init({ module_or_path })`), with an
`application/wasm` MIME type so `WebAssembly.instantiateStreaming` works.

## Sessions and errors

- `ZioSession` is one persistent Env + EvalContext across calls:
  top-level definitions survive between `eval` calls on the same session.
  The module-level `eval_zio` export is a single-shot eval with a fresh
  environment each call — use `ZioSession` when state must persist.
- `eval` returns the last value's representation as a string; on failure
  it returns an `Error: ...` string rather than throwing. Check the
  return value's prefix.
- `ZioSession.run_bundle(payload)` runs a bundle emitted by
  `zio build` against the session's authority.

See the [Zio repository](https://github.com/lszio/zio) for the language
reference and the landing-page playground that uses this exact bundle.
