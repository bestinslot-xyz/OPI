FROM golang:1.21.3-alpine AS build
ARG GO_OS="linux"
ARG GO_ARCH="amd64"
WORKDIR /usr/local/build/
COPY ./go.mod .
COPY ./go.sum .
COPY ./lib/brc20_swap/go.mod ./lib/brc20_swap/go.mod

# Build binary output
RUN GOOS=${GO_OS} GOARCH=${GO_ARCH} go mod download

COPY . .

RUN GOOS=${GO_OS} GOARCH=${GO_ARCH} go build -o brc20query -ldflags '-s -w' main.go

FROM alpine:3.18.4
RUN apk add tzdata && cp /usr/share/zoneinfo/Asia/Hong_Kong /etc/localtime && echo "Asia/Hong_Kong" > /etc/timezone

RUN adduser -u 1000 -D sato -h /data
USER sato
WORKDIR /data/

COPY --chown=sato --from=build /usr/local/build/brc20query /data/brc20query
COPY --chown=sato --from=build /usr/local/build/docs /data/docs

ENV LISTEN 0.0.0.0:8000
EXPOSE 8000
CMD ["./brc20query"]
