# The four images, from one file and one dependency build.
#
# Two things this is arranged around, and both were costing whole minutes
# per run before it existed:
#
# - **Dependencies are cooked once.** `cargo-chef` reduces the workspace
#   to a recipe of its dependency graph, builds *that* on its own layer,
#   and only then copies the sources in. An edit to this project's code
#   reuses the layer holding the gRPC stack, the AWS SDK and rustls, which
#   is where the time actually goes.
# - **The binaries share that layer.** They were separate files with the
#   same forty lines and a `COPY . .` build each, so every image paid for
#   the whole workspace's dependencies again.
#
# **The base images are pinned by digest**, and the tag is kept beside the
# digest so a reader can see what it is. A digest says which bytes; a tag
# says which bytes *today*, and a build that is reproducible only until
# somebody re-pushes a tag is not one. Dependabot's `docker` ecosystem
# moves them weekly (`.github/dependabot.yml`) — without that, a pinned
# base image is one that stops receiving the security updates the nightly
# Trivy scan exists to notice.
#
# Build one:  docker build -f docker/Dockerfile --target agent -t agent:dev .
FROM rust:1.98-slim@sha256:cc0448b41c3b7b7fea44f5dc50eacba729a56db365b65b7bd5e8a82d5b3db078 AS chef
WORKDIR /src
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev \
      protobuf-compiler libprotobuf-dev \
 && rm -rf /var/lib/apt/lists/* \
 && cargo install cargo-chef --locked --version 0.1.71

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build
# The recipe alone, so this layer is invalidated by a dependency change
# and by nothing else.
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release \
      -p dynamic-config-agent \
      -p dynamic-config-webhook \
      -p dynamic-config-operator \
      -p dynamic-config-node-agent

# One stage per binary, all from the same build layer. `--target` picks
# which one an image is.
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f AS agent
COPY --from=build /src/target/release/dynamic-config-agent /usr/local/bin/
USER nonroot
ENTRYPOINT ["dynamic-config-agent"]

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f AS webhook
COPY --from=build /src/target/release/dynamic-config-webhook /usr/local/bin/
USER nonroot
ENTRYPOINT ["dynamic-config-webhook"]

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f AS operator
COPY --from=build /src/target/release/dynamic-config-operator /usr/local/bin/
USER nonroot
ENTRYPOINT ["dynamic-config-operator"]

# The one image here that does NOT set `USER nonroot`, and the reason is
# worth reading rather than discovering: the kubelet creates a pod's volume
# directories owned by root, and a CSI node plugin that cannot write into
# them cannot publish anything. Every other binary in this repository runs
# nonroot and this one cannot, which is one of the reasons the node agent
# is off by default and the sidecar remains the shape to reach for.
FROM gcr.io/distroless/cc-debian12@sha256:e5d81ddde149641e2a9ba55be4545bc125c67de07508b03ba4c22e6eb0ded5aa AS node-agent
COPY --from=build /src/target/release/dynamic-config-node-agent /usr/local/bin/
ENTRYPOINT ["dynamic-config-node-agent"]
