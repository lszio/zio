# ── build the Astro site ────────────────────────────────────────
# The site is static: astro build emits site/dist, which nginx serves at the
# domain root (/ = landing, /grove and /agent are real routes).
# `public/wasm` is copied verbatim, so tools/build-wasm.sh must run before this
# image is built — a stale wasm means the page runs an old engine.
FROM oven/bun:1 AS site-build
WORKDIR /build
COPY site/package.json site/bun.lock ./
RUN bun install --frozen-lockfile
COPY site ./
# wasm/ lives in site/public and is committed; nothing to build for it.
RUN bun run build

# ── serve ───────────────────────────────────────────────────────
FROM nginx:alpine
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=site-build /build/dist /usr/share/nginx/html
COPY docs /usr/share/nginx/html/docs
COPY examples /usr/share/nginx/html/examples
COPY book /usr/share/nginx/html/book
COPY blog /usr/share/nginx/html/blog
