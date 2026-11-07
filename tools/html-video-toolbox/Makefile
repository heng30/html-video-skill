#!/usr/bin/env bash

app-name = html-video-toolbox
run-env = RUST_LOG=
build-env =

all: release

build:
	$(build-env) cargo build --bin ${app-name}

release:
	$(build-env) cargo build --release --bin ${app-name}

debug:
	$(build-env) $(run-env) cargo run --bin ${app-name}

install-linux: release
	cp -f ~/.local/bin/${app-name} ~/.local/bin/${app-name}.bak 2>/dev/null || true
	cp -f ./target/release/${app-name} ~/.local/bin/

check:
	$(build-env) cargo check

clean:
	cargo clean
