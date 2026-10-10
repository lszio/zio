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
# The /reference/ pages are extracted from libs/**.zio `;; doc:` comments at
# build time (apps/site/src/lib/zio-lib-docs.mjs), so the sources must exist.
COPY libs /build/libs
COPY Cargo.toml /build/Cargo.toml
# Git is not copied into this build. CI may supply verified provenance.
ARG ZIO_BUILD_COMMIT
ARG ZIO_BUILD_BRANCH
ARG ZIO_BUILD_TAG
ARG ZIO_BUILD_DIRTY
ENV ZIO_BUILD_COMMIT=$ZIO_BUILD_COMMIT \
    ZIO_BUILD_BRANCH=$ZIO_BUILD_BRANCH \
    ZIO_BUILD_TAG=$ZIO_BUILD_TAG \
    ZIO_BUILD_DIRTY=$ZIO_BUILD_DIRTY
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
