# kestrel-studio Makefile — 快捷命令入口
# kestrel-studio Makefile — the single command entry point for the monorepo.

SHELL := /bin/bash
.DEFAULT_GOAL := help

PNPM := pnpm
PM2  := pm2
APP  := kestrel-studio

# Repository layout (monorepo, packaged by backend technology stack):
#
#   nest/server/   NestJS backend  (package.json, src/, __tests__/, configs, dist/)
#   nest/client/   Nest browser    (scripts/, styles/, public/, vite.config.mts)
#   rust/server/   Rust backend    (Cargo package `kestrel-studio`)
#   rust/client/   Rust browser    (Cargo package `kestrel-client`, compiles to WASM)
#   crates/        shared Rust     (proto/, md/)
#   public/        build output    — the asset directory BOTH servers serve
#
# The root package.json is the pnpm workspace root and declares no dependencies.
# Node commands run per package (`pnpm --dir nest/server run build`); the nest and
# drizzle scripts `cd ../..` first because `.env`, `DATABASE_FILE` and
# `STORAGE_DIR` are resolved from the working directory, which must stay the repo
# root. Only cross-package orchestration lives here — per-package commands can be
# run directly with pnpm/cargo when a single target is genuinely needed.

CARGO ?= cargo
NEST_SERVER_DIR ?= $(CURDIR)/nest/server
NEST_CLIENT_DIR ?= $(CURDIR)/nest/client
RUST_SERVER_DIR ?= $(CURDIR)/rust/server
RUST_CLIENT_DIR ?= $(CURDIR)/rust/client
CARGO_SERVER_ARGS := --manifest-path $(RUST_SERVER_DIR)/Cargo.toml
CARGO_CLIENT_ARGS := --manifest-path $(RUST_CLIENT_DIR)/Cargo.toml

# The pm2 entry must point here. `start` and `restart` compare it against the
# entry pm2 actually holds and rebuild the entry when the repository moved — a
# plain `pm2 restart` keeps the old `exec path` and leaves a "running" process
# with no pid.
ENTRY ?= $(CURDIR)/nest/server/dist/main.js

# Local port the app listens on. Keep in sync with nest/server/src/config/configuration.ts.
PORT ?= 9949

# The deploy binary is cross-compiled, never built on the host: a static musl
# ELF needs no glibc there and no toolchain there.
RUST_LINUX_TARGET ?= x86_64-unknown-linux-musl
# Outside the working tree, twice over: the cargo target dir and the final
# binary. Both are reachable by deploy.sh, which assembles the docker context
# under /tmp rather than next to source files.
LINUX_TARGET_DIR ?= /tmp/kestrel-studio-linux-target
LINUX_BINARY     ?= /tmp/kestrel-studio-linux/kestrel-studio

DEPLOY_HOST ?= lavo-test
DEPLOY_DIR  ?= /opt/kestrel-studio

# Health probe shared by `start` and `status`, so the two cannot drift.
define HEALTH_PROBE
probe=$$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 "http://127.0.0.1:$(PORT)/login" 2>/dev/null); \
if [ "$$probe" = "200" ]; then echo "正常  HTTP 200"; \
elif [ -z "$$probe" ] || [ "$$probe" = "000" ]; then echo "无响应（服务未启动？）"; \
else echo "异常  HTTP $$probe"; fi
endef

.PHONY: help install
.PHONY: dev watch
.PHONY: build build-nest build-web build-rust build-wasm
.PHONY: start stop restart status logs
.PHONY: check test test-watch
.PHONY: clean run-rust build-linux
.PHONY: deploy deploy-status deploy-logs deploy-restart deploy-down
.PHONY: census census-prod

help:
	@echo "kestrel-studio — 可用命令:"
	@echo ""
	@echo "  安装 / 开发"
	@echo "    make install                安装 workspace 依赖（nest/server + nest/client）"
	@echo "    make dev                    Nest 后端 watch（nest start --watch）"
	@echo "    make watch                  Nest 客户端资源 watch 构建（另开终端）"
	@echo ""
	@echo "  构建"
	@echo "    make build                  全部包（Nest client/server + Rust client/server）"
	@echo "    make build-nest             仅 Nest 后端"
	@echo "    make build-web              仅 Nest 客户端资源（→ public/assets）"
	@echo "    make build-rust             仅 Rust release 二进制"
	@echo "    make build-wasm             仅 WASM 客户端（→ public/wasm；页面是否加载它由 CLIENT_BUNDLE 决定，见 docs/deployment.md §2.2）"
	@echo ""
	@echo "  运行（pm2 守护 Nest）"
	@echo "    make start                  构建 Nest 前后端并启动，打印访问信息"
	@echo "    make stop / restart / status / logs"
	@echo ""
	@echo "  质量"
	@echo "    make check                  全部包（Nest 三份 tsconfig + 两个 cargo check）"
	@echo "    make test                   全部包单元测试（Nest 两轮 vitest + 两个 cargo test）"
	@echo "    make test-watch             Nest 后端测试 watch"
	@echo "    make census / census-prod   数一遍助手消息里的 markdown 构造（子集取舍的哨兵，只读）"
	@echo "    make clean                  清全部产物（保留 data/ 与 storage/）"
	@echo ""
	@echo "  Rust / 部署"
	@echo "    make run-rust               本机前台运行 Rust 服务（占用 $(PORT)）"
	@echo "    make build-linux            交叉编译静态 musl 二进制（需 zig + cargo-zigbuild）"
	@echo "    make deploy                 一键部署到 $(DEPLOY_HOST)"
	@echo "    make deploy-status / deploy-logs / deploy-restart / deploy-down"
	@echo "    make deploy-nest            打包并构建 Nest 那套的镜像（不启动，见目标注释）"
	@echo "    make nest-up / nest-down / nest-logs   第二后端的启停与日志（共用同一个库，先读 §13.2）"
	@echo ""
	@echo "  单个包的 check/test 直接调 pnpm --dir 或 cargo（这里只保留跨包编排）。"

# ===== 安装 / 开发 =====

install:
	$(PNPM) install

dev:
	@if $(PM2) describe $(APP) >/dev/null 2>&1; then \
		echo "注意: pm2 进程 $(APP) 正在运行并占用 $(PORT) 端口，watch 模式会启动失败。"; \
		echo "      先执行 make stop，或直接用 make start / make restart。"; \
		echo ""; \
	fi
	$(PNPM) --dir $(NEST_SERVER_DIR) run dev

watch:
	$(PNPM) --dir $(NEST_CLIENT_DIR) run watch

# ===== 构建 =====

build: build-web build-nest build-rust build-wasm

build-web:
	$(PNPM) --dir $(NEST_CLIENT_DIR) run build

build-nest:
	$(PNPM) --dir $(NEST_SERVER_DIR) run build

build-rust:
	$(CARGO) build $(CARGO_SERVER_ARGS) --release
	@echo "    二进制      $(RUST_SERVER_DIR)/target/release/kestrel-studio"

# wasm-pack output, staged under `public/wasm/`. Templates still load
# `/assets/main.js` and `/assets/voice.js` until roadmap §4 P3 wires
# `initChat` / `initVoice`. Order matters: Vite empties `public/` first.
build-wasm: build-web
	cd $(RUST_CLIENT_DIR) && wasm-pack build --release --target web --out-dir pkg
	mkdir -p $(CURDIR)/public/wasm
	cp -R $(RUST_CLIENT_DIR)/pkg/. $(CURDIR)/public/wasm/
	cp $(RUST_CLIENT_DIR)/public/capture.js $(CURDIR)/public/capture.js
	@echo "    WASM          $(CURDIR)/public/wasm/kestrel_client_bg.wasm"

# ===== 运行（pm2 守护） =====

# `--cwd` stays the repo root: `.env`, `data/` and `storage/` are resolved from
# the working directory. `start` builds only the Nest stack; the Rust binary is
# the other server (run-rust / deploy) and has no place in the pm2 entry.
start: build-web build-nest
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
	@echo "    访问        http://localhost:$(PORT)"
	@echo "    初始账号    admin / 123456"
	@echo "    状态        make status    日志  make logs    停止  make stop"
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

check:
	$(PNPM) --dir $(NEST_SERVER_DIR) run typecheck
	$(PNPM) --dir $(NEST_CLIENT_DIR) run typecheck
	$(CARGO) check $(CARGO_SERVER_ARGS) --tests
	$(CARGO) check $(CARGO_CLIENT_ARGS) --tests

test:
	$(PNPM) --dir $(NEST_SERVER_DIR) run test
	$(PNPM) --dir $(NEST_CLIENT_DIR) run test
	$(CARGO) test $(CARGO_SERVER_ARGS)
	$(CARGO) test $(CARGO_CLIENT_ARGS)

test-watch:
	$(PNPM) --dir $(NEST_SERVER_DIR) run test:watch

# `dist/` at the root is the pre-restructure Nest output; kept so an older
# checkout cleans it too. `public/` is the shared asset directory.
clean:
	rm -rf public nest/server/dist dist rust/client/target rust/client/pkg crates/*/target
	$(CARGO) clean $(CARGO_SERVER_ARGS)

# ===== Rust / 部署 =====

# Foreground on purpose: the pm2 entry stays the Nest process.
run-rust:
	PORT=$(PORT) $(CARGO) run $(CARGO_SERVER_ARGS) --release

# zig carries a musl sysroot, so `ring`'s C/asm and the bundled SQLite compile
# without installing a cross gcc. cargo-zigbuild translates the flags.
build-linux:
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

# Deploy inputs are produced on this machine, never on the host. `build-wasm`
# depends on `build-web` and stages the WASM client after Vite empties
# `public/`; templates still load the TS client (roadmap §4 P3).
deploy: build-linux build-wasm
	./scripts/deploy.sh

# The second backend (roadmap.md §4.5). This ships the context and builds the
# image, then stops: the two processes share one SQLite file and one storage
# directory, and three of their writes carry no process ownership — the boot
# finalizer, the task worker and the per-conversation `seq` (architecture.md
# §13.2). Starting it is therefore `make nest-up`, a separate deliberate step, not
# a side effect of a deploy.
deploy-nest: build-nest
	./scripts/deploy-nest.sh

nest-up:
	ssh $(DEPLOY_HOST) "cd $(DEPLOY_DIR) && docker compose --profile nest up -d nest"

nest-down:
	ssh $(DEPLOY_HOST) "cd $(DEPLOY_DIR) && docker compose --profile nest stop nest"

nest-logs:
	ssh $(DEPLOY_HOST) "docker logs --tail=100 -f kestrel-studio-nest"

# The subset in crates/md is a deliberate choice, so "does the assistant actually
# write tables / images / task lists" is a question to measure, not to remember.
# Open the live database read-only: the server holds it, and this must never write.
census:
	sqlite3 "file:$(CURDIR)/data/kestrel-studio.db?mode=ro" ".read scripts/markdown-census.sql"

# The same census against the production copy. The SQL goes in over stdin so nothing
# is left behind on the host.
census-prod:
	ssh $(DEPLOY_HOST) 'sqlite3 "file:$(DEPLOY_DIR)/data/kestrel-studio.db?mode=ro"' < scripts/markdown-census.sql

deploy-status:
	# --all so a service started under the `nest` profile shows up here too; without
	# it compose lists only the default-profile services and the second backend is
	# invisible to whoever is checking.
	ssh $(DEPLOY_HOST) "cd $(DEPLOY_DIR) && docker compose ps --all"

deploy-logs:
	ssh $(DEPLOY_HOST) "cd $(DEPLOY_DIR) && docker compose logs -f --tail=100"

deploy-restart:
	ssh $(DEPLOY_HOST) "cd $(DEPLOY_DIR) && docker compose restart"

deploy-down:
	ssh $(DEPLOY_HOST) "cd $(DEPLOY_DIR) && docker compose down"
