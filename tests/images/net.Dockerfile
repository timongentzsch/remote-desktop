# The far end of a shaped link: the test clients, and `tc` to shape either end's network.
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends iproute2 python3 && rm -rf /var/lib/apt/lists/*
