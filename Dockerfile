# Download gpg
FROM alpine:3.19 AS gpg
RUN apk add --no-cache gnupg


# runc
FROM golang:1.21-alpine3.19 AS runc
ARG RUNC_VERSION=v1.1.10
# Download runc binary release since static build doesn't work with musl libc anymore since 1.1.8, see https://github.com/opencontainers/runc/issues/3950
RUN set -eux; \
	ARCH="`uname -m | sed 's!x86_64!amd64!; s!aarch64!arm64!'`"; \
	wget -O /usr/local/bin/runc https://github.com/opencontainers/runc/releases/download/$RUNC_VERSION/runc.$ARCH; \
	chmod +x /usr/local/bin/runc; \
	runc --version; \
	! ldd /usr/local/bin/runc


# podman build base
FROM golang:1.21-alpine3.19 AS podmanbuildbase
RUN apk add --update --no-cache git make gcc pkgconf musl-dev \
	btrfs-progs btrfs-progs-dev libassuan-dev lvm2-dev device-mapper \
	glib-static libc-dev gpgme-dev protobuf-dev protobuf-c-dev \
	libseccomp-dev libseccomp-static libselinux-dev ostree-dev openssl iptables \
	bash go-md2man
# Hotfix for musl build failure https://github.com/mattn/go-sqlite3/issues/1164
#RUN go get github.com/mattn/go-sqlite3


# netavark
FROM podmanbuildbase AS netavark
#RUN apk add --update --no-cache tzdata curl rust cargo
RUN apk add --update --no-cache rust cargo
ARG NETAVARK_VERSION=v1.9.0
RUN git clone -c 'advice.detachedHead=false' --depth=1 --branch=${NETAVARK_VERSION} https://github.com/containers/netavark /netavark
#RUN git clone -c 'advice.detachedHead=false' --depth=1 --branch=${NETAVARK_VERSION:-$(curl -s https://api.github.com/repos/containers/netavark/releases/latest | grep tag_name | cut -d '"' -f 4)} https://github.com/containers/netavark /netavark
WORKDIR /netavark
RUN set -ex; \
	make build_netavark
#	make


# Build rootless podman base image with runc
FROM rootlesspodmanbase AS rootlesspodmanrunc
COPY --from=runc   /usr/local/bin/runc   /usr/local/bin/runc

# Download crun
# (switched keyserver from sks to ubuntu since sks is offline now and gpg refuses to import keys from keys.openpgp.org because it does not provide a user ID with the key.)
FROM gpg AS crun
ARG CRUN_VERSION=1.12
RUN set -ex; \
	wget -O /usr/local/bin/crun https://github.com/containers/crun/releases/download/$CRUN_VERSION/crun-${CRUN_VERSION}-linux-amd64-disable-systemd; \
	wget -O /tmp/crun.asc https://github.com/containers/crun/releases/download/$CRUN_VERSION/crun-${CRUN_VERSION}-linux-amd64-disable-systemd.asc; \
	gpg --keyserver hkps://keyserver.ubuntu.com --recv-keys 027F3BD58594CA181BB5EC50E4730F97F60286ED; \
	gpg --batch --verify /tmp/crun.asc /usr/local/bin/crun; \
	chmod +x /usr/local/bin/crun; \
	crun --help >/dev/null

# Build minimal rootless podman
FROM rootlesspodmanbase AS rootlesspodmanminimal
COPY --from=crun /usr/local/bin/crun /usr/local/bin/crun
COPY conf/crun-containers.conf /etc/containers/containers.conf

# Build podman image with rootless binaries and CNI plugins
FROM rootlesspodmanrunc AS podmanall
RUN apk add --no-cache iptables ip6tables
COPY --from=netavark /netavark/bin/netavark /usr/local/bin/netavark
COPY conf/cni /etc/cni
