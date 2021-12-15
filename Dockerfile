# Using multistage build:
# 	https://docs.docker.com/develop/develop-images/multistage-build/
# 	https://whitfin.io/speeding-up-rust-docker-builds/

########################## RUNTIME IMAGE ##########################
# Create new stage with a minimal image for the actual
# runtime image/container
FROM debian:bullseye-slim

ARG CREATED
ARG VERSION
ARG GIT_REF=origin/master

ENV CONDUIT_CONFIG="/srv/conduit/conduit.toml"

# Labels according to https://github.com/opencontainers/image-spec/blob/master/annotations.md
# including a custom label specifying the build command
LABEL org.opencontainers.image.created=${CREATED} \
      org.opencontainers.image.authors="Conduit Contributors" \
      org.opencontainers.image.title="Conduit" \
      org.opencontainers.image.version=${VERSION} \
      org.opencontainers.image.vendor="Conduit Contributors" \
      org.opencontainers.image.description="A Matrix homeserver written in Rust" \
      org.opencontainers.image.url="https://conduit.rs/" \
      org.opencontainers.image.revision=${GIT_REF} \
      org.opencontainers.image.source="https://gitlab.com/famedly/conduit.git" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.documentation="" \
      org.opencontainers.image.ref.name="" \
      org.label-schema.docker.build="docker build . -t matrixconduit/matrix-conduit:latest --build-arg CREATED=$(date -u +'%Y-%m-%dT%H:%M:%SZ') --build-arg VERSION=$(grep -m1 -o '[0-9].[0-9].[0-9]' Cargo.toml)" \
      maintainer="Weasy666"

# Standard port on which Conduit launches. You still need to map the port when using the docker command or docker-compose.
EXPOSE 6167

# Copy config files from context and the binary from
# the "builder" stage to the current stage into folder
# /srv/conduit and create data folder for database
RUN mkdir -p /srv/conduit/.local/share/conduit
COPY ./conduit /srv/conduit/
COPY ./docker/healthcheck.sh /srv/conduit/

# Add www-data user and group with UID 82, as used by alpine
# https://git.alpinelinux.org/aports/tree/main/nginx/nginx.pre-install
RUN set -x ; \
    chmod +x /srv/conduit/healthcheck.sh ; \
    addgroup -Sg 82 www-data 2>/dev/null ; \
    adduser -S -D -H -h /srv/conduit -G www-data -g www-data www-data 2>/dev/null ; \
    addgroup www-data www-data 2>/dev/null && exit 0 ; exit 1

# Change ownership of Conduit files to www-data user and group
RUN chown -cR www-data:www-data /srv/conduit

# Install packages needed to run Conduit
RUN apk add --no-cache \
        ca-certificates \
        curl \
        libgcc

# Test if Conduit is still alive, uses the same endpoint as Element
HEALTHCHECK --start-period=5s --interval=60s CMD ./healthcheck.sh

# Set user to www-data
USER www-data
# Set container home directory
WORKDIR /srv/conduit
# Run Conduit
ENTRYPOINT [ "/srv/conduit/conduit" ]