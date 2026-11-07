# slidepdf CLI in a scratch image: one static binary, 4.3 MB.
#
#   docker build -t slidepdf .
#   docker run --rm -m 64m --user "$(id -u):$(id -g)" -v "$PWD:/work" slidepdf \
#     fragments /work/deck.pptx -o /work/out --memory-limit 32 --timeout 60
#
# The container has no shell and no libc; the entrypoint is the binary. It
# runs as an unprivileged user, so pass --user to write into a mounted
# directory you own.
FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY viewer ./viewer
COPY bindings ./bindings
RUN cargo build --release --locked --bin slidepdf \
 && strip target/release/slidepdf

FROM scratch
COPY --from=build /src/target/release/slidepdf /slidepdf
USER 65534:65534
WORKDIR /work
ENTRYPOINT ["/slidepdf"]
CMD ["--help"]
