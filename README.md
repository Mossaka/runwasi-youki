# runwasi-youki

This repo contains an implementation of [runwasi](https://github.com/containerd/runwasi) `Instance` trait using [youki](https://github.com/containers/youki)'s `Container` APIs.

## Requirements

- Check out youki's repo for the [requirements](https://github.com/containers/youki#dependencies).
- `ctr` from [containerd](https://containerd.io/downloads/)
- `docker` from [docker](https://docs.docker.com/get-docker/)

## Build

```bash
make build
```

## Run using ctr

```bash
make run
```

## Check containerd log

```bash
sudo journalctl -u containerd --reverse  
```
