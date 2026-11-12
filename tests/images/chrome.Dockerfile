# A browser to run the real client in, and Node to drive it.
FROM node:24-bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends chromium && rm -rf /var/lib/apt/lists/*
