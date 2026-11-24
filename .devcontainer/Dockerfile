FROM node:24-bookworm-slim AS node

FROM golang:1.27-bookworm

ENV CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup
ENV PATH=/usr/local/cargo/bin:$PATH

RUN curl -fsSL https://sh.rustup.rs -o /tmp/rustup-init.sh \
    && sh /tmp/rustup-init.sh -y --no-modify-path --profile minimal \
        --default-toolchain 1.91.1 --component rustfmt \
    && rm /tmp/rustup-init.sh

COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && npm install -g pnpm@10.34.7 \
    && npm cache clean --force
