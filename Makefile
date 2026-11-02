# kestrel-studio Makefile — 快捷命令入口
# kestrel-studio Makefile — the single command entry point for the monorepo.

SHELL := /bin/bash
.DEFAULT_GOAL := help

PNPM := pnpm
PM2  := pm2
APP  := kestrel-studio

# Repository layout (monorepo, packaged by backend technology stack). Each stack
# owns both its server and its browser client; there is no separate frontend app:
#
#   nest/server/   NestJS backend  — package.json, src/, __tests__/, tsconfig*.json,
#                                    nest-cli.json, drizzle.config.ts, dist/
#   nest/client/   Nest browser    — package.json, scripts/, styles/, public/,
#                                    __tests__/, tsconfig.json, vite.config.mts,
#                                    vitest.config.mts
#   rust/server/   Rust backend    — Cargo package `kestrel-studio` (the deploy binary)
#   rust/client/   Rust browser    — Cargo package `kestrel-client` (compiles to WASM)
#   crates/        shared Rust     — proto/ (wire types), md/ (markdown subset)
#   public/        build output    — the asset directory BOTH servers serve
#
# The root `package.json` is the pnpm workspace root and declares **no** dependencies:
# every package belongs to the stack that imports it, and each package owns its
# config, its tooling and its test runner. `pnpm-workspace.yaml` lists
# `nest/server` and `nest/client`; the Rust packages (rust/server/, rust/client/,
# crates/) stay out of it. Node commands run per package (`pnpm --dir nest/server
# run build`) — except the nest/drizzle ones, whose script does `cd ../..` first,
# because `.env`, `DATABASE_FILE` and `STORAGE_DIR` are resolved from the working
# directory and must keep the repo root as their cwd.
#
# Target names are `<verb>-<stack>-<role>`:
#
#   build-nest-server      build-nest-client
#   build-rust-server      build-rust-client
#   typecheck-* / test-* / clean-* follow the same suffixes
#
# The unsuffixed build / typecheck / test / clean aggregate every package. The
# pre-restructure spellings (`build-server`, `build-web`, `rust-*`, ...) remain as
# aliases at the bottom.

CARGO ?= cargo
NEST_SERVER_DIR ?= $(CURDIR)/nest/server
NEST_CLIENT_DIR ?= $(CURDIR)/nest/client
RUST_SERVER_DIR ?= $(CURDIR)/rust/server
RUST_CLIENT_DIR ?= $(CURDIR)/rust/client
CARGO_SERVER_ARGS := --manifest-path $(RUST_SERVER_DIR)/Cargo.toml
CARGO_CLIENT_ARGS := --manifest-path $(RUST_CLIENT_DIR)/Cargo.toml

# The Rust server is what `make deploy` ships now. `make start` (pm2 + the Nest
# process) is still the Nest pipeline until the WASM client lands; the two are
# separate paths and neither one silently takes the other's port.
#
# The deploy binary is cross-compiled, never built on the host: a static musl
# ELF needs no glibc there and no toolchain there.
RUST_LINUX_TARGET ?= x86_64-unknown-linux-musl
# Outside the working tree, twice over: the cargo target dir and the final
# binary. Both are reachable by deploy.sh, which assembles the docker context
# under /tmp rather than next to source files.
LINUX_TARGET_DIR ?= /tmp/kestrel-studio-linux-target
LINUX_BINARY     ?= /tmp/kestrel-studio-linux/kestrel-studio

# The pm2 entry must point here. `start` and `restart` compare it against the
# entry pm2 actually holds and rebuild the entry when the repository moved — a
# plain `pm2 restart` keeps the old `exec path` and leaves a "running" process
# with no pid.
ENTRY ?= $(CURDIR)/nest/server/dist/main.js

# Local port the app listens on. Keep in sync with nest/server/src/config/configuration.ts.
PORT ?= 8848

DEPLOY_HOST ?= lavo-test
DEPLOY_DIR  ?= /opt/kestrel-studio

# Health probe shared by `start` and `status`, so the two cannot drift.
# The code is captured rather than printed by curl: on a refused connection curl
# still emits its -w format (as `000`), which would otherwise be reported as if
# it were a real status code. `$$` escapes for make; the shell sees `$`.
define HEALTH_PROBE
probe=$$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 "http://127.0.0.1:$(PORT)/login" 2>/dev/null); \
if [ "$$probe" = "200" ]; then echo "正常  HTTP 200"; \
elif [ -z "$$probe" ] || [ "$$probe" = "000" ]; then echo "无响应（服务未启动？）"; \
else echo "异常  HTTP $$probe"; fi
endef

.PHONY: help
.PHONY: install
.PHONY: dev web-watch
.PHONY: build build-nest-server build-nest-client build-rust-server build-rust-client
.PHONY: start stop restart status logs
.PHONY: typecheck typecheck-nest-server typecheck-nest-client typecheck-rust-server typecheck-rust-client
.PHONY: test test-nest-server test-nest-client test-rust-server test-rust-client test-watch
.PHONY: clean clean-nest-server clean-nest-client clean-rust-server clean-rust-client clean-crates
.PHONY: run-rust build-rust-server-linux
.PHONY: build-server build-web build-rust
.PHONY: typecheck-server typecheck-web typecheck-rust
.PHONY: test-server test-web test-rust
.PHONY: clean-server clean-web clean-rust
.PHONY: rust-build rust-check rust-test rust-clean rust-run rust-build-linux
.PHONY: deploy deploy-status deploy-logs deploy-restart deploy-down

help:
	@echo "kestrel-studio — 可用命令:"
	@echo ""
	@echo "  安装"
	@echo "    make install                安装 workspace 依赖（nest/server + nest/client）"
	@echo ""
	@echo "  开发"
	@echo "    make dev                    Nest 后端 watch（nest start --watch）"
	@echo "    make web-watch              Nest 客户端资源 watch 构建（另开终端）"
	@echo ""
	@echo "  构建"
	@echo "    make build                  全部包（nest client/server + rust client/server）"
	@echo "    make build-nest-server      仅 Nest 后端（→ nest/server/dist）"
	@echo "    make build-nest-client      仅 Nest 客户端资源（Vite → public/assets）"
	@echo "    make build-rust-server      仅 Rust release 二进制"
	@echo "    make build-rust-client      wasm-pack 构建 WASM 到 public/wasm（模板暂未切换）"
	@echo ""
	@echo "  运行（pm2 守护 Nest）"
	@echo "    make start                  构建 Nest 前后端并一键启动，打印访问信息"
	@echo "    make stop                   停止服务"
	@echo "    make restart                重启服务（不重新构建）"
	@echo "    make status                 查看服务运行状态与健康探测"
	@echo "    make logs                   跟随服务日志"
	@echo ""
	@echo "  质量"
	@echo "    make typecheck              全部包（Nest 三份 tsconfig + 两个 cargo check）"
	@echo "    make typecheck-nest-server  仅 Nest 后端（tsconfig.json + tsconfig.test.json）"
	@echo "    make typecheck-nest-client  仅 Nest 客户端（tsconfig.json）"
	@echo "    make typecheck-rust-server  仅 Rust 服务端（cargo check --tests）"
	@echo "    make typecheck-rust-client  仅 Rust 客户端（cargo check --tests）"
	@echo "    make test                   全部包单元测试（Nest 两轮 vitest + 两个 cargo test）"
	@echo "    make test-nest-server       仅 Nest 后端（vitest，node 环境）"
	@echo "    make test-nest-client       仅 Nest 客户端（vitest，jsdom 环境）"
	@echo "    make test-rust-server       仅 Rust 服务端（cargo test）"
	@echo "    make test-rust-client       仅 Rust 客户端（cargo test）"
	@echo "    make test-watch             Nest 后端 watch（Nest 客户端：pnpm --dir nest/client run test:watch）"
	@echo ""
	@echo "  清理"
	@echo "    make clean                  全部产物（保留 data/ 与 storage/）"
	@echo "    make clean-nest-server      仅 nest/server/dist（含重组前的根 dist/）"
	@echo "    make clean-nest-client      仅 public/"
	@echo "    make clean-rust-server      仅 rust/server/target（cargo clean）"
	@echo "    make clean-rust-client      仅 rust/client/target"
	@echo "    make clean-crates           仅 crates/*/target"
	@echo ""
	@echo "  Rust（make deploy 走这条管线）"
	@echo "    make build-rust-server-linux 交叉编译静态 musl 二进制到 $(LINUX_BINARY)（需 zig）"
	@echo "    make run-rust                本机前台运行 Rust 服务（占用 $(PORT)）"
	@echo "    兼容别名                     build-server / build-web / build-rust / typecheck-{server,web,rust}"
	@echo "                                 test-{server,web,rust} / clean-{server,web,rust} / rust-*"
	@echo ""
	@echo "  部署"
	@echo "    make deploy                 一键部署到 $(DEPLOY_HOST)"
	@echo "    make deploy-status          查看远端容器状态"
	@echo "    make deploy-logs            跟随远端容器日志"
	@echo "    make deploy-restart         重启远端容器"
	@echo "    make deploy-down            停止并移除远端容器（保留 data/ 与 storage/）"

# ===== 安装 =====

# One `pnpm install` at the workspace root covers both Node packages; dependencies
# live in nest/server/package.json and nest/client/package.json, never in root.
install:
	$(PNPM) install

# ===== 开发 =====

dev:
	@if $(PM2) describe $(APP) >/dev/null 2>&1; then \
		echo "注意: pm2 进程 $(APP) 正在运行并占用 $(PORT) 端口，watch 模式会启动失败。"; \
		echo "      先执行 make stop，或直接用 make start / make restart。"; \
		echo ""; \
	fi
	$(PNPM) --dir $(NEST_SERVER_DIR) run dev

web-watch:
	$(PNPM) --dir $(NEST_CLIENT_DIR) run watch

# ===== 构建 =====

build: build-nest-client build-nest-server build-rust-server build-rust-client

build-nest-client:
	$(PNPM) --dir $(NEST_CLIENT_DIR) run build

build-nest-server:
	$(PNPM) --dir $(NEST_SERVER_DIR) run build

build-rust-server:
	$(CARGO) build $(CARGO_SERVER_ARGS) --release
	@echo "    二进制      $(RUST_SERVER_DIR)/target/release/kestrel-studio"

# Builds the Rust client to WASM with wasm-pack and stages it under
# `public/wasm/`. Templates still load `/assets/main.js` and `/assets/voice.js`
# (the TS client) until roadmap §4 P3 wires `initChat` / `initVoice`; until then
# the staged WASM is a build artifact, not a runtime entry point.
# Order matters: Vite (`build-nest-client`) empties `public/`, so both the WASM
# output and the AudioWorklet are copied afterwards.
build-rust-client: build-nest-client
	cd $(RUST_CLIENT_DIR) && wasm-pack build --release --target web --out-dir pkg
	mkdir -p $(CURDIR)/public/wasm
	cp -R $(RUST_CLIENT_DIR)/pkg/. $(CURDIR)/public/wasm/
	cp $(RUST_CLIENT_DIR)/public/capture.js $(CURDIR)/public/capture.js
	@echo "    WASM          $(CURDIR)/public/wasm/kestrel_client_bg.wasm"
	@echo "    AudioWorklet  $(CURDIR)/public/capture.js"

# ===== 运行（pm2 守护） =====

# The frontend has no runtime process — Vite only emits assets that Nest serves —
# so "starting the Nest stack" means building the assets and putting the Nest
# process under pm2.
# `start` depends on the two Nest builds rather than on `build`: the Rust binary
# is the other server (run-rust / deploy) and has no place in the pm2 entry.
# `--cwd` stays the repo root even though the entry point is `nest/server/dist`:
# `.env`, `data/` and `storage/` are all resolved from the working directory, and
# moving them would change what the running instance sees.
start: build-nest-client build-nest-server
	@echo ""
	@echo "==> 启动 $(APP)"
	@if $(PM2) describe $(APP) >/dev/null 2>&1 \
		&& $(PM2) describe $(APP) 2>/dev/null | grep -F "script path" | grep -qF "$(ENTRY)"; then \
		$(PM2) restart $(APP) --update-env >/dev/null 2>&1 \
			&& echo "    已重启现有 pm2 进程" \
			|| { echo "    重启失败，pm2 输出："; $(PM2) restart $(APP) --update-env; exit 1; }; \
	else \
		if $(PM2) describe $(APP) >/dev/null 2>&1; then \
			echo "    现有 pm2 条目指向旧路径，删除后重建"; \
			$(PM2) delete $(APP) >/dev/null 2>&1 || true; \
		fi; \
		$(PM2) start "$(ENTRY)" --name $(APP) --cwd "$(CURDIR)" >/dev/null 2>&1 \
			&& echo "    已创建 pm2 进程" \
			|| { echo "    启动失败，pm2 输出："; $(PM2) start "$(ENTRY)" --name $(APP) --cwd "$(CURDIR)"; exit 1; }; \
	fi
	@sleep 2
	@printf "    健康探测    "
	@$(HEALTH_PROBE)
	@echo ""
	@echo "  $(APP) 已交给 pm2 托管"
	@echo "    访问        http://localhost:$(PORT)"
	@echo "    初始账号    admin / 123456"
	@echo "    状态        make status"
	@echo "    日志        make logs"
	@echo "    停止        make stop"
	@echo "    提示        若上面健康探测未通过，说明进程起来了但服务没就绪，看 make logs"
	@echo ""

stop:
	@if $(PM2) describe $(APP) >/dev/null 2>&1; then \
		$(PM2) delete $(APP) >/dev/null 2>&1; \
		echo "  已停止 $(APP)"; \
	else \
		echo "  $(APP) 未在运行"; \
	fi

restart:
	@if ! $(PM2) describe $(APP) >/dev/null 2>&1; then \
		echo "  $(APP) 未在运行，请用 make start"; \
	elif $(PM2) describe $(APP) 2>/dev/null | grep -F "script path" | grep -qF "$(ENTRY)"; then \
		$(PM2) restart $(APP) --update-env >/dev/null 2>&1 \
			&& echo "  已重启 $(APP)" \
			|| { echo "  重启失败，pm2 输出："; $(PM2) restart $(APP) --update-env; exit 1; }; \
	else \
		echo "  pm2 条目指向旧路径，按 $(ENTRY) 重建"; \
		$(PM2) delete $(APP) >/dev/null 2>&1 || true; \
		$(PM2) start "$(ENTRY)" --name $(APP) --cwd "$(CURDIR)" >/dev/null 2>&1 \
			&& echo "  已按当前入口重建 $(APP)" \
			|| { echo "  启动失败，pm2 输出："; $(PM2) start "$(ENTRY)" --name $(APP) --cwd "$(CURDIR)"; exit 1; }; \
	fi

status:
	@echo "$(APP) 运行状态"
	@echo ""
	@if $(PM2) describe $(APP) >/dev/null 2>&1; then \
		$(PM2) status $(APP); \
		$(PM2) describe $(APP) 2>/dev/null | grep -F "script path" | grep -qF "$(ENTRY)" \
			|| echo "  入口路径    与当前仓库不一致（make stop && make start 重建）"; \
	else \
		echo "  进程        未运行（pm2 中无 $(APP)）"; \
	fi
	@echo ""
	@printf "  健康探测    "
	@$(HEALTH_PROBE)
	@echo ""
	@echo "  构建产物    dist: $$([ -f nest/server/dist/main.js ] && echo 已就绪 || echo 缺失)   public: $$([ -f public/assets/main.js ] && echo 已就绪 || echo 缺失)   rust: $$([ -f $(RUST_SERVER_DIR)/target/release/kestrel-studio ] && echo 已就绪 || echo 缺失)"
	@echo ""

logs:
	$(PM2) logs $(APP)

# ===== 质量 =====

# Each package owns its typecheck and its test runner; the unsuffixed targets are
# just the union. Nest server's typecheck covers both tsconfigs (src + tests).
typecheck: typecheck-nest-server typecheck-nest-client typecheck-rust-server typecheck-rust-client

typecheck-nest-server:
	$(PNPM) --dir $(NEST_SERVER_DIR) run typecheck

typecheck-nest-client:
	$(PNPM) --dir $(NEST_CLIENT_DIR) run typecheck

typecheck-rust-server:
	$(CARGO) check $(CARGO_SERVER_ARGS) --tests

typecheck-rust-client:
	$(CARGO) check $(CARGO_CLIENT_ARGS) --tests

# Unit tests cover the pure modules only. See each package's vitest.config.mts for
# why tests needing a Nest container are out of scope for its runner.
test: test-nest-server test-nest-client test-rust-server test-rust-client

test-nest-server:
	$(PNPM) --dir $(NEST_SERVER_DIR) run test

test-nest-client:
	$(PNPM) --dir $(NEST_CLIENT_DIR) run test

test-rust-server:
	$(CARGO) test $(CARGO_SERVER_ARGS)

test-rust-client:
	$(CARGO) test $(CARGO_CLIENT_ARGS)

test-watch:
	$(PNPM) --dir $(NEST_SERVER_DIR) run test:watch

# ===== 清理 =====

clean: clean-nest-client clean-nest-server clean-rust-server clean-rust-client clean-crates

# `dist/` at the root is the pre-restructure Nest output; nothing writes there now,
# but a checkout that predates the move may still carry it.
clean-nest-server:
	rm -rf nest/server/dist dist

# `public/` is the shared asset directory (Vite's outDir, see
# nest/client/vite.config.mts).
clean-nest-client:
	rm -rf public

clean-rust-server:
	$(CARGO) clean $(CARGO_SERVER_ARGS)

# The WASM client's Cargo and wasm-pack outputs (not under rust/server/target).
clean-rust-client:
	rm -rf rust/client/target rust/client/pkg

# A direct `cargo test` on a shared crate writes crates/<name>/target/.
clean-crates:
	rm -rf crates/*/target

# ===== Rust 实现 =====

# `--manifest-path` rather than `cd`: the working directory persists between
# shell calls, so a relative path in the next command would silently aim at the
# wrong tree.

# Foreground on purpose: the pm2 entry stays the Nest process, so this cannot
# take over what `make status` and the health probe describe.
run-rust:
	PORT=$(PORT) $(CARGO) run $(CARGO_SERVER_ARGS) --release

# zig carries a musl sysroot, so `ring`'s C/asm and the bundled SQLite compile
# without installing a cross gcc. cargo-zigbuild does the flag translation between
# Rust's triples and zig's spelling, and links without fighting the startup files
# (see docs/implementation-notes.md §14 for why the hand-rolled shim version of
# this failed). Both are brew installs; zig's bottle is ~37 MB and ghcr.io crawls
# from here, so set HOMEBREW_BOTTLE_DOMAIN to a mirror when installing.
build-rust-server-linux:
	@command -v zig >/dev/null 2>&1 \
		|| { echo "缺 zig —— 它是 x86_64-linux-musl 的 C 交叉编译器。装法：brew install zig"; exit 1; }
	@command -v cargo-zigbuild >/dev/null 2>&1 \
		|| { echo "缺 cargo-zigbuild —— 装法：cargo install cargo-zigbuild --locked"; exit 1; }
	@rustup target list --installed 2>/dev/null | grep -qx $(RUST_LINUX_TARGET) \
		|| { echo "缺编译目标：rustup target add $(RUST_LINUX_TARGET)"; exit 1; }
	CARGO_TARGET_DIR=$(LINUX_TARGET_DIR) $(CARGO) zigbuild $(CARGO_SERVER_ARGS) \
		--release --target $(RUST_LINUX_TARGET)
	@mkdir -p "$$(dirname $(LINUX_BINARY))"
	cp $(LINUX_TARGET_DIR)/$(RUST_LINUX_TARGET)/release/kestrel-studio $(LINUX_BINARY)
	@echo "    静态二进制  $(LINUX_BINARY)"
	@echo "    校验        file -b 应显示 'statically linked'（scratch 镜像靠它）"

# ===== 兼容别名 =====

# Pre-restructure spellings, kept so existing docs, scripts and muscle memory
# keep working. New commands should use the `<verb>-<stack>-<role>` names above.
build-server: build-nest-server
build-web: build-nest-client
build-rust: build-rust-server

typecheck-server: typecheck-nest-server
typecheck-web: typecheck-nest-client
typecheck-rust: typecheck-rust-server

test-server: test-nest-server
test-web: test-nest-client
test-rust: test-rust-server

clean-server: clean-nest-server
clean-web: clean-nest-client
clean-rust: clean-rust-server clean-crates

rust-build: build-rust-server
rust-check: typecheck-rust-server
rust-test: test-rust-server
rust-clean: clean-rust
rust-run: run-rust
rust-build-linux: build-rust-server-linux

# ===== 部署 =====

# Deploy inputs are produced on this machine, never on the host: the static
# binary and the client assets. `build-rust-client` depends on `build-nest-client`
# and builds/stages the WASM client plus its AudioWorklet after Vite empties
# `public/`. Templates still load the TS client (roadmap §4 P3).
deploy: build-rust-server-linux build-rust-client
	./scripts/deploy.sh

deploy-status:
	ssh $(DEPLOY_HOST) "cd $(DEPLOY_DIR) && docker compose ps"

deploy-logs:
	ssh $(DEPLOY_HOST) "cd $(DEPLOY_DIR) && docker compose logs -f --tail=100"

deploy-restart:
	ssh $(DEPLOY_HOST) "cd $(DEPLOY_DIR) && docker compose restart"

deploy-down:
	ssh $(DEPLOY_HOST) "cd $(DEPLOY_DIR) && docker compose down"
