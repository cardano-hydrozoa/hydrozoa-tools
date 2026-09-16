# Everything here assumes the flake devshell. Outside it, librocksdb-sys has no
# ROCKSDB_LIB_DIR and recompiles its bundled C++ source -- minutes and ~950 MB
# per build configuration. Reproduce CI with `nix develop --command just test`.

default:
    @just --list

build:
    cargo build --workspace

test:
    cargo test --workspace

lint:
    cargo clippy --all-targets --all-features -- -D warnings

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

# What CI gates on.
ci: fmt-check lint test

# The dashboard, against a head on localhost:8080.
top *ARGS:
    cargo run -p hztop -- {{ARGS}}
