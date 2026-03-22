#!/bin/sh
# Dot command variations

. ./init.sh
. config.sh
. /usr/local/lib/utils.sh

# These should be captured as-is (raw path metadata)
# Variable expansion - capture raw text
. "$CONFIG_DIR/settings.sh"
. ${HOME}/.bashrc

# Tilde expansion - capture raw text
. ~/scripts/helper.sh
