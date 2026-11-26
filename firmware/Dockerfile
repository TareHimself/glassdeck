# The firmware's build environment: Rust for the RP2040 plus an Arm C compiler for littlefs.
FROM rust:1.100-slim-bookworm
RUN apt-get update \
 && apt-get install -y --no-install-recommends gcc-arm-none-eabi libnewlib-arm-none-eabi libudev-dev pkg-config \
 && rm -rf /var/lib/apt/lists/*
RUN rustup target add thumbv6m-none-eabi && rustup component add llvm-tools
RUN cargo install elf2uf2-rs@2.2.0 --locked
