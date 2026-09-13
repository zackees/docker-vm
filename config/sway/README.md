# Desktop Sway configuration

The desktop worker installs its versioned Sway template at build time as
`/cfg/sway/config`.  GoW copies that file to the persistent user's
`$HOME/.config/sway/config` immediately before it starts Sway and appends the
Wolf virtual-output dimensions and desktop-session command.

To preserve local Sway preferences, place them in
`$HOME/.config/sway/custom-cfg`; the generated configuration includes it on
every start. Do not edit the generated `config` directly.

Keyboard shortcuts: `Super+Enter` terminal, `Super+B` browser, `Super+E` file
manager, `Super+D` launcher, and `Super+Shift+Q` closes the focused window.
