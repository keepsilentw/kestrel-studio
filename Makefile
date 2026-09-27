# kestrel-studio Makefile — 快捷命令入口

SHELL := /bin/bash
.DEFAULT_GOAL := help

PNPM := pnpm
PM2  := pm2
APP  := kestrel-studio

# Local port the app listens on. Keep in sync with src/config/configuration.ts.
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
.PHONY: build build-web build-server
.PHONY: start stop restart status logs
.PHONY: typecheck test test-watch
.PHONY: clean
.PHONY: deploy deploy-status deploy-logs deploy-restart deploy-down

help:
	@echo "kestrel-studio — 可用命令:"
	@echo ""
	@echo "  安装"
	@echo "    make install                安装依赖"
	@echo ""
	@echo "  开发"
	@echo "    make dev                    启动后端 watch（nest start --watch）"
	@echo "    make web-watch              前端资源 watch 构建（开发时另开终端）"
	@echo ""
	@echo "  构建"
	@echo "    make build                  构建前端资源 + 后端"
	@echo "    make build-web              仅构建前端资源"
	@echo "    make build-server           仅构建后端"
	@echo ""
	@echo "  运行（pm2 守护）"
	@echo "    make start                  构建并一键启动，打印访问信息"
	@echo "    make stop                   停止服务"
	@echo "    make restart                重启服务（不重新构建）"
	@echo "    make status                 查看服务运行状态与健康探测"
	@echo "    make logs                   跟随服务日志"
	@echo ""
	@echo "  质量"
	@echo "    make typecheck              类型检查（后端 + 前端脚本 + 测试）"
	@echo "    make test                   运行单元测试（纯模块，无 DI）"
	@echo "    make test-watch             单元测试 watch 模式"
	@echo ""
	@echo "  清理"
	@echo "    make clean                  清理构建产物（保留 data/ 与 storage/）"
	@echo ""
	@echo "  部署"
	@echo "    make deploy                 一键部署到 $(DEPLOY_HOST)"
	@echo "    make deploy-status          查看远端容器状态"
	@echo "    make deploy-logs            跟随远端容器日志"
	@echo "    make deploy-restart         重启远端容器"
	@echo "    make deploy-down            停止并移除远端容器（保留 data/ 与 storage/）"

# ===== 安装 =====

install:
	$(PNPM) install

# ===== 开发 =====

dev:
	@if $(PM2) describe $(APP) >/dev/null 2>&1; then \
		echo "注意: pm2 进程 $(APP) 正在运行并占用 $(PORT) 端口，watch 模式会启动失败。"; \
		echo "      先执行 make stop，或直接用 make start / make restart。"; \
		echo ""; \
	fi
	$(PNPM) dev

web-watch:
	$(PNPM) web:watch

# ===== 构建 =====

build: build-web build-server

build-web:
	$(PNPM) web:build

build-server:
	$(PNPM) build

# ===== 运行（pm2 守护） =====

# The frontend has no runtime process — Vite only emits assets that Nest serves —
# so "starting web and server" means building the assets and putting the Nest
# process under pm2.
start: build
	@echo ""
	@echo "==> 启动 $(APP)"
	@if $(PM2) describe $(APP) >/dev/null 2>&1; then \
		$(PM2) restart $(APP) --update-env >/dev/null 2>&1 \
			&& echo "    已重启现有 pm2 进程" \
			|| { echo "    重启失败，pm2 输出："; $(PM2) restart $(APP) --update-env; exit 1; }; \
	else \
		$(PM2) start "$(CURDIR)/dist/main.js" --name $(APP) --cwd "$(CURDIR)" >/dev/null 2>&1 \
			&& echo "    已创建 pm2 进程" \
			|| { echo "    启动失败，pm2 输出："; $(PM2) start "$(CURDIR)/dist/main.js" --name $(APP) --cwd "$(CURDIR)"; exit 1; }; \
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
	@if $(PM2) describe $(APP) >/dev/null 2>&1; then \
		$(PM2) restart $(APP) --update-env >/dev/null 2>&1; \
		echo "  已重启 $(APP)"; \
	else \
		echo "  $(APP) 未在运行，请用 make start"; \
	fi

status:
	@echo "$(APP) 运行状态"
	@echo ""
	@if $(PM2) describe $(APP) >/dev/null 2>&1; then \
		$(PM2) status $(APP); \
	else \
		echo "  进程        未运行（pm2 中无 $(APP)）"; \
	fi
	@echo ""
	@printf "  健康探测    "
	@$(HEALTH_PROBE)
	@echo ""
	@echo "  构建产物    dist: $$([ -f dist/main.js ] && echo 已就绪 || echo 缺失)   public: $$([ -f public/assets/main.js ] && echo 已就绪 || echo 缺失)"
	@echo ""

logs:
	$(PM2) logs $(APP)

# ===== 质量 =====

typecheck:
	$(PNPM) typecheck
	$(PNPM) run typecheck:web
	$(PNPM) run typecheck:test

# Unit tests cover the pure modules only. See vitest.config.mts for why tests
# needing a Nest container are out of scope for this runner.
test:
	$(PNPM) test

test-watch:
	$(PNPM) test:watch

# ===== 清理 =====

clean:
	rm -rf dist public

# ===== 部署 =====

deploy:
	./scripts/deploy.sh

deploy-status:
	ssh $(DEPLOY_HOST) "cd $(DEPLOY_DIR) && docker compose ps"

deploy-logs:
	ssh $(DEPLOY_HOST) "cd $(DEPLOY_DIR) && docker compose logs -f --tail=100"

deploy-restart:
	ssh $(DEPLOY_HOST) "cd $(DEPLOY_DIR) && docker compose restart"

deploy-down:
	ssh $(DEPLOY_HOST) "cd $(DEPLOY_DIR) && docker compose down"
