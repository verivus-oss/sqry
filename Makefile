# sqry Makefile — Publication Pipeline v2 targets

.PHONY: preflight

# Advisory local preflight — CI independently recomputes all checks.
# Usage: make preflight VERSION=v4.8.17
preflight:
ifndef VERSION
	$(error VERSION is required. Usage: make preflight VERSION=v4.8.17)
endif
	@./scripts/release/preflight.sh $(VERSION)
