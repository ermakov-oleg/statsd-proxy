FROM rust:1.49 as builder

RUN USER=root cargo new --bin statsd-proxy
WORKDIR ./statsd-proxy
COPY ./Cargo.toml ./Cargo.toml
RUN cargo build --release
RUN rm src/*.rs

ADD . ./

RUN rm ./target/release/deps/statsd_proxy*

RUN cargo build --release


FROM debian:buster-slim
COPY --from=builder /statsd-proxy/target/release/statsd-proxy /usr/local/bin/statsd-proxy

ENV RUST_LOG=info

EXPOSE 8125/udp

ENTRYPOINT ["statsd-proxy"]