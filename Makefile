.PHONY: default check-format lint tag test

# Used by the tag target to create matching commits and tags with the contents
# of the Changelog.
TAG_VERSION = $(shell cargo pkgid | sed -e 's,.*#,,' -e 's,.*@,,')

default:
	@cargo build

check-format:
	@cargo fmt --check

lint: check-format
	@cargo clippy --no-deps --all-targets -- --deny warnings

test:
	@cargo test

tag:
	@if [[ -z "$(TAG_VERSION)" ]]; then echo "No TAG_VERSION set"; false; fi
	@which parse-changelog >/dev/null
	@if git describe --exact-match v$(TAG_VERSION) &>/dev/null; then \
		echo "$(TAG_VERSION) already exists"; \
		false; \
	fi
	@if [[ -z "$$(parse-changelog --prefix-format v CHANGELOG.md $(TAG_VERSION))" ]]; then \
		echo "No changelog entry for $(TAG_VERSION)"; \
		false; \
	fi
	@git -c core.commentchar=: commit -a -m chetter-app-$(TAG_VERSION) \
		-m "$$(parse-changelog --prefix-format v CHANGELOG.md $(TAG_VERSION))"
	@git -c core.commentchar=: tag -a -m chetter-app-$(TAG_VERSION) \
		-m "$$(parse-changelog --prefix-format v CHANGELOG.md $(TAG_VERSION))" \
		v$(TAG_VERSION)
