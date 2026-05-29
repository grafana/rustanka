# Runtime stage - distroless/cc-debian12.
#
# The rtk/jrsonnet binaries are built for *-unknown-linux-gnu and are
# dynamically linked against glibc and libgcc_s.so.1 (verified via readelf:
# NEEDED libc.so.6, libm.so.6, libgcc_s.so.1). distroless/cc is the smallest
# distroless variant that ships both glibc and libgcc_s, so the binaries run
# unmodified. scratch / distroless:static (no glibc) and distroless:base
# (glibc but no libgcc_s) would fail at startup.
#
# Compared to the previous ubuntu:24.04 base this drops the shell, package
# manager, and the rest of the OS userland, and pins the base by digest.
# The :nonroot tag runs as UID/GID 65532 and ships ca-certificates.
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:bd2899c12b335c827750ccf2359879eab09c09b206023dcebea408947d54127c

# Copy the pre-built binaries (copied to docker-bin/ by the workflow)
COPY docker-bin/rtk /usr/local/bin/rtk
COPY docker-bin/jrsonnet /usr/local/bin/jrsonnet

# distroless ships the nonroot user (UID/GID 65532); no useradd needed.
USER nonroot:nonroot

# Add labels for metadata
ARG VERSION
ARG BRANCH
ARG REVISION
LABEL org.opencontainers.image.title="rustanka" \
      org.opencontainers.image.description="Rust implementation of Tanka (rtk) with jrsonnet" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.source="https://github.com/grafana/rustanka"

ENTRYPOINT ["/usr/local/bin/rtk"]
CMD ["--help"]
