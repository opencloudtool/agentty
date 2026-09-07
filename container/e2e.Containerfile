# Canonical Linux environment for the Agentty end-to-end feature suite.
#
# CI uses this image as the E2E job container and runs the `test-agentty-e2e`
# hook with `TESTTY_GIF_MODE=check`, which verifies committed GIF hash sidecars
# without rewriting them. Developers record or refresh feature artifacts in
# the same image with a writable mount and `TESTTY_GIF_MODE=generate` (see
# `skills/feature-test/SKILL.md`), which keeps committed hashes portable between
# local recording and CI verification.
# Every tool is pinned; upgrade pins deliberately and re-verify the committed
# GIF hash sidecars.
FROM debian@sha256:7b140f374b289a7c2befc338f42ebe6441b7ea838a042bbd5acbfca6ec875818 AS tool-downloads

ARG TARGETARCH

ARG RUSTUP_VERSION=1.29.0
ARG NEXTEST_VERSION=0.9.140
ARG PREK_VERSION=0.4.14
ARG TTYD_VERSION=1.7.7
ARG VHS_VERSION=0.11.0

ARG NEXTEST_SHA256_AMD64=4ee9aaa0d0171a985a5d0eb735b87355894c1c455972e9674fb9fdbd1387c9a3
ARG NEXTEST_SHA256_ARM64=8b3f4d4560b6b0f83774fecc6be07e47716dbad0eb0bb6c3890f478f4affe4b6
ARG PREK_SHA256_AMD64=d1eacb826b9f71ce9098636d2c3e96df0ed1cf08c9c17c57bf358b401f5a995d
ARG PREK_SHA256_ARM64=a0302d10599f40eb1b46bfe28cfe04c2638f0af858787d21c5904acc8047aaff
ARG RUSTUP_INIT_SHA256_AMD64=4acc9acc76d5079515b46346a485974457b5a79893cfb01112423c89aeb5aa10
ARG RUSTUP_INIT_SHA256_ARM64=9732d6c5e2a098d3521fca8145d826ae0aaa067ef2385ead08e6feac88fa5792
ARG TTYD_SHA256_AMD64=8a217c968aba172e0dbf3f34447218dc015bc4d5e59bf51db2f2cd12b7be4f55
ARG TTYD_SHA256_ARM64=b38acadd89d1d396a0f5649aa52c539edbad07f4bc7348b27b4f4b7219dd4165
ARG VHS_SHA256_AMD64=99cb634587eaae0473c1ea377db80c3a048c27f99fe0a7febb1a1e8cb7ee5009
ARG VHS_SHA256_ARM64=af782cddbf844a377df6ea41c0e72339393fa021be3f6cb70a2f47d48675d92b

# Download the architecture-matched, checksum-verified tool bundle. The `prek`
# pin matches `.github/actions/setup-rust-prek/action.yml`; Debian bookworm does
# not package `ttyd`, so its upstream static binary is pinned here too.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/* \
    && case "${TARGETARCH}" in \
    amd64) \
        rust_target=x86_64-unknown-linux-gnu; \
        ttyd_arch=x86_64; \
        vhs_arch=x86_64; \
        nextest_sha256="${NEXTEST_SHA256_AMD64}"; \
        prek_sha256="${PREK_SHA256_AMD64}"; \
        rustup_init_sha256="${RUSTUP_INIT_SHA256_AMD64}"; \
        ttyd_sha256="${TTYD_SHA256_AMD64}"; \
        vhs_sha256="${VHS_SHA256_AMD64}" \
        ;; \
    arm64) \
        rust_target=aarch64-unknown-linux-gnu; \
        ttyd_arch=aarch64; \
        vhs_arch=arm64; \
        nextest_sha256="${NEXTEST_SHA256_ARM64}"; \
        prek_sha256="${PREK_SHA256_ARM64}"; \
        rustup_init_sha256="${RUSTUP_INIT_SHA256_ARM64}"; \
        ttyd_sha256="${TTYD_SHA256_ARM64}"; \
        vhs_sha256="${VHS_SHA256_ARM64}" \
        ;; \
    *) \
        echo "unsupported target architecture: ${TARGETARCH}" >&2; \
        exit 1 \
        ;; \
    esac \
    && curl --proto '=https' --proto-redir '=https' --fail --location --silent --show-error \
        --output /rustup-init \
        "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${rust_target}/rustup-init" \
    && curl --proto '=https' --proto-redir '=https' --fail --location --silent --show-error \
        --output /nextest.tar.gz \
        "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-${NEXTEST_VERSION}/cargo-nextest-${NEXTEST_VERSION}-${rust_target}.tar.gz" \
    && curl --proto '=https' --proto-redir '=https' --fail --location --silent --show-error \
        --output /prek.tar.gz \
        "https://github.com/j178/prek/releases/download/v${PREK_VERSION}/prek-${rust_target}.tar.gz" \
    && curl --proto '=https' --proto-redir '=https' --fail --location --silent --show-error \
        --output /ttyd \
        "https://github.com/tsl0922/ttyd/releases/download/${TTYD_VERSION}/ttyd.${ttyd_arch}" \
    && curl --proto '=https' --proto-redir '=https' --fail --location --silent --show-error \
        --output /vhs.tar.gz \
        "https://github.com/charmbracelet/vhs/releases/download/v${VHS_VERSION}/vhs_${VHS_VERSION}_Linux_${vhs_arch}.tar.gz" \
    && echo "${rustup_init_sha256} */rustup-init" | sha256sum -c - \
    && echo "${nextest_sha256} */nextest.tar.gz" | sha256sum -c - \
    && echo "${prek_sha256} */prek.tar.gz" | sha256sum -c - \
    && echo "${ttyd_sha256} */ttyd" | sha256sum -c - \
    && echo "${vhs_sha256} */vhs.tar.gz" | sha256sum -c -

FROM debian@sha256:7b140f374b289a7c2befc338f42ebe6441b7ea838a042bbd5acbfca6ec875818

LABEL org.opencontainers.image.source="https://github.com/agentty-xyz/agentty"

ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_TARGET_DIR=/opt/target \
    PATH=/usr/local/cargo/bin:$PATH \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    TZ=UTC \
    # Unprivileged containers lack the kernel privileges Chromium's own
    # sandbox needs, so VHS must disable it to launch Chromium.
    VHS_NO_SANDBOX=true

# Build toolchain plus the VHS recording stack: `ffmpeg`, Chromium, and the
# JetBrains Mono font VHS renders with by default, all from the pinned Debian
# release (`ttyd` is pinned separately below). `check` mode needs none of the
# recording stack, but one shared image for checking and recording is what
# makes the hashes portable. Podman-based publication checks bind-mount the
# checkout at `/workspace`; keep git working when its owner differs from the
# image user, so `prek` can enumerate files. The reusable CI workflow runs the
# job container as root and registers GitHub's exact workspace path as safe
# before invoking `prek`.
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    ca-certificates \
    chromium \
    ffmpeg \
    fonts-dejavu \
    fonts-jetbrains-mono \
    fonts-noto-color-emoji \
    git \
    pkg-config \
    && rm -rf /var/lib/apt/lists/* \
    && git config --system --add safe.directory /workspace

# Pin the nightly toolchain to a date so image rebuilds keep the same
# compiler. `RUSTUP_TOOLCHAIN` overrides the workspace toolchain selection
# inside the container, so rustup never downloads a newer nightly at run time.
# Bump the date deliberately and re-verify the committed GIF hash sidecars.
ARG RUST_TOOLCHAIN=nightly-2026-07-15
ENV RUSTUP_TOOLCHAIN=${RUST_TOOLCHAIN}

# Install the checksum-verified tools from the read-only download stage, then
# create the unprivileged runtime user with the GitHub Actions runner's UID
# 1001. Mounting the downloads keeps installers and archives out of the final
# image layers, while the installed toolchain remains root-owned and read-only.
RUN --mount=from=tool-downloads,target=/downloads \
    install -m 0755 /downloads/rustup-init /tmp/rustup-init \
    && /tmp/rustup-init -y --profile minimal --default-toolchain "${RUST_TOOLCHAIN}" \
    && rm /tmp/rustup-init \
    && tar -xzf /downloads/nextest.tar.gz -C "${CARGO_HOME}/bin" \
    && mkdir /tmp/prek \
    && tar -xzf /downloads/prek.tar.gz -C /tmp/prek \
    && install -m 0755 "$(find /tmp/prek -type f -name prek)" /usr/local/bin/prek \
    && rm -rf /tmp/prek \
    && install -m 0755 /downloads/ttyd /usr/local/bin/ttyd \
    && mkdir /tmp/vhs \
    && tar -xzf /downloads/vhs.tar.gz -C /tmp/vhs \
    && install -m 0755 "$(find /tmp/vhs -type f -name vhs)" /usr/local/bin/vhs \
    && rm -rf /tmp/vhs \
    && useradd --uid 1001 --user-group --create-home agentty \
    && install -d -o agentty -g agentty \
    "${CARGO_HOME}/registry" "${CARGO_HOME}/git" "${CARGO_TARGET_DIR}"

USER agentty
ENV HOME=/home/agentty

WORKDIR /workspace
