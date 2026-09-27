---
name: deploy
description: Deploy or operate kestrel-studio on the lavo-test server (Docker). Use when the user asks to deploy, redeploy, release, ship, check status/logs of, or tear down the kestrel-studio deployment.
---

# Deploying kestrel-studio

Target host: the `lavo-test` ssh alias — address, user and key resolve from
`~/.ssh/config`, so nothing here needs to repeat them.
Remote directory: `/opt/kestrel-studio`. Container: `kestrel-studio`, host port 8848.

## One-shot deploy

```bash
make deploy
```

That runs `scripts/deploy.sh`: it packages the source plus `node_modules`,
uploads them (~5 MB/s), and builds the image **on the host** — where nothing is
downloaded, because the toolchain and the Node runtime both come from images
already cached there. First build takes a few minutes (native compile of
better-sqlite3); later runs reuse the Docker layer cache.

## Operational commands

| Command | Effect |
|---|---|
| `make deploy` | Full redeploy |
| `scripts/bootstrap-host.sh` | One-time per fresh host: cache the two base images |
| `make deploy-status` | Container state |
| `make deploy-logs` | Follow logs |
| `make deploy-restart` | Restart container |
| `make deploy-down` | Stop and remove container (keeps `data/`, `storage/`) |

## Constraints that shape this setup — do not "simplify" them away

**The host's outbound bandwidth is ~65 KB/s** (measured: an 8.7MB apt index did
not finish in 60s) while uploads to it run at ~5 MB/s — a 77x asymmetry. Every
decision below follows from that: download nothing on the host, ship everything
from the dev machine over the upload path.

**The build runs on the host but downloads nothing.** The Dockerfile is a
two-stage build over images already cached there:

- `golang:1.24` — its image is built on buildpack-deps, so it already carries
  gcc / g++ / make / python3, exactly the toolchain node-gyp needs. No `apt install`.
- `node:24-bookworm-slim` — the Node runtime, brought in with `COPY --from`.

Both are Debian 12 (bookworm), so the copied Node binary is libc-compatible.
`npm_config_nodedir=/usr/local` points node-gyp at the copied headers; without it
node-gyp downloads a header tarball, which is the one step that would still need
the network.

**node_modules ships in the build context.** Installing them on the host would
take ~25 minutes at 65 KB/s. They are installed on macOS, which is why the
Dockerfile recompiles better-sqlite3 for linux during the build.

**Do not "simplify" this back to a single-stage build on a pulled base image.**
That reintroduces exactly the downloads this shape exists to avoid.

**`.dockerignore` patterns must be anchored.** Unanchored names match at any
depth and silently delete same-named directories inside node_modules. A bare
`storage` did this to `multer/storage`, which surfaced at runtime as
`Cannot find module './storage/disk'`, reported misleadingly by NestJS as
"platform-express is not installed". See docs/implementation-notes.md §6.

**Node cannot be installed on the host directly.** The server runs CentOS 7
(glibc 2.17); Nest 12 / Vite 8 need Node 20+ (glibc 2.28+). Verified: the official
linux-x64 binary dies with `GLIBC_2.27 not found`. This is also why pm2 with a
system Node is not an option — and it would still need to compile better-sqlite3.
Docker is the only viable runtime.

**Base images come from a mirror.** Docker Hub is unreachable from the host; run
`scripts/bootstrap-host.sh` once per fresh host to cache them.

**Docker Hub is unreachable from the server** (`registry-1.docker.io` returns
nothing), so the base image is pulled from a mirror. Measured on the host:

| Mirror | Result |
|---|---|
| `dockerproxy.net` | works, ~41s |
| `hub.rat.dev` | timeout |
| `docker.nju.edu.cn` | fails immediately |
| `docker.m.daocloud.io` | stalls (~0.04 MB/s) |
| `docker.1ms.run` | stalls |

Use `dockerproxy.net`. Because Docker stores layers by digest, retrying through
a different mirror reuses whatever layers already landed — switching mirrors is
never wasted work.

npm traffic is a separate path and is fast here; the Dockerfile points it at
`registry.npmmirror.com`.

**Do not touch `/etc/docker/daemon.json` or restart the docker daemon.** The host
runs other production containers plus 1Panel's openresty; restarting the daemon
takes them all down. Mirrors are consumed per-image instead.

**The app listens on 8848 inside the container**, published via `HOST_PORT`
(default 8848). Changing it means also opening the port in the cloud security
group.

## Secrets

`.env` lives only on the server at `/opt/kestrel-studio/.env` and is written on
the first deploy by `scripts/deploy.sh`, which reads the Bailian key from the
local cc-switch database, generates a fresh `SESSION_SECRET`, and copies the
`SUPER_ADMIN_USERNAME` / `SUPER_ADMIN_PASSWORD` you export in the shell that runs
the deploy. **The repository ships no usable credential**: without those two
variables the script stops rather than seeding an account nobody chose, and under
`NODE_ENV=production` (set by the Dockerfile) the local `admin` convenience
account is not created at all.

The script never overwrites an existing `.env` — regenerating `SESSION_SECRET`
would invalidate every logged-in session, and the host has no cc-switch database
to fall back to for the API key. To rotate the key, edit the file on the host
directly and `make deploy-restart`. To rotate the super admin password, use
`/admin/users/:id` — a restart never resets it.

## Troubleshooting

Build fails on `pnpm install` / native compile:

```bash
ssh lavo-test "cd /opt/kestrel-studio && docker compose build --no-cache 2>&1 | tail -50"
```

Container starts but the app exits: the key is usually the cause — check
`make deploy-logs` for `No Bailian API key found`.

App healthy inside the container but unreachable from outside: pass the port
check first (`ssh lavo-test "curl -sf -o /dev/null http://127.0.0.1:8848/login && echo ok"`),
then look at the cloud security group.
