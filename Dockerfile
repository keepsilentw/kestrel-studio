# Builds on the target host with ZERO network access.
#
# Both base images are already cached on that host, and the host's outbound
# bandwidth is ~65 KB/s — so anything that needs downloading there is a
# non-starter. What makes this work:
#
#   * golang's official image is built on buildpack-deps, so it already carries
#     gcc / g++ / make / python3 — exactly the toolchain node-gyp needs to
#     compile better-sqlite3. No apt install required.
#   * node:24-bookworm-slim supplies the runtime. Both images are Debian 12
#     (bookworm), so the Node binary copied across is libc- and ABI-compatible.
#   * node_modules arrives inside the build context, shipped from the dev
#     machine where the network is fast. Installing them here would take ~25
#     minutes at 65 KB/s.

FROM dockerproxy.net/library/node:24-bookworm-slim AS nodedist

FROM golang:1.24

COPY --from=nodedist /usr/local/ /usr/local/

WORKDIR /app

COPY . .

# node_modules was installed on macOS, so the better-sqlite3 binary in it is
# darwin-arm64. Rebuild it against the linux toolchain in this image.
#
# npm_config_nodedir points node-gyp at the headers that came with the copied
# Node install (/usr/local/include/node); without it, node-gyp downloads a
# matching header tarball from nodejs.org, which is the one thing that would
# still need the network.
RUN cd "$(ls -d node_modules/.pnpm/better-sqlite3@*/node_modules/better-sqlite3 | head -1)" \
    && npm_config_nodedir=/usr/local /app/node_modules/.bin/node-gyp rebuild

ENV NODE_ENV=production
ENV PORT=8848
EXPOSE 8848

CMD ["node", "dist/main.js"]
