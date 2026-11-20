# Inforno workspace tasks: run `make <target>` from the workspace root.
# Recipe lines start with a TAB, not spaces. The checks live in xtask
# (`cargo xtask check`); every build here goes through them.

CARGO    ?= cargo
PKG      := inforno
ICON     := inforno_gui/assets/icon.ico
WIN_GNU  := x86_64-pc-windows-gnu
WIN_MSVC := x86_64-pc-windows-msvc
# Options for the signing tasks, e.g. make release XTASK_ARGS="--days 30"
XTASK_ARGS ?=

.PHONY: run build release sign check win xwin test test_vfsmask

run: check
	$(CARGO) run --release -p $(PKG)

build: check
	$(CARGO) build --release -p $(PKG)

# Release binaries plus signed vaultorno statements (xtask checks first).
release:
	$(CARGO) xtask build $(XTASK_ARGS)

# Signs the statements of the release binaries already built.
sign:
	$(CARGO) xtask sign $(XTASK_ARGS)

check:
	$(CARGO) xtask check

win: check
	$(CARGO) build --release -p $(PKG) --target $(WIN_GNU)
	rcedit target/$(WIN_GNU)/release/inforno.exe --set-icon $(ICON)

xwin: check
	$(CARGO) xwin build --release -p $(PKG) --target $(WIN_MSVC)
	rcedit target/$(WIN_MSVC)/release/inforno.exe --set-icon $(ICON)

# Every crate's tests, not only those of the default member (inforno_gui).
test:
	$(CARGO) test --release --workspace -- --nocapture

test_vfsmask:
	$(CARGO) test --release -p inforno_core test_vfsmask_permissions -- --nocapture
