#!/usr/bin/env bash
set -eo pipefail

# This file is called by GoW's inherited /opt/gow/startup.sh as the configured
# non-root account. launcher copies /cfg/sway/config into the persistent home,
# applies Wolf's virtual-display dimensions, then starts Sway.
# The inherited GoW launcher reads several optional environment variables
# directly, so it must not be sourced with nounset enabled.
export RUN_GAMESCOPE="${RUN_GAMESCOPE:-}"
export RUN_SWAY="${RUN_SWAY:-1}"
export SWAY_STOP_ON_APP_EXIT="${SWAY_STOP_ON_APP_EXIT:-no}"

# The GoW launcher rewrites the display-specific main config at every start;
# this optional include is deliberately kept in the persistent home instead.
mkdir -p "$HOME/.config/sway"
touch "$HOME/.config/sway/custom-cfg"

source /opt/gow/launch-comp.sh
launcher /opt/gow/desktop-session
