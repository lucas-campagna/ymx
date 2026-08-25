# ─────────────────────────────────────────────────────────────────────────────
# Stage 1: build ymx with the pdf-system feature (no Chromium download).
# Uses the stable Rust toolchain pinned in rust-toolchain.toml.
# ─────────────────────────────────────────────────────────────────────────────
FROM rust:slim-bookworm AS builder

WORKDIR /build

# Install cross-compile targets (for multi-arch Docker builds)
RUN apt-get update && apt-get install -y --no-install-recommends \
    gcc aarch64-linux-gnu libc6-dev-arm64-cross \
    && rm -rf /var/lib/apt/lists/*

COPY rust-toolchain.toml ./
RUN rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu

# Build with pdf-system (system Chrome — requires Chrome installed in the runtime stage)
COPY . .
RUN cargo build --release -p ymx-cli --features pdf-system \
    --target x86_64-unknown-linux-gnu \
    --target aarch64-unknown-linux-gnu

# ─────────────────────────────────────────────────────────────────────────────
# Stage 2: runtime — minimal image with system Chrome installed.
# Chrome is installed here so the ymx binary (built with pdf-system) can use it.
# ─────────────────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

# Install Chrome (for pdf-system backend) and basic utilities
RUN apt-get update && apt-get install -y --no-install-recommends \
    gnupg curl \
    && curl -fsSL https://dl.google.com/linux/linux_signing_key.pub \
        | gpg --dearmor -o /usr/share/keyrings/google-chrome.gpg \
    && echo "deb [arch=amd64 signed-by=/usr/share/keyrings/google-chrome.gpg] http://dl.google.com/linux/chrome/deb/ stable main" \
        > /etc/apt/sources.list.d/google-chrome.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        google-chrome-stable \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user for running ymx
RUN useradd --create-home --shell /bin/bash ymx

WORKDIR /home/ymx

# Copy ymx binaries from the build stage
COPY --from=builder /build/target/x86_64-unknown-linux-gnu/release/ymx /usr/local/bin/ymx
COPY --from=builder /build/target/aarch64-unknown-linux-gnu/release/ymx /usr/local/bin/ymx-arm64

# Ensure ymx is on PATH
ENV PATH="/usr/local/bin:${PATH}"

# Verify Chrome is available (for pdf-system backend)
RUN which google-chrome-stable && google-chrome-stable --version

ENTRYPOINT ["ymx"]
CMD ["--help"]
