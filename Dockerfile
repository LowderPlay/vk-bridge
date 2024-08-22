FROM rust:1.80 as builder
WORKDIR /usr/src/vkbridge
COPY . .
RUN cargo install --path .

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y openssl && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/cargo/bin/vkbridge /usr/local/bin/vkbridge
CMD ["vkbridge"]