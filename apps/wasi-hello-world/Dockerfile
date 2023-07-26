FROM rust as builder
COPY . /build
WORKDIR /build
RUN rustup target add wasm32-wasi
RUN cargo build --release --target wasm32-wasi

FROM scratch
COPY --link --from=builder /build/target/wasm32-wasi/release/wasi-hello-world.wasm /