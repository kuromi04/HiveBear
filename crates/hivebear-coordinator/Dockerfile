FROM rust:latest as builder

# Install required tools for compiling C++ bindings (llama-cpp-sys-2/bindgen)
RUN apt-get update && apt-get install -y clang libclang-dev cmake build-essential && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/hivebear
COPY . .

RUN cargo build -p hivebear-coordinator --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/src/hivebear/target/release/hivebear-coordinator /usr/local/bin/hivebear-coordinator

EXPOSE 7879
ENTRYPOINT ["hivebear-coordinator", "--bind", "0.0.0.0", "--port", "7879"]
