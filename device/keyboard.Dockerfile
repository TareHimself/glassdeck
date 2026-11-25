# Builds Glassdeck's keyboard programs against glibc 2.36, the stock keyboard's (Debian bookworm, as
# Keyglass does). Used by device/keyboard-build.sh.
FROM debian:bookworm

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl pkg-config gcc g++ libc6-dev \
      gcc-aarch64-linux-gnu g++-aarch64-linux-gnu libc6-dev-arm64-cross \
 && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
ARG RUST_VERSION=1.100.0
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain $RUST_VERSION \
      --target aarch64-unknown-linux-gnu --no-modify-path

ENV CARGO_TARGET_DIR=/target \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
    PKG_CONFIG_ALLOW_CROSS=1 \
    PKG_CONFIG_SYSROOT_DIR=/sysroot \
    PKG_CONFIG_LIBDIR=/sysroot/usr/local/lib/pkgconfig \
    CXXSTDLIB_aarch64_unknown_linux_gnu=static=stdc++ \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-L native=/usr/lib/gcc-cross/aarch64-linux-gnu/12 -C link-arg=-Wl,--allow-shlib-undefined"
