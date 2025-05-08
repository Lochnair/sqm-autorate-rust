FROM lochnair/alpine-sdk:latest

USER root
# Install required dependencies
RUN apk add --no-cache \
    bash \
    curl \
    gcc \
    musl-dev \
    libgcc \
    lld \
    openssl-dev \
    pkgconfig \
    make \
    git \
    rustup

RUN GCC_TARGETS="aarch64-linux-musl arm-linux-musleabi armv7m-linux-musleabi armv7l-linux-musleabihf mips-linux-muslsf mips64-linux-musl mips64el-linux-musl mipsel-linux-muslsf x86_64-linux-musl" && \
    for GCC_TARGET in $GCC_TARGETS; do \
        TOOLCHAIN_URL="https://musl.cc/${GCC_TARGET}-cross.tgz"; \
        wget -O- ${TOOLCHAIN_URL} | tar -xz -C /opt; \
    done

USER sdk

# Install Rust and set up rustup
RUN rustup-init -y && \
    source $HOME/.cargo/env && \
    rustup update nightly && \
    rustup default nightly && \
    rustup component add rust-src --toolchain nightly

