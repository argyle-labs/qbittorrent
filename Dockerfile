# TODO: base image + build for qbittorrent. Mirror jellyfin/Dockerfile conventions.
FROM debian:12-slim
LABEL org.opencontainers.image.source="https://github.com/argyle-labs/qbittorrent"
EXPOSE 8080
