_default:
    @just --list --unsorted

[group('dev')]
fmt *args:
    cargo fmt {{args}}

[group('dev')]
lint:
    cargo clippy --release -- -D warnings

[group('dev')]
test:
    cargo test

# Run all checks (before commit, push, merge, release)
[group('dev')]
@checks: (fmt "--check") lint test

[group('dev')]
run:
    cargo run --bin mujina-minerd

BUILD_IMAGE := "mujina-build"
BUILD_TAG := `sha256sum build.Containerfile | cut -c1-12`

# Build the build toolchain image (skips if unchanged)
[group('container')]
build-image:
    podman image exists {{BUILD_IMAGE}}:{{BUILD_TAG}} || \
        podman build -t {{BUILD_IMAGE}}:{{BUILD_TAG}} -f build.Containerfile .

# Run a just recipe inside the build toolchain image
[group('container')]
in-container *args: build-image
    podman run --rm \
        -v "$(pwd)":/workspace:Z \
        -v mujina-cargo-registry:/usr/local/cargo/registry \
        -v mujina-cargo-git:/usr/local/cargo/git \
        -w /workspace \
        {{BUILD_IMAGE}}:{{BUILD_TAG}} \
        just {{args}}

# The CI pipeline. This is what GitHub Actions runs.
[group('ci')]
ci: (in-container "checks")

[group('container')]
container-build tag=`git rev-parse --abbrev-ref HEAD`:
    podman build -t mujina-minerd:{{tag}} -f Containerfile .

[group('container')]
container-push tag=`git rev-parse --abbrev-ref HEAD`:
    podman tag mujina-minerd:{{tag}} ghcr.io/256foundation/mujina-minerd:{{tag}}
    podman push ghcr.io/256foundation/mujina-minerd:{{tag}}

[group('setup')]
hooks:
    ./scripts/setup-hooks.sh