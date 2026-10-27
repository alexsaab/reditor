.PHONY: install plugins build test check fmt

install:
	sh ./install.sh

plugins:
	cargo run --release --locked -- --install-all-plugins --scope user

build:
	cargo build --release --locked

check:
	cargo check --all-targets --locked

test:
	cargo test --locked

fmt:
	cargo fmt
