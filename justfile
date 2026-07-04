dev:
    cargo watch -x run

build:
    cargo build

test:
    cargo test

check:
    cargo clippy -- -D warnings

fmt:
    cargo fmt --all

clean:
    cargo clean
