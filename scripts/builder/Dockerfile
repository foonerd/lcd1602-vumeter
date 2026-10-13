# The machine the CI and release builds run on: Ubuntu 24.04 with the cross
# compilers, Node for the plugin's checks and modules, and the pinned Rust
# toolchain with its targets. A job installs no system package when it
# runs, so it never waits for a package mirror.
#
# On a developer's machine, from the repository's root:
#
#   docker build -t lcd1602-builder -f scripts/builder/Dockerfile .
#   docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/work" -w /work lcd1602-builder scripts/ship.sh
#
# The context is the repository's root, of which the build reads only
# rust-toolchain.toml (see Dockerfile.dockerignore).
FROM ubuntu:24.04

LABEL org.opencontainers.image.source=https://github.com/foonerd/lcd1602-vumeter
LABEL org.opencontainers.image.description="The machine the LCD1602 VU meter's CI and release builds run on"

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    ca-certificates curl file git xz-utils zip unzip shellcheck \
    gcc libc6-dev make binutils \
    gcc-arm-linux-gnueabihf gcc-aarch64-linux-gnu libc6-dev-armhf-cross libc6-dev-arm64-cross \
    libasound2-dev \
    && rm -rf /var/lib/apt/lists/*

# The checkout a build works in belongs to whoever mounted it, not to the
# user inside; git answers for it all the same, so a package can name the
# commit it was built from.
RUN git config --system --add safe.directory '*'

# Node, for the plugin's tests and the modules the plugin zip carries.
ARG NODE_VERSION=20.20.2
ARG NODE_SHA256=df770b2a6f130ed8627c9782c988fda9669fa23898329a61a871e32f965e007d
RUN curl -fsSL -o /tmp/node.tar.xz "https://nodejs.org/dist/v$NODE_VERSION/node-v$NODE_VERSION-linux-x64.tar.xz" \
    && echo "$NODE_SHA256  /tmp/node.tar.xz" | sha256sum -c - \
    && tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1 --no-same-owner \
    && rm /tmp/node.tar.xz /usr/local/CHANGELOG.md /usr/local/README.md /usr/local/LICENSE

# The Rust toolchain rust-toolchain.toml names, with its components and
# targets. Cargo's home is writable by any user, so the image also serves a
# developer's own uid.
ARG RUSTUP_VERSION=1.29.1
ARG RUSTUP_SHA256=dda7234360b7f578ca8b0ddcb80145646fa61a67c1720a5abc7051b35c9fcb71
ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
COPY rust-toolchain.toml /tmp/toolchain/rust-toolchain.toml
RUN curl -fsSL -o /tmp/rustup-init "https://static.rust-lang.org/rustup/archive/$RUSTUP_VERSION/x86_64-unknown-linux-gnu/rustup-init" \
    && echo "$RUSTUP_SHA256  /tmp/rustup-init" | sha256sum -c - \
    && chmod +x /tmp/rustup-init \
    && /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain none \
    && cd /tmp/toolchain \
    && rustup toolchain install \
    && rustup default "$(rustup show active-toolchain | cut -d' ' -f1)" \
    && rm -rf /tmp/rustup-init /tmp/toolchain \
    && chmod -R a+rwX "$CARGO_HOME"
