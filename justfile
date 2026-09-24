default: lint test

fmt:
    cargo fmt --all

lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets

test *args:
    cargo nextest run --workspace {{args}}
