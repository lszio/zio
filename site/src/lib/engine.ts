// Single place where the WASM engine is loaded. The site is served under
// Astro's `base` ('/site'), so a relative './wasm/zio_core.js' silently
// breaks from a nested page (e.g. /site/agent/ → /site/agent/wasm/... 404).
// Both pages import this and get one loader, one URL.
export interface ZioSession {
  eval(code: string): string;
}

export async function loadEngine(): Promise<ZioSession> {
  // Dynamic import is the documented exception here: the specifier is
  // runtime-selected (BASE_URL prefix) and points at a public/ asset the
  // bundler must not try to resolve or fingerprint at build time.
  // BASE_URL carries no trailing slash when `base` is written as '/site',
  // so the separator must be explicit — '/site' + 'wasm/…' would 404.
  const mod = await import(/* @vite-ignore */ `${import.meta.env.BASE_URL}/wasm/zio_core.js`);
  await mod.default();
  return new mod.ZioSession();
}
