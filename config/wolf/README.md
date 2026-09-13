# Chromium seccomp profile

`chromium-seccomp.json` is vendored from Moby Profiles commit
[`61eaf32614c7c71b60bd8927d3e6a4ffc8ff1f31`](https://github.com/moby/profiles/blob/61eaf32614c7c71b60bd8927d3e6a4ffc8ff1f31/seccomp/default.json)
(Apache-2.0; see [LICENSE.moby](LICENSE.moby)),
with exactly one additional allow rule for `clone`, `unshare`, and `setns`.
Chromium's setuid sandbox needs those namespace operations; Docker's default
profile blocks them. All other Moby default allow and capability-gated rules
remain intact. `scripts/configure` copies the profile into protected Wolf state
and embeds its JSON in the Docker API create request.

Wolf stores the rendered Docker create JSON in `cfg/config.toml`. On later
`scripts/configure` runs it deliberately preserves that file, including paired
clients and profiles; changing this source profile does not silently alter an
already-paired worker policy. Treat a seccomp/profile upgrade as an explicit
migration: back up the state directory, stop Wolf, update the rendered Desktop
runner configuration deliberately, then revalidate pairing and streaming.
