# The Tesla image has no shell. Copy the binary into an image that has a shell.
# The entrypoint script needs a shell to write the TLS files from variables.
FROM tesla/fleet-telemetry:v0.10.1 AS upstream

FROM debian:trixie-slim
# The upgrade installs the Debian security fixes that are newer than the base
# image (docs/security/image-advisories.md#debian-13-base). The result depends
# on the build date.
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get upgrade -y \
    && apt-get install -y --no-install-recommends ca-certificates libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=upstream /fleet-telemetry /fleet-telemetry
COPY config/server_config.json /etc/fleet-telemetry/config.json
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# The Railway TCP proxy forwards the public port to this port.
EXPOSE 4443

ENTRYPOINT ["/entrypoint.sh"]
