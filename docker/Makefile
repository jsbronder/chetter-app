TOPDIR := $(realpath $(dir $(lastword $(MAKEFILE_LIST)))../)

REGISTRY ?= localhost
CONTAINER_APP ?= podman

ifndef VERSION
	VERSION = $(patsubst v%,%,$(shell git describe))
endif

.PHONY: all clean

all: chetter-app

%: %.dockerfile
	@$(CONTAINER_APP) build \
		--tag=$(REGISTRY)/$@:$(VERSION) \
		--file=$< \
		$(TOPDIR)

chetter-app: chetter-app.dockerfile

clean:
	@$(CONTAINER_APP) image rm -f \
		$(REGISTRY)/chetter-app:$(VERSION)
