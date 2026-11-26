# The Tesla image has no shell. Copy the binary into an image that has a shell.
# The entrypoint script needs a shell to write the TLS files from variables.
FROM tesla/fleet-telemetry:v0.10.1 AS upstream

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=upstream /fleet-telemetry /fleet-telemetry
COPY config/server_config.json /etc/fleet-telemetry/config.json
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# The Railway TCP proxy forwards the public port to this port.
EXPOSE 4443

ENTRYPOINT ["/entrypoint.sh"]
