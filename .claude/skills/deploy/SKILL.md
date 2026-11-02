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

That is `make build-linux` + `make build-wasm` followed by `scripts/deploy.sh`.
The script stages the cross-compiled **static musl binary** plus the built
`public/`, uploads a few MB (~5 MB/s inbound), and builds the image **on the
host** — where nothing is downloaded and nothing is compiled: the runtime base
is `scratch`, which Docker never pulls. The build is two `COPY` directives, so
it finishes in seconds; the resulting image is ~10 MB.

Two preflight failures are by design and happen before anything is uploaded:

- no binary at `/tmp/kestrel-studio-linux/kestrel-studio` (or `KESTREL_LINUX_BINARY`
  pointing elsewhere) — `make build-linux` did not run;
- a `rust/server/src/**/*.rs` **newer** than that binary — the binary is stale, so
  the deploy would serve yesterday's code. Rebuild instead of overriding.

After `docker compose up -d --build` the script polls
`http://127.0.0.1:${HOST_PORT}/login` on the host for up to 180 s and fails if
the app never answers.

## Operational commands

| Command | Effect |
|---|---|
| `make deploy` | Full redeploy |
| `scripts/bootstrap-host.sh` | One-time per fresh host: checks that Docker works. Nothing to cache under the Rust pipeline; pass `NEST_BASE_IMAGES=1` only to roll back to the Node pipeline |
| `make deploy-status` | Container state |
| `make deploy-logs` | Follow logs |
| `make deploy-restart` | Restart container |
| `make deploy-down` | Stop and remove container (keeps `data/`, `storage/`) |

## Constraints that shape this setup — do not "simplify" them away

**The host's outbound bandwidth is ~65 KB/s** (measured: an 8.7MB apt index did
not finish in 60s) while uploads to it run at ~5 MB/s — a 77x asymmetry. Every
decision below follows from that: download nothing on the host, ship everything
from the dev machine over the upload path.

**The build runs on the host but downloads nothing.** `FROM scratch` is an empty
base that Docker never pulls, so the host needs **no image at all** — there is
nothing to cache, and nothing to compile. Everything else comes from the dev
machine over the fast upload path: the static binary (`make build-linux`, via
zig) and the client assets (`make build` / `make build-wasm` → `public/`).

**Static linking is the load-bearing choice.** The host is CentOS 7 (glibc
2.17); a dynamically linked binary would die with `GLIBC_2.27 not found`. The
musl ELF needs no libc at all, which is what makes `scratch` usable. Do not
"simplify" this back into a pulled base image or a host-side install: at 65 KB/s
of outbound bandwidth, either one reintroduces exactly the downloads this shape
exists to avoid (the old Node pipeline cached `golang:1.24` +
`node:24-bookworm-slim` and recompiled better-sqlite3 on the host).

**TLS trust is compiled in.** `scratch` has no `/etc/ssl/certs`, so the binary
uses `webpki-roots`. If outbound HTTPS ever starts failing inside the
container, the root store is the suspect — not a missing CA bundle on the host.

**`/tmp` comes from compose's `tmpfs`.** `scratch` has no `/tmp`, and SQLite
writes temporary b-tree files there; without the mount that shows up only under
load as "unable to open temporary file". Keep it in `docker-compose.yml`.

**No `USER` directive, on purpose.** `data/` and `storage/` are host-mounted
volumes previously written as root, and `scratch` has no `/etc/passwd` to map a
name against. Changing ownership is its own migration, not a side effect.

**The container has no shell.** `docker exec` into it will fail — there is no
/bin/sh in `scratch`. Inspect via `docker logs`, host-side files
(`/opt/kestrel-studio/...`), and HTTP probes; that is also why the verification
list below is written that way.

**`.dockerignore` patterns must be anchored.** Unanchored names match at any
depth and silently delete same-named directories inside node_modules. A bare
`storage` did this to `multer/storage`, which surfaced at runtime as
`Cannot find module './storage/disk'`, reported misleadingly by NestJS as
"platform-express is not installed". See docs/implementation-notes.md §6.

Note that the build context is no longer the repository: `deploy.sh` assembles
it in `/tmp/kestrel-studio-deploy` from a whitelist (`public`,
`.build/linux-x86_64/kestrel-studio`, `Dockerfile`, `docker-compose.yml`,
`.dockerignore`, `Makefile`). Most `.dockerignore` entries are therefore
insurance for manual `docker compose build` from a full checkout — the anchoring
rule still applies if you add one.

**Node cannot be installed on the host directly.** The server runs CentOS 7
(glibc 2.17); Nest 12 / Vite 8 need Node 20+ (glibc 2.28+). Verified: the official
linux-x64 binary dies with `GLIBC_2.27 not found`. This is also why pm2 with a
system Node is not an option — and it would still need to compile better-sqlite3.
Docker is the only viable runtime.

**Rollback only: base images come from a mirror.** The Rust pipeline pulls
nothing, so this section does not apply to a normal deploy. It matters when
rolling back to the Node pipeline (`NEST_BASE_IMAGES=1
scripts/bootstrap-host.sh`). Docker Hub is unreachable from the server
(`registry-1.docker.io` returns nothing), so images are pulled from a mirror:

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

**Rollback only: node_modules.** `deploy.sh` stopped uploading it on 2026-10-30.
The copy under `/opt/kestrel-studio/node_modules` (353 MB) is from **2026-09-27**
and predates the monorepo split, so it does not match the Nest tree's layout
today: a rollback must reinstall on the dev machine and ship a fresh
`node_modules`, not reuse the host's. See docs/deployment.md §1.1.

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

## Cutting over between the two server implementations

The repository holds two same-endpoint server implementations over one SQLite
file. **Production has been `rust/server/` since 2026-11-02 13:24** (three
deploys that afternoon; the running one started 13:47 — `ENTRYPOINT
[/app/kestrel-studio]`, image ~10 MB). `nest/server/` stays in the tree as the
contract source and the rollback option until roadmap.md §4 P4.

What is known from the host, not inferred (docs/verification.md §13):

- The Rust server answers `/login`, serves `public/` from its own static handler,
  guards `/admin`, mounts `/api/voice/ticket`, and completed a real streaming
  turn whose 37 events landed with a contiguous `seq` and a `turn_done`.
- Not yet exercised on the Rust path: image/video generation tasks, a real voice
  call, `/admin` page contents, SSE reconnect-with-replay, browser console noise.
  Those need a logged-in browser, so they only get settled by a human pass.
- **Sessions do not survive a change of sides.** The cookie signing scheme
  differs — both are HMAC-SHA256 + base64url-no-pad, but `cookie-signature` signs
  the token **without** the `s:` prefix while `rust/server/src/auth/session.rs`
  signs `"s:" + token` — so the same token yields a different digest and old
  cookies are rejected rather than mis-trusted: whoever takes traffic logs every
  user out once. Happened on 2026-11-02 as predicted; tell users before it
  happens again instead of letting it look like data loss.
- Row shapes differ, and **reading is asymmetric**. Passwords stay bcrypt `$2b$`
  and both sides verify them. But `sessions.sess` is `{"cookie":{…},
  "passport":{"user":<id>}}` from Nest (passport stores only the id) and
  `{"cookie":{…},"user":<id>}` from Rust: Rust accepts both positions, Nest only
  knows `passport.user`. **A rollback to Nest logs out everyone who signed in on
  Rust** — it is not a data problem, but it is not transparent either.
  `sessions.expire` is ISO-8601 text either way (Rust also parses the epoch-millis
  rows it wrote before `09c975b`).
- One writer at a time. Two containers cannot share port 8848 and the ordering
  rule in docs/deployment.md §7 (stop the old before starting the new) applies
  to any cutover, including a rollback.

## Verifying a deploy (read-only, no shell in the container)

```bash
# what is running, and how heavy it is
ssh lavo-test 'docker ps --filter name=kestrel-studio --format "{{.Image}} {{.Status}} {{.Ports}}"
  docker image inspect kestrel-studio:latest --format "created={{.Created}} entrypoint={{.Config.Entrypoint}}"
  docker logs --tail 15 kestrel-studio 2>&1
  docker stats --no-stream kestrel-studio | tail -1'

# endpoint surface, probed from the host
ssh lavo-test 'for p in /login /assets/main.css /capture.js /wasm/kestrel_client_bg.wasm /api/voice/ticket /admin; do
  printf "%-32s " "$p"; curl -s -o /dev/null -w "%{http_code} %{size_download}B\n" "http://127.0.0.1:8848$p"; done'

# assets really are what this machine built
md5 public/assets/main.css
ssh lavo-test "md5sum /opt/kestrel-studio/public/assets/main.css"

# public entry through openresty, resolved locally so it works even without DNS
ssh lavo-test 'curl -s -o /dev/null -w "%{http_code} -> %{redirect_url}\n" \
  --resolve try.kestrel.justwork.link:443:127.0.0.1 https://try.kestrel.justwork.link/admin'

# did work land in the database
ssh lavo-test 'sqlite3 "file:/opt/kestrel-studio/data/kestrel-studio.db?mode=ro" \
  "select (select count(*) from conversation_events), (select count(*) from messages), (select count(*) from assets);"'
```

Expected on 2026-11-02: `kestrel_studio listening on http://localhost:8848` plus
`Bailian credentials resolved` and `expired sessions cleared=N` in the logs, RSS
around **1.2 MiB**, `/login` 200, `/assets/main.css` 200 with an md5 equal to the
local build, `/capture.js` 200, the wasm 200 (**shipped but not referenced by any
template yet**), `/api/voice/ticket` 405, `/admin` 302 to `/login`, and
`https://try.kestrel.justwork.link/admin` 302 to the login page.

Open the database with `mode=ro` while the app holds it. A plain open would also
let `sqlite3` checkpoint the WAL on close — harmless and even useful, but then
don't be surprised when `kestrel-studio.db-wal` shrinks and the main file grows
(docs/deployment.md §7 already records this).

**A read-only pass needs the DEBUG switch.** Reads do not write rows, so the
per-request line is what makes `/admin`, an SSE resume or a static-file 404
auditable at all. `rust/server/src/http/router.rs` carries
`tower_http::trace::TraceLayer`, at DEBUG only, so the default `info` filter hides
it completely and production log volume does not change; it has been live on the
host since the 16:18 deploy on 2026-11-02. Turn it on just for the walkthrough:

```bash
# turn it on (compose reads RUST_LOG from the shell; .env is never rewritten)
ssh lavo-test "cd /opt/kestrel-studio && RUST_LOG=debug docker compose up -d"
# walk the page, then read it
ssh lavo-test "docker logs --since 10m kestrel-studio 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E 'method=|status=' | tail -40"
# back to the default; `make deploy` does not forward RUST_LOG either
ssh lavo-test "cd /opt/kestrel-studio && docker compose up -d"
```

| What was exercised | Evidence to look for |
|---|---|
| an image or video turn | new `messages` + `assets` rows (video also `generation_tasks`) |
| a voice call | `voice session opened` in `docker logs`, plus a conversation titled `语音会话` |
| a login | a new `sessions` row whose `sess` carries the principal at top-level `user` |
| `/admin`, SSE resume, static files | a DEBUG line: `method=`, `uri=` (query kept, so a resume reads `/stream?from=N`), `status=`, latency |

Three limits when reading that log: the lines arrive **with ANSI escapes**, so a
plain `grep 'method='` matches nothing — strip them with `sed 's/\x1b\[[0-9;]*m//g'`
first (docs/verification.md §13.8); `/api/voice` is deliberately outside the traced
group (it hijacks the connection and its 101 takeover has never run on a host — a
test pins the exclusion), and for a streaming response `latency` is
time-to-first-byte, not the length of the call. Details: docs/deployment.md §2.1,
docs/verification.md §13.7, docs/architecture.md §13.3.

So when a manual browser walkthrough gets reported as "verified", the first
question is which of those artefacts appeared. If the answer is only console
cleanliness, record it as an operator observation, not machine evidence, and never
let it flip a roadmap.md P1 criterion on its own (docs/verification.md §13.6 walks
through exactly this case).

## Troubleshooting

`deploy.sh` stops with **"no /tmp/kestrel-studio-linux/kestrel-studio"** or
**"… is newer than … — run `make build-linux` again"**: the cross-compile did not
run, or it ran before the last source edit. Both are preflight guards against
shipping yesterday's code — rebuild, do not edit the check. `make build-linux`
itself needs three things: `zig`, `cargo-zigbuild`, and the
`x86_64-unknown-linux-musl` compile target. It checks each one and prints the
install command. There are **no** `scripts/cross/` shims any more — that
hand-written approach was dropped on 2026-10-30 because the link step never
worked (docs/implementation-notes.md §14).

Image build tries to pull a base image: someone moved the `Dockerfile` off
`FROM scratch`, or the compose context is not the directory `deploy.sh`
assembled. The host cannot reach Docker Hub, so a pull is always a bug here.

Container starts but the app exits: read `make deploy-logs` first. The three
known fatal configurations are
`No Bailian API key found. Set BAILIAN_API_KEY, or keep the bailian-token-plan provider in cc-switch.`,
`SESSION_SECRET must be set when NODE_ENV=production`, and — if compose lost the
`tmpfs: /tmp` line — SQLite's "unable to open temporary file", which only shows
up once a query needs a temp b-tree.

Every user got logged out by a deploy that changed nothing else: the serving
implementation changed sides, or `SESSION_SECRET` was regenerated by a recreated
`.env`. Both are covered above; check which before touching user data.

Assets 404 or stale after a deploy: the image sets `STATIC_DIR=/app/public`
explicitly so a layout change fails loudly instead of 404-ing everything —
compare the md5 of `public/assets/main.css` against the host copy, then hard
refresh before suspecting the app.

SSE appears to arrive in one burst instead of streaming: that is the reverse
proxy's `proxy_buffering`, not the app. The site fragment settings are in
docs/deployment.md §3; the same fragment also needs `location = /api/voice` with
`Upgrade`/`Connection` for voice to work. (Unrelated: a `GET` to
`/api/voice/ticket` returning 405 is correct — that route only takes `POST`.)

App healthy inside the container but unreachable from outside: pass the port
check first (`ssh lavo-test "curl -sf -o /dev/null http://127.0.0.1:8848/login && echo ok"`),
then look at the cloud security group.
