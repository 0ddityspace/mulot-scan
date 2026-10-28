# Environnement de compilation Linux portable : Debian 12 (glibc 2.36).
# Le binaire produit ne demande que la glibc 2.35 : Ubuntu 22.04+, Debian 12+,
# Fedora 36+, Arch…
ARG RUST_IMAGE=rust:1-bookworm
FROM ${RUST_IMAGE}

# bindgen (crate v4l) a besoin de libclang à la compilation.
RUN apt-get update \
    && apt-get install -y --no-install-recommends libclang-dev \
    && rm -rf /var/lib/apt/lists/*
