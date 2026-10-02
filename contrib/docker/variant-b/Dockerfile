# barkd/bark image for the variant-b fork, laid out like secondark/bark
# (bark and barkd in /usr/local/bin, /run.sh as entrypoint) so it drops into
# the same regtest compose files.
#
#   docker build -f contrib/docker/variant-b/Dockerfile \
#     --build-arg GIT_HASH=$(git rev-parse HEAD) -t abandon-ship/bark:variant-b .

FROM rust:1.92-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends protobuf-compiler \
	&& rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
ARG GIT_HASH=unknown
ENV GIT_HASH=${GIT_HASH}
# LTO off: the regtest image does not need it and it triples the link time.
ENV CARGO_PROFILE_RELEASE_LTO=false
RUN --mount=type=cache,target=/usr/local/cargo/registry \
	--mount=type=cache,target=/src/target \
	cargo build --release --locked -p bark-cli --features sqlite-bundled \
	&& cp target/release/bark target/release/barkd /usr/local/bin/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates telnet \
	&& rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/bark /usr/local/bin/bark
COPY --from=build /usr/local/bin/barkd /usr/local/bin/barkd
COPY contrib/docker/variant-b/run.sh /run.sh
RUN chmod a+x /usr/local/bin/bark /usr/local/bin/barkd /run.sh
CMD ["/run.sh"]
