# mulot-scan — génération des versions publiées (Linux et Windows 11)
#
#   make release              Linux portable (Docker) + Windows 11 + sommes SHA-256
#   make linux                Linux x86_64 compilé sur cette machine (glibc locale)
#   make linux-portable       Linux x86_64 compilé dans Docker (Debian 12, glibc ≥ 2.35 requise)
#   make windows ACCEPT_MSVC_LICENSE=1
#                             Windows 11 x86_64 (cible MSVC, liée avec lld-link)
#   make windows WINDOWS_ABI=gnu
#                             Windows 11 x86_64 avec MinGW-w64 (paquet mingw-w64-gcc)
#   make install [PREFIX=~/.local]
#                             installe le binaire, l'icône et le raccourci (Linux)
#   make clean | cleanall | distclean
#                             nettoyage (voir la fin du Makefile)
#   make test | help
#
# Les archives sont écrites dans dist/.

NAME     := mulot-scan
VERSION  := $(shell sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n1)
DIST     := dist
BUILD    := build
ABS_BUILD = $(abspath $(BUILD))

CARGO    ?= cargo
DOCS     := README.md docs/PROTOCOLE.md

# ---------------------------------------------------------------- Linux
LINUX_TARGET  := x86_64-unknown-linux-gnu
LINUX_PKG     := $(NAME)-$(VERSION)-linux-x86_64
DOCKER        ?= docker
RUST_IMAGE    ?= rust:1-bookworm
DOCKER_TAG    := $(NAME)-builder:bookworm

# ---------------------------------------------------------------- Windows
WINDOWS_ABI    ?= msvc
WINDOWS_TARGET := x86_64-pc-windows-$(WINDOWS_ABI)
WINDOWS_PKG    := $(NAME)-$(VERSION)-windows-x86_64
WIN_TARGET_DIR := $(BUILD)/target-windows-$(WINDOWS_ABI)
WIN_EXE        := $(WIN_TARGET_DIR)/$(WINDOWS_TARGET)/release/$(NAME).exe

# Sans rustup, on utilise un compilateur officiel téléchargé dans build/toolchain
# (la bibliothèque standard Windows officielle n'est pas compatible avec le rustc
# des distributions).
HAVE_RUSTUP := $(shell command -v rustup 2>/dev/null)
ifeq ($(HAVE_RUSTUP),)
  WIN_RUSTC := $(ABS_BUILD)/toolchain/bin/rustc
else
  WIN_RUSTC := rustc
endif

XWIN_DIR := $(ABS_BUILD)/xwin
XWIN_BIN := $(ABS_BUILD)/tools/bin/xwin

ifeq ($(WINDOWS_ABI),msvc)
  WIN_ENV := CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER=lld-link \
             CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="-C target-feature=+crt-static \
               -Lnative=$(XWIN_DIR)/crt/lib/x86_64 \
               -Lnative=$(XWIN_DIR)/sdk/lib/um/x86_64 \
               -Lnative=$(XWIN_DIR)/sdk/lib/ucrt/x86_64"
  WIN_DEPS := windows-sdk
else ifeq ($(WINDOWS_ABI),gnu)
  WIN_ENV  := CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
  WIN_DEPS := mingw-check
else
  $(error WINDOWS_ABI doit valoir msvc ou gnu)
endif

PREFIX ?= $(HOME)/.local

.PHONY: install uninstall
install:
	@test -x target/release/$(NAME) || $(CARGO) build --release --locked
	install -Dm755 target/release/$(NAME) $(PREFIX)/bin/$(NAME)
	install -Dm644 packaging/linux/mulot-scan.desktop $(PREFIX)/share/applications/mulot-scan.desktop
	install -Dm644 assets/icon.svg $(PREFIX)/share/icons/hicolor/scalable/apps/mulot-scan.svg
	install -Dm644 assets/icon-256.png $(PREFIX)/share/icons/hicolor/256x256/apps/mulot-scan.png
	install -Dm644 assets/icon-512.png $(PREFIX)/share/icons/hicolor/512x512/apps/mulot-scan.png
	-gtk-update-icon-cache -q $(PREFIX)/share/icons/hicolor 2>/dev/null
	@echo "Installé dans $(PREFIX)"

uninstall:
	rm -f $(PREFIX)/bin/$(NAME) $(PREFIX)/share/applications/mulot-scan.desktop \
	      $(PREFIX)/share/icons/hicolor/scalable/apps/mulot-scan.svg \
	      $(PREFIX)/share/icons/hicolor/256x256/apps/mulot-scan.png \
	      $(PREFIX)/share/icons/hicolor/512x512/apps/mulot-scan.png

.PHONY: help release linux linux-portable windows windows-toolchain windows-sdk \
        mingw-check checksums test clean cleanall distclean

help:
	@sed -n '2,16s/^# \{0,1\}//p' Makefile

release: linux-portable windows checksums
	@echo "Versions publiées dans $(DIST)/ :"
	@ls -lh $(DIST)

test:
	$(CARGO) test --release

# ---------------------------------------------------------------- Linux

linux:
	$(CARGO) build --release --locked --target $(LINUX_TARGET)
	$(MAKE) --no-print-directory package-linux BIN=target/$(LINUX_TARGET)/release/$(NAME)
	@echo "Attention : ce binaire exige la glibc de cette machine" \
	      "($$(ldd --version | head -n1 | awk '{print $$NF}')). Utilisez 'make linux-portable' pour diffuser."

linux-portable:
	$(DOCKER) build -q -t $(DOCKER_TAG) --build-arg RUST_IMAGE=$(RUST_IMAGE) packaging/linux >/dev/null
	mkdir -p $(BUILD)/docker-cargo $(BUILD)/target-docker
	$(DOCKER) run --rm -u $$(id -u):$$(id -g) \
	    -e HOME=/tmp -e CARGO_HOME=/src/$(BUILD)/docker-cargo \
	    -v "$(CURDIR)":/src -w /src $(DOCKER_TAG) \
	    cargo build --release --locked --target $(LINUX_TARGET) --target-dir $(BUILD)/target-docker
	$(MAKE) --no-print-directory package-linux BIN=$(BUILD)/target-docker/$(LINUX_TARGET)/release/$(NAME)

.PHONY: package-linux
package-linux:
	rm -rf $(BUILD)/$(LINUX_PKG) && mkdir -p $(BUILD)/$(LINUX_PKG) $(DIST)
	cp $(BIN) $(BUILD)/$(LINUX_PKG)/$(NAME)
	cp $(DOCS) packaging/linux/mulot-scan.desktop $(BUILD)/$(LINUX_PKG)/
	cp assets/icon-256.png $(BUILD)/$(LINUX_PKG)/mulot-scan.png
	cp assets/icon.svg $(BUILD)/$(LINUX_PKG)/mulot-scan.svg
	tar -czf $(DIST)/$(LINUX_PKG).tar.gz -C $(BUILD) $(LINUX_PKG)
	@echo "→ $(DIST)/$(LINUX_PKG).tar.gz"

# ---------------------------------------------------------------- Windows 11

windows-toolchain:
ifeq ($(HAVE_RUSTUP),)
	sh packaging/bootstrap-toolchain.sh $(BUILD)/toolchain $(WINDOWS_TARGET)
else
	rustup target add $(WINDOWS_TARGET)
endif

# Bibliothèques du SDK Windows et du runtime MSVC, téléchargées depuis les
# serveurs de Microsoft par xwin. Leur utilisation est soumise à la licence
# Microsoft : elle doit être acceptée explicitement avec ACCEPT_MSVC_LICENSE=1.
windows-sdk:
	@if [ ! -d "$(XWIN_DIR)/sdk" ]; then \
	    if [ "$(ACCEPT_MSVC_LICENSE)" != "1" ]; then \
	        echo "La cible Windows MSVC télécharge le SDK Windows et le runtime MSVC de Microsoft."; \
	        echo "Lisez la licence (https://go.microsoft.com/fwlink/?LinkId=2086102) puis relancez avec :"; \
	        echo "    make windows ACCEPT_MSVC_LICENSE=1"; \
	        echo "ou utilisez MinGW : make windows WINDOWS_ABI=gnu"; \
	        exit 1; \
	    fi; \
	    command -v lld-link >/dev/null || { echo "lld-link introuvable (paquet lld)"; exit 1; }; \
	    [ -x "$(XWIN_BIN)" ] || $(CARGO) install xwin --locked --root $(ABS_BUILD)/tools; \
	    "$(XWIN_BIN)" --accept-license --arch x86_64 --cache-dir $(ABS_BUILD)/xwin-cache \
	        splat --output $(XWIN_DIR); \
	fi

mingw-check:
	@command -v x86_64-w64-mingw32-gcc >/dev/null || { \
	    echo "x86_64-w64-mingw32-gcc introuvable : installez MinGW-w64 (Arch : sudo pacman -S mingw-w64-gcc)."; \
	    exit 1; }

windows: windows-toolchain $(WIN_DEPS)
	RUSTC=$(WIN_RUSTC) $(WIN_ENV) \
	    $(CARGO) build --release --locked --target $(WINDOWS_TARGET) --target-dir $(WIN_TARGET_DIR)
	rm -rf $(BUILD)/$(WINDOWS_PKG) && mkdir -p $(BUILD)/$(WINDOWS_PKG) $(DIST)
	cp $(WIN_EXE) $(BUILD)/$(WINDOWS_PKG)/
	cp $(DOCS) $(BUILD)/$(WINDOWS_PKG)/
	rm -f $(DIST)/$(WINDOWS_PKG).zip
	cd $(BUILD) && zip -qr ../$(DIST)/$(WINDOWS_PKG).zip $(WINDOWS_PKG)
	@echo "→ $(DIST)/$(WINDOWS_PKG).zip"

# ---------------------------------------------------------------- divers

checksums:
	cd $(DIST) && sha256sum *.tar.gz *.zip 2>/dev/null > SHA256SUMS; cat SHA256SUMS

# Nettoyage, du plus léger au plus complet :
#   clean      archives et dossiers de préparation (dist/)
#   cleanall   + toutes les compilations (target/, build/target-*, caches)
#              ; garde le compilateur officiel et le SDK Windows téléchargés
#   distclean  + compilateur officiel, SDK Windows et outils téléchargés
clean:
	rm -rf $(DIST) $(BUILD)/$(NAME)-*

cleanall: clean
	$(CARGO) clean
	rm -rf $(BUILD)/target-* $(BUILD)/docker-cargo $(BUILD)/xwin-cache

distclean: cleanall
	rm -rf $(BUILD)
