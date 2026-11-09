FROM node:26.12.1-trixie

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    build-essential pkg-config python3 xz-utils imagemagick \
    libsecret-1-dev libgtk-3-0t64 libnss3 libasound2t64 libgbm1 \
  && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup CARGO_HOME=/usr/local/cargo PATH=/usr/local/cargo/bin:$PATH
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable

ARG PNPM_VERSION
RUN npm install -g "pnpm@${PNPM_VERSION}"
