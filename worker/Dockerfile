# -----------------------------------------
# 1) Builder Stage
# -----------------------------------------
FROM ubuntu:24.04 AS builder

# Install build dependencies
RUN apt-get update && \
    apt-get install -y curl build-essential libssl-dev ca-certificates pkg-config && \
    rm -rf /var/lib/apt/lists/*

# Install Rust (latest stable) via rustup
RUN curl https://sh.rustup.rs -sSf | sh -s -- -y
ENV PATH=/root/.cargo/bin:$PATH

WORKDIR /app

# Copy full source and build in one shot
COPY . .
RUN cargo build --release

# -----------------------------------------
# 2) Runtime Stage
# -----------------------------------------
FROM ubuntu:24.04

RUN apt-get update && apt-get install -y ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the compiled binary from builder
COPY --from=builder /app/target/release/hydra-worker /app/hydra-worker

# Set default command
CMD ["/app/hydra-worker"]
