"""Runs a command and prints wall time, CPU time and peak memory (RSS)."""
import resource
import subprocess
import sys
import time

label, cmd = sys.argv[1], sys.argv[2:]
start = time.time()
proc = subprocess.run(cmd, capture_output=True, text=True)
usage = resource.getrusage(resource.RUSAGE_CHILDREN)
if proc.returncode != 0:
    sys.exit(f"{label}: failed\n{proc.stderr}")
print(f"| {label} | {time.time() - start:.2f} s | {usage.ru_utime + usage.ru_stime:.2f} s | {usage.ru_maxrss / 1024:.0f} MB |")
