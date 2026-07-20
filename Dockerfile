# 多阶段构建：builder 编译，runtime 只带二进制 + ca-certificates。
FROM rust:1.87-slim AS builder
WORKDIR /app
# rustls 后端 aws-lc-rs 需要 cmake / clang / perl 等构建工具
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential cmake clang perl pkg-config \
    && rm -rf /var/lib/apt/lists/*
# 先只用 manifest + 空 main 编译依赖，命中缓存，后续改 src 不重编依赖
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src
# 再拷真实源码编译
COPY src ./src
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/feishu-github-bot /usr/local/bin/feishu-github-bot
# 配置文件挂载到 /app/config.toml，密钥用环境变量注入
ENTRYPOINT ["feishu-github-bot", "/app/config.toml"]
