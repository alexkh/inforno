# Inforno workspace tasks: run `make <target>` from the workspace root.
# Recipe lines start with a TAB, not spaces.

CARGO    ?= cargo
PKG      := inforno
ICON     := inforno_gui/assets/icon.ico
WIN_GNU  := x86_64-pc-windows-gnu
WIN_MSVC := x86_64-pc-windows-msvc

# `cargo tree` for the app, over every target platform at once (Linux,
# Windows, and the web build later).
TREE = $(CARGO) tree -p $(PKG) --target all

.PHONY: run build win xwin test test_vfsmask check-no-fetch

run: check-no-fetch
	$(CARGO) run --release -p $(PKG)

build: check-no-fetch
	$(CARGO) build --release -p $(PKG)

win: check-no-fetch
	$(CARGO) build --release -p $(PKG) --target $(WIN_GNU)
	rcedit target/$(WIN_GNU)/release/inforno.exe --set-icon $(ICON)

xwin: check-no-fetch
	$(CARGO) xwin build --release -p $(PKG) --target $(WIN_MSVC)
	rcedit target/$(WIN_MSVC)/release/inforno.exe --set-icon $(ICON)

# Every crate's tests, not only those of the default member (inforno_gui).
test:
	$(CARGO) test --release --workspace -- --nocapture

test_vfsmask:
	$(CARGO) test --release -p inforno_core test_vfsmask_permissions -- --nocapture

# Fails if an http image loader is back in the build: egui_extras' "http",
# egui_commonmark's "fetch", or the ehttp crate they download with. Cargo
# merges features across the whole dependency tree, so any crate can bring
# them back. Nothing a message renders may reach the network.
check-no-fetch:
	@if $(TREE) -e features -i egui_extras | grep -q 'feature "http"'; then \
		echo "egui_extras' http loader is back:"; \
		$(TREE) -e features -i egui_extras; \
		exit 1; \
	fi
	@if $(TREE) -e features -i egui_commonmark | grep -q 'feature "fetch"'; then \
		echo "egui_commonmark's fetch is back:"; \
		$(TREE) -e features -i egui_commonmark; \
		exit 1; \
	fi
	@if $(TREE) -i ehttp >/dev/null 2>&1; then \
		echo "ehttp is back:"; \
		$(TREE) -i ehttp; \
		exit 1; \
	fi
	@echo "OK: no http image loader in the build"
