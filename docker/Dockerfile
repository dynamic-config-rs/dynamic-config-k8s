# The three images, from one file and one dependency build.
#
# Two things this is arranged around, and both were costing whole minutes
# per run before it existed:
#
# - **Dependencies are cooked once.** `cargo-chef` reduces the workspace
#   to a recipe of its dependency graph, builds *that* on its own layer,
#   and only then copies the sources in. An edit to this project's code
#   reuses the layer holding the gRPC stack, the AWS SDK and rustls, which
#   is where the time actually goes.
# - **The three binaries share that layer.** They were three files with
#   the same forty lines and three separate `COPY . .` builds, so each
#   image paid for the whole workspace's dependencies again.
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
FROM rust:1.97-slim@sha256:8e8cf8f7fd54a2d23d5a743b3a03f56e26b6c774276c33fa0595111704ebb15c AS chef
WORKDIR /src
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev protobuf-compiler \
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
      -p dynamic-config-operator

# One stage per binary, all three from the same build layer. `--target`
# picks which one an image is.
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:adcd20c7b4c988b73cbfbddb26d2eee574571e6d7c9ffea29b3821e0690efb77 AS agent
COPY --from=build /src/target/release/dynamic-config-agent /usr/local/bin/
USER nonroot
ENTRYPOINT ["dynamic-config-agent"]

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:adcd20c7b4c988b73cbfbddb26d2eee574571e6d7c9ffea29b3821e0690efb77 AS webhook
COPY --from=build /src/target/release/dynamic-config-webhook /usr/local/bin/
USER nonroot
ENTRYPOINT ["dynamic-config-webhook"]

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:adcd20c7b4c988b73cbfbddb26d2eee574571e6d7c9ffea29b3821e0690efb77 AS operator
COPY --from=build /src/target/release/dynamic-config-operator /usr/local/bin/
USER nonroot
ENTRYPOINT ["dynamic-config-operator"]
