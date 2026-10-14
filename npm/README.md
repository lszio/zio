# @lszio/zio-core

The Zio language engine compiled to WebAssembly. This is the same
`zio-core` the native CLI runs — bytecode VM, macros, ZOS objects and the
self-hosting compiler frontend included.

Published on every `v*` release; the npm version matches the Zio tag.

## Node

```js
import { readFileSync } from 'node:fs';
import * as zio from '@lszio/zio-core';

// The `web`-target glue needs the module bytes explicitly in Node.
await zio.default(readFileSync(require.resolve(
  '@lszio/zio-core/zio_core_bg.wasm')));

const session = new zio.ZioSession();
session.eval('(defn sq [x] (* x x)) (sq 7)'); // => "49"
```

## Browser

```js
import init, { ZioSession } from '@lszio/zio-core';
await init(); // fetches zio_core_bg.wasm next to the glue
const session = new ZioSession();
```

State persists across `eval` calls on one session; errors return an
`Error: …` string. See the [Zio repo](https://github.com/lszio/zio) for
the language reference and the landing-page playground that uses this
exact bundle.
