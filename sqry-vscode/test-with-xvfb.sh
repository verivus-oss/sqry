#!/bin/bash
# Run VS Code integration tests with xvfb

set -e

echo "🧪 sqry VS Code Extension Test Runner"
echo "======================================"
echo ""

# Check if xvfb is installed
if ! command -v xvfb-run &> /dev/null; then
    echo "❌ xvfb is not installed"
    echo ""
    echo "Install with:"
    echo "  sudo apt-get install -y xvfb  # Ubuntu/Debian"
    echo "  sudo dnf install -y xorg-x11-server-Xvfb  # Fedora/RHEL"
    echo ""
    echo "Or run on a machine with a display:"
    echo "  npm run test:integration"
    echo ""
    exit 1
fi

echo "✅ xvfb found"
echo ""

# Run unit tests first (fast)
echo "Running unit tests..."
npm run test:unit

echo ""
echo "Running integration tests with xvfb..."
# Unset ELECTRON_RUN_AS_NODE to prevent VS Code from running as Node.js
# This is necessary when running from VS Code's integrated terminal
unset ELECTRON_RUN_AS_NODE
xvfb-run -a npm run test:integration

echo ""
echo "✅ All tests completed!"
