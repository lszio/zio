// Single place where the WASM engine is loaded. A relative
// './wasm/zio_core.js' silently breaks from any page not served out of the
// directory the path was written against (the agent page did exactly that),
// so the URL is always built from Astro's base prefix. Both pages import this
// and get one loader, one URL.
export interface ZioSession {
  eval(code: string): string;
}

export async function loadEngine(): Promise<ZioSession> {
  // Dynamic import is the documented exception here: the specifier is
  // runtime-selected (BASE_URL prefix) and points at a public/ asset the
  // bundler must not try to resolve or fingerprint at build time.
  // The join is explicit on both sides: BASE_URL is '/' when serving from the
  // domain root (concatenating gives '//wasm/…', which the browser reads as
  // protocol-relative and resolves to a bogus host), and it carries NO
  // trailing slash when `base` is a subpath (concatenating gives '/sitewasm').
  const base = import.meta.env.BASE_URL.replace(/\/$/, '');
  const mod = await import(/* @vite-ignore */ `${base}/wasm/zio_core.js`);
  await mod.default();
  return new mod.ZioSession();
}
