.PHONY: all rust native check clean

CARGO ?= $(HOME)/.cargo/bin/cargo
MODULES := libxtcc_inject.dylib libxtcc_ne.dylib

all: rust native

rust:
	$(CARGO) build --release --locked

native: $(MODULES)

libxtcc_inject.dylib: native/tccd_inject.m native/Attribution.m native/Attribution.h native/ControlProtocol.h
libxtcc_inject.dylib: NATIVE_LIBS := -lbsm
libxtcc_ne.dylib: native/ne.m native/ne/connection.m native/ne/connection.h
libxtcc_ne.dylib: NATIVE_LIBS := -framework NetworkExtension -framework SystemExtensions -framework Security -framework SystemConfiguration

# Sign the temporary file, then replace atomically to avoid truncating a mapped library.
$(MODULES): Makefile
	@set -eu; \
	temporary=$$(mktemp .build-module.XXXXXX); \
	trap 'rm -f "$$temporary"' EXIT HUP INT TERM; \
	/usr/bin/xcrun clang -arch arm64e -fobjc-arc -framework Foundation $(NATIVE_LIBS) \
		-mmacosx-version-min=15.0 -O2 -Wall -Wextra -Werror -dynamiclib \
		-Wl,-install_name,@rpath/$@ \
		-o "$$temporary" $(filter %.m,$^); \
	/usr/bin/codesign --force --sign - --identifier dev.xtcc "$$temporary"; \
	chmod 755 "$$temporary"; \
	mv -f "$$temporary" "$@"

check:
	$(CARGO) fmt --check
	$(CARGO) clippy --all-targets --locked -- -D warnings
	$(CARGO) test --locked

# Do not stop services or alter loaded modules.
clean:
	$(CARGO) clean
	rm -f $(MODULES)
