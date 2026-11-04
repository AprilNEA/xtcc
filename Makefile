.PHONY: all rust native check clean

CARGO ?= $(HOME)/.cargo/bin/cargo
MODULE := libxtcc_inject.dylib
MODULE_INPUTS := native/tccd_inject.m native/Attribution.m native/Attribution.h native/ControlProtocol.h

all: rust native

rust:
	$(CARGO) build --release --locked

native: $(MODULE)

# 先签名临时文件，再原子替换，避免截断已映射的库。
$(MODULE): $(MODULE_INPUTS) Makefile
	@set -eu; \
	temporary=$$(mktemp .build-module.XXXXXX); \
	trap 'rm -f "$$temporary"' EXIT HUP INT TERM; \
	/usr/bin/xcrun clang -arch arm64e -fobjc-arc -framework Foundation -lbsm \
		-mmacosx-version-min=15.0 -O2 -Wall -Wextra -Werror -dynamiclib \
		-Wl,-install_name,@rpath/$(MODULE) \
		-o "$$temporary" native/tccd_inject.m native/Attribution.m; \
	/usr/bin/codesign --force --sign - --identifier dev.xtcc "$$temporary"; \
	chmod 755 "$$temporary"; \
	mv -f "$$temporary" "$(MODULE)"

check:
	$(CARGO) fmt --check
	$(CARGO) clippy --all-targets --locked -- -D warnings
	$(CARGO) test --locked

# 不终止服务，不改变已加载模块。
clean:
	$(CARGO) clean
	rm -f $(MODULE)
