.PHONY: build
build: 
	cargo build --release

.PHONY: install
install: build
	sudo install ./target/release/containerd-shim-youki-v1 /usr/local/bin

.PHONY: build-app
build-app:
	docker build -t py-cmd-app:latest apps/py-cmd-app
	docker build -t py-flask-app:latest apps/py-flask-app
	docker build -t wasi-hello-world:latest apps/wasi-hello-world

.PHONY: load-app
load-app: build-app
	mkdir -p apps/images/out_cmd
	mkdir -p apps/images/out_flask
	mkdir -p apps/images/out_hello

	docker save -o apps/images/out_flask/img.tar py-flask-app:latest
	docker save -o apps/images/out_cmd/img.tar py-cmd-app:latest
	docker save -o apps/images/out_hello/img.tar wasi-hello-world:latest

	sudo ctr images import apps/images/out_flask/img.tar
	sudo ctr images import apps/images/out_cmd/img.tar
	sudo ctr images import apps/images/out_hello/img.tar

.PHONY: run
run: install load-app
	sudo ctr run --net-host --rm --runtime=io.containerd.youki.v1 docker.io/library/py-flask-app:latest pyflask

.PHONY: run-wasm
run-wasm: install load-app
	sudo ctr run --rm --runtime=io.containerd.youki.v1 --annotation run.oci.handler=wasm docker.io/library/wasi-hello-world:latest wasmhello /wasi-hello-world.wasm