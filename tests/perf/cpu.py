"""CPU% per process over an interval: container processes (via docker top's
host PIDs) plus the given host PIDs. Usage: cpu.py SECONDS CONTAINER PID..."""
import os, subprocess, sys, time

seconds, container, host_pids = float(sys.argv[1]), sys.argv[2], sys.argv[3:]
tick = os.sysconf("SC_CLK_TCK")


def container_pids():
    out = subprocess.run(["docker", "top", container, "-eo", "pid"], capture_output=True, text=True).stdout
    return [line.strip() for line in out.splitlines()[1:] if line.strip()]


def sample(pids):
    result = {}
    for pid in pids:
        try:
            stat = open(f"/proc/{pid}/stat").read()
            cmd = open(f"/proc/{pid}/cmdline").read().replace("\0", " ")
        except OSError:
            continue
        fields = stat[stat.rindex(")") + 2:].split()
        result[pid] = (int(fields[11]) + int(fields[12]), cmd)
    return result


groups = {"container": container_pids(), "host viewer": host_pids}
before = {name: sample(pids) for name, pids in groups.items()}
start = time.time()
time.sleep(seconds)
elapsed = time.time() - start
after = {name: sample(pids) for name, pids in groups.items()}


def label(cmd):
    for key in ("Xtigervnc", "type=gpu-process", "type=renderer", "type=utility", "type=zygote",
                "WebKitWebProcess", "WebKitNetworkProcess", "docker-vm-viewer", "xfwm4", "xfce4-panel"):
        if key in cmd:
            return key
    if "chromium" in cmd:
        return "chromium (browser)"
    return cmd.split(" ")[0].rsplit("/", 1)[-1][:30]


for name in groups:
    totals = {}
    for pid, (ticks, cmd) in after[name].items():
        if pid in before[name]:
            totals[label(cmd)] = totals.get(label(cmd), 0) + (ticks - before[name][pid][0])
    print(f"-- {name}")
    for key, ticks in sorted(totals.items(), key=lambda kv: -kv[1]):
        pct = 100 * ticks / tick / elapsed
        if pct >= 1:
            print(f"   {pct:6.1f}%  {key}")

