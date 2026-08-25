# =============================================================================
# ZGALAXY-RS as a full zerotier-one replacement for the ZTNET stack.
#
# Builds the drop-in image used by deploy/ztnet/docker-compose.yml:
#   - zgalaxy-rs (musl static) installed as /usr/sbin/zerotier-one
#   - zerotier-cli / zerotier-idtool symlinks (argv0-dispatch, same contract)
#   - identical volume (/var/lib/zerotier-one), ports (9993), healthcheck
#
# Build context MUST be the repository root:
#   docker build -f deploy/ztnet/Dockerfile -t zgalaxy-zerotier:rs .
# The musl binary must exist first (host glibc is newer than the image's):
#   cargo build --release --target x86_64-unknown-linux-musl
# =============================================================================
FROM debian:bookworm-slim

LABEL org.opencontainers.image.title="ZGALAXY Sovereign ZeroTier Controller & Client Daemon"
LABEL org.opencontainers.image.description="100% Memory-Safe Rust ZeroTier Controller & Mesh Client for ZTNET"
LABEL org.opencontainers.image.vendor="DreamZone ZGALAXY"
LABEL org.opencontainers.image.version="1.3.0"

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    iproute2 \
    iptables \
    curl \
    && rm -rf /var/lib/apt/lists/*

# musl static-pie binary: runs on any base image regardless of host glibc.
COPY target/x86_64-unknown-linux-musl/release/zgalaxy-rs /usr/sbin/zerotier-one

RUN ln -sf /usr/sbin/zerotier-one /usr/sbin/zerotier-cli && \
    ln -sf /usr/sbin/zerotier-one /usr/sbin/zerotier-idtool && \
    ln -sf /usr/sbin/zerotier-one /usr/local/bin/zgalaxy-rs && \
    ln -sf /usr/sbin/zerotier-one /usr/local/bin/zgalaxy-cli && \
    mkdir -p /var/lib/zerotier-one

VOLUME ["/var/lib/zerotier-one"]

EXPOSE 9993/udp
EXPOSE 9993/tcp

HEALTHCHECK --interval=15s --timeout=5s --start-period=5s --retries=3 \
  CMD /usr/sbin/zerotier-cli status || exit 1

ENTRYPOINT ["/usr/sbin/zerotier-one"]
