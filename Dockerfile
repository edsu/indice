# Multi-stage build → a small glibc image running `indice serve`.
#
# Builder: the pinned Rust toolchain on Debian bookworm (glibc 2.36), which
# matches the distroless runtime base below (same glibc), so the binary it
# produces runs on that base. ReplayWeb.page assets are embedded at build time.
FROM rust:1.97.1-bookworm AS builder
WORKDIR /src
COPY . .
RUN cargo build --release --locked --bin indice

# Runtime: a distroless glibc base — no shell, no package manager, minimal
# surface. It has no curl, so the container's HEALTHCHECK calls the binary's own
# `indice health`. Runs as the base image's non-root user.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /src/target/release/indice /usr/local/bin/indice

# The indice home (archive/ + index/) lives here; mount a volume for persistence.
WORKDIR /data
EXPOSE 8080

# Inside the container `indice health` probes the local server; a distroless
# image has no curl, so the binary checks itself.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/indice", "health", "--url", "http://127.0.0.1:8080/health"]

# No default arguments. Serving on a public interface needs an authenticating
# proxy in front, and indice refuses to start otherwise rather than trusting
# whoever reaches the port, so there is no useful command we can guess here.
# compose.yaml supplies the real one; `docker run` without arguments prints the
# help, which is the honest answer to "what do I pass?".
ENTRYPOINT ["/usr/local/bin/indice"]
