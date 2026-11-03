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

# glibc's allocator keeps memory that reading a large PDF has freed rather than
# handing it back, so each one raises the process's footprint and the next
# stacks on top. Under a container memory limit that is what kills an ingest
# that would otherwise fit: a crawl of large PDFs was killed at a 2.5 GB limit
# with glibc's defaults, and with these peaked at 1.3 to 2.1 GB over three runs
# (rustyweb-63jk). Two arenas, and low trim and mmap thresholds, return freed
# memory promptly.
ENV GLIBC_TUNABLES=glibc.malloc.arena_max=2:glibc.malloc.trim_threshold=131072:glibc.malloc.mmap_threshold=131072

# The indice home (archive/ + index/) lives here; mount a volume for persistence.
WORKDIR /data
# 8080 is the app, 8081 replay, which runs on an origin of its own.
EXPOSE 8080 8081

# Inside the container `indice health` probes the local server; a distroless
# image has no curl, so the binary checks itself.
#
# The port is hard-coded, and with no CMD nothing in the image guarantees it any
# more. Override `--bind` to another port and the container runs correctly while
# reporting itself unhealthy for ever, which in compose blocks anything waiting
# on `service_healthy`. Override HEALTHCHECK alongside it, or keep 8080.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/indice", "health", "--url", "http://127.0.0.1:8080/health"]

# No default arguments. Serving on a public interface needs an authenticating
# proxy in front, and indice refuses to start otherwise rather than trusting
# whoever reaches the port, so there is no useful command we can guess here.
# compose.yaml supplies the real one; `docker run` without arguments prints the
# help, which is the honest answer to "what do I pass?".
ENTRYPOINT ["/usr/local/bin/indice"]
