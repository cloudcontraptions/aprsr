# aprsr, in a container.
#
# Standard Dockerfile syntax throughout — no BuildKit-only features — so `podman build`
# works on this file unmodified. That is a deliberate constraint: a good deal of amateur
# radio infrastructure runs on hosts where rootless Podman is the only container runtime
# installed, and a Dockerfile that only builds under BuildKit would exclude them.
#
#   docker build -t aprsr .
#   docker run --rm -p 14580:14580 -p 14501:14501 -v aprsr-data:/var/lib/aprsr aprsr
#
# See docs/deploy.md for the full picture, including compose and the non-container options.

# --- build ---------------------------------------------------------------------------------
#
# Pinned to the toolchain in rust-toolchain.toml. A container that silently followed the
# latest Rust would build differently from CI, which is the one thing a container is
# supposed to stop happening.
FROM rust:1.94-trixie AS build

WORKDIR /src

# Dependencies first, in their own layer. The manifests change far less often than the
# source, so an ordinary edit reuses the compiled dependency layer and takes seconds
# instead of minutes. The dummy sources exist only to give cargo something to compile;
# they are replaced immediately below.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/aprsr/Cargo.toml crates/aprsr/
COPY crates/aprsr-core/Cargo.toml crates/aprsr-core/
COPY crates/aprsr-config/Cargo.toml crates/aprsr-config/
COPY crates/aprsr-store/Cargo.toml crates/aprsr-store/
COPY crates/aprsr-server/Cargo.toml crates/aprsr-server/
COPY crates/aprsr-web/Cargo.toml crates/aprsr-web/
RUN set -eu; \
    for crate in aprsr aprsr-core aprsr-config aprsr-store aprsr-server aprsr-web; do \
        mkdir -p "crates/$crate/src"; \
        echo '// placeholder' > "crates/$crate/src/lib.rs"; \
    done; \
    echo 'fn main() {}' > crates/aprsr/src/main.rs; \
    cargo build --release --locked -p aprsr; \
    rm -rf crates/*/src

# Now the real sources. `touch` is not enough — cargo keys on mtime, and a COPY that
# preserves an older timestamp than the placeholder build would leave the placeholder
# artefacts in place and produce a binary that does nothing.
COPY crates crates
COPY aprsr.example.toml ./
RUN find crates -name '*.rs' -exec touch {} + && \
    cargo build --release --locked -p aprsr && \
    strip target/release/aprsr

# --- runtime -------------------------------------------------------------------------------
#
# Debian slim rather than distroless. `libsqlite3-sys` links a C library, and distroless
# would add a variable to every future dependency change for the sake of an image size that
# is not the binding constraint on an APRS-IS server. ca-certificates is needed for the TLS
# uplinks on the roadmap and costs almost nothing now.
FROM debian:trixie-slim AS runtime

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# A fixed uid, so a bind-mounted data directory has predictable ownership on the host.
# 10001 is above the range Debian allocates to system accounts and well clear of the first
# ordinary user, which is where a collision would actually bite.
RUN groupadd --system --gid 10001 aprsr && \
    useradd --system --uid 10001 --gid aprsr --home-dir /var/lib/aprsr --shell /usr/sbin/nologin aprsr && \
    mkdir -p /var/lib/aprsr /etc/aprsr && \
    chown aprsr:aprsr /var/lib/aprsr

COPY --from=build /src/target/release/aprsr /usr/local/bin/aprsr
COPY --from=build /src/aprsr.example.toml /etc/aprsr/aprsr.example.toml

# The dashboard assets are compiled into the binary, so there is nothing else to copy.

USER aprsr
WORKDIR /var/lib/aprsr

# 14580 clients, 10152 full feed, 14501 the status dashboard. Documentation rather than
# configuration — the container publishes nothing on its own.
EXPOSE 14580 10152 14501

# `/healthz` touches no shared state beyond the configuration, so a probe cannot be the
# thing that makes a loaded server look unhealthy. Uses the binary's own subcommand rather
# than curl, which is not installed and would be a dependency added for one line.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["/usr/local/bin/aprsr", "healthcheck"]

# The configuration is expected as a mount. There is deliberately no default one baked in:
# a server that started with somebody else's callsign would corrupt loop detection for the
# whole network, which is why `server.id = "NOCALL"` refuses to start in the first place.
ENTRYPOINT ["/usr/local/bin/aprsr"]
CMD ["run", "--config", "/etc/aprsr/aprsr.toml"]
