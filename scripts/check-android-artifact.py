#!/usr/bin/env python3
"""Check final ELF/RELRO layout and APK native-library zip alignment."""
import subprocess
import sys

readelf, library, zipalign, apk = sys.argv[1:]
headers = subprocess.check_output([readelf, "-lW", library], text=True)
loads = 0
relro = False
for line in headers.splitlines():
    fields = line.split()
    if fields and fields[0] == "LOAD":
        loads += 1
        alignment = int(fields[-1], 16)
        assert alignment >= 16384, f"LOAD alignment below 16KB: {line}"
        assert (int(fields[1], 16) - int(fields[2], 16)) % 16384 == 0, line
    elif fields and fields[0] == "GNU_RELRO":
        relro = True
        assert (int(fields[2], 16) + int(fields[5], 16)) % 16384 == 0, line
assert loads and relro, "Missing LOAD or GNU_RELRO program headers"
subprocess.run([zipalign, "-c", "-P", "16", "4", apk], check=True)
print("16KB ELF LOAD/RELRO and APK alignment passed")
