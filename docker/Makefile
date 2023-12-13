TOPDIR := $(realpath $(dir $(lastword $(MAKEFILE_LIST)))../)

REGISTRY ?= localhost
VERSION ?= $(shell git describe)
CONTAINER_APP ?= podman

.PHONY: all clean

all: chetter-app

%: %.dockerfile
	@$(CONTAINER_APP) build \
		--tag=$(REGISTRY)/$@:$(VERSION:v%=%) \
		--file=$< \
		$(TOPDIR)

chetter-app: chetter-app.dockerfile

clean:
	@$(CONTAINER_APP) image rm -f \
		$(REGISTRY)/chetter-app:$(VERSION:v%=%)
