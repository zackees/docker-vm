# docker-vm

A GPU-accelerated Linux desktop in Docker, accessed with Moonlight. The stack
uses Games on Whales Wolf, Sway, and Chromium; it is a container desktop, not a
virtual machine.

Start with [setup and validation](docs/SETUP.md), then read [DESIGN.md](DESIGN.md)
for architecture and [benchmark rules](docs/BENCHMARKS.md) before making a
performance claim.

```bash
./scripts/preflight
./scripts/configure
./scripts/build
docker compose -f compose.yaml -f compose.nvidia.yaml up -d
# pair from Moonlight, then:
./scripts/doctor --wolf docker-vm-wolf --desktop <live-worker-name> --strict
```

`doctor --strict` deliberately fails when evidence is missing. Browser rendering,
browser media decoding, server encoding, and client decoding are separate gates.
