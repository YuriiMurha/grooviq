FROM node:24-bookworm-slim AS node
FROM mcr.microsoft.com/playwright:v1.64.1-noble
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -sf /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm && ln -sf /usr/local/lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx
WORKDIR /work
COPY app/ui/package.json app/ui/package-lock.json ./app/ui/
RUN cd app/ui && npm ci
COPY . .
ENV CI=1
CMD ["node", "scripts/stability/run.mjs"]
