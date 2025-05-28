FROM rust:1.82 as builder
WORKDIR /usr/src/vkbridge
COPY . .
RUN cargo install --path .

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates openssl && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/cargo/bin/vkbridge /usr/local/bin/vkbridge
CMD ["vkbridge"]
