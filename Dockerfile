# ── build the Astro site ────────────────────────────────────────
# The site is static: Astro emits apps/site/dist, served by nginx.
# Language documentation and playground are separate from the Grove application.
# `public/wasm` is copied verbatim, so tools/build-wasm.sh must run before this
# image is built — a stale wasm means the page runs an old engine.
FROM oven/bun:1 AS site-build
WORKDIR /build/apps/site
COPY apps/site/package.json apps/site/bun.lock ./
RUN bun install --frozen-lockfile
COPY apps/site ./
COPY docs /build/docs
COPY book /build/book
COPY blog /build/blog
# wasm/ is committed under apps/site/public and rebuilt with tools/build-wasm.sh.
RUN bun run build

# ── serve ───────────────────────────────────────────────────────
FROM nginx:alpine
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=site-build /build/apps/site/dist /usr/share/nginx/html
COPY docs /usr/share/nginx/html/docs
COPY examples /usr/share/nginx/html/examples
COPY book /usr/share/nginx/html/book
COPY blog /usr/share/nginx/html/blog
