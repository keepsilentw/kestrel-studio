# Nothing is downloaded and nothing is compiled here.
#
# The host has ~65 KB/s of outbound bandwidth and cannot reach the Docker Hub
# registry, so the image is assembled from artifacts the dev machine already
# produced: a statically linked Linux binary (see `make rust-build-linux`) plus
# the client assets in `public/`. `scratch` is an empty base that Docker never
# pulls, which is what keeps the zero-download property total — there is no
# base image to cache on the host at all.
#
# Static linking is the load-bearing choice. The binary is built for
# x86_64-unknown-linux-musl, where rustc links the C runtime statically by
# default, so there is no glibc to match against (the host is CentOS 7 with
# glibc 2.17, which is also why the Node runtime never worked outside a
# container). TLS trust comes from `webpki-roots` compiled into the binary, so
# the absence of /etc/ssl/certs here is not a problem.
#
# No USER directive: `data/` and `storage/` are host-mounted volumes that the
# previous container wrote as root, and `scratch` has no /etc/passwd to map a
# name against. Changing the ownership is a migration step of its own, not a
# side effect of this switch.
FROM scratch

COPY .build/linux-x86_64/kestrel-studio /app/kestrel-studio
COPY public /app/public

WORKDIR /app

# Relative to WORKDIR unless overridden; spelled out so a layout change fails
# loudly instead of serving 404s for every asset.
ENV STATIC_DIR=/app/public
ENV DATABASE_FILE=data/kestrel-studio.db
ENV STORAGE_DIR=storage
ENV NODE_ENV=production
ENV PORT=8848
EXPOSE 8848

ENTRYPOINT ["/app/kestrel-studio"]
