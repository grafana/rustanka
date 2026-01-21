# Runtime stage - using distroless for minimal, secure image
FROM gcr.io/distroless/cc-debian12:latest

# Copy the pre-built binary (copied to docker-bin/ by the workflow)
COPY docker-bin/rtk /usr/local/bin/rtk

# Add labels for metadata
ARG VERSION
ARG BRANCH
ARG REVISION
LABEL org.opencontainers.image.title="rustanka" \
      org.opencontainers.image.description="Rust implementation of Tanka (rtk)" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.source="https://github.com/grafana/rustanka"

ENTRYPOINT ["/usr/local/bin/rtk"]
CMD ["--help"]
