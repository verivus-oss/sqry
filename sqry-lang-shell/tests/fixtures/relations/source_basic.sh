#!/bin/bash
# Basic sourcing test

# Using 'source' command
source ./config/env.sh

# Using '.' command
. ../lib/common.sh

# Absolute path
source /etc/profile.d/app.sh

# Complex path
. ./scripts/helpers.sh

function main() {
    echo "Main function"
}
