.PHONY: all rust rebuild check clean

CARGO ?= cargo

all: rust

rust:
	$(CARGO) build --release --locked

rebuild:
	$(CARGO) clean --release --package xtcc
	$(MAKE) rust

check:
	$(CARGO) fmt --check
	$(CARGO) clippy --all-targets --locked -- -D warnings
	$(CARGO) test --locked

# Do not stop services or alter loaded modules.
clean:
	$(CARGO) clean
