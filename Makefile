.PHONY: build
build: 
	cargo build --release

.PHONY: install
install: build
	sudo install ./target/release/containerd-shim-youki-v1 /usr/local/bin

.PHONY: build-app
build-app:
	cd py-cmd-app && docker build -t py-cmd-app:latest .
	cd py-flask-app && docker build -t py-flask-app:latest .

.PHONY: load-app
load-app: build-app
	mkdir -p test/out_cmd
	mkdir -p test/out_flask
	docker save -o test/out_flask/img.tar py-flask-app:latest
	docker save -o test/out_cmd/img.tar py-cmd-app:latest

	sudo ctr images import test/out_flask/img.tar
	sudo ctr images import test/out_cmd/img.tar

.PHONY: run
run: install load-app
	sudo ctr run --net-host --rm --runtime=io.containerd.youki.v1 docker.io/library/py-flask-app:latest pyflask