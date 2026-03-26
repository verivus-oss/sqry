# sqry Makefile — Publication Pipeline v2 targets

.PHONY: preflight install-hooks

# Advisory local preflight — CI independently recomputes all checks.
# Usage: make preflight VERSION=v4.8.17
preflight:
ifndef VERSION
	$(error VERSION is required. Usage: make preflight VERSION=v4.8.17)
endif
	@./scripts/release/preflight.sh $(VERSION)

# Install git hooks from scripts/. Safe to re-run (idempotent).
install-hooks:
	@echo "Installing git hooks..."
	@ln -sf ../../scripts/pre-commit-compliance.sh .git/hooks/pre-commit
	@chmod +x scripts/pre-commit-compliance.sh
	@echo "  pre-commit -> scripts/pre-commit-compliance.sh"
	@echo "Done. Run 'make install-hooks' after cloning to activate."
