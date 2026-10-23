#!/usr/bin/env python3.11
"""Read the mounted root/data devices and their actual ext4 geometry."""
import json
import os
from pathlib import Path
import subprocess


def collect():
    evidence = {"boot_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip(),
                "commands": {}, "mounts": {}}

    def command(label, *arguments):
        result = subprocess.run(arguments, capture_output=True, text=True, timeout=30,
                                env=dict(os.environ, LC_ALL="C"))
        evidence["commands"][label] = {"argv": arguments, "exit_code": result.returncode,
                                       "stdout": result.stdout, "stderr": result.stderr}
        if result.returncode:
            raise RuntimeError(label + " failed")
        return result.stdout

    try:
        command("crypt", "dmsetup", "status", "--target", "crypt")
        command("verity", "dmsetup", "status", "--target", "verity")
        for label, target in [("root", "/"), ("data", "/opt/data/allinone")]:
            mount = json.loads(command(label + "-findmnt", "findmnt", "--json", "--target", target,
                                      "--output", "TARGET,SOURCE,FSTYPE,MAJ:MIN,OPTIONS"))["filesystems"]
            if len(mount) != 1:
                raise RuntimeError(label + " has ambiguous mounted device")
            mount = mount[0]
            device = Path("/dev/block") / mount["maj:min"]
            info = os.statvfs(target)
            geometry = command(label + "-ext4", "dumpe2fs", "-h", str(device))
            fields = dict(line.split(":", 1) for line in geometry.splitlines() if ":" in line)
            evidence["mounts"][label] = {"mount": mount, "device": str(device.resolve()),
                "device_bytes": int(command(label + "-device-size", "blockdev", "--getsize64", str(device))),
                "filesystem_blocks": int(fields["Block count"].strip()),
                "filesystem_block_bytes": int(fields["Block size"].strip()),
                "filesystem_state": fields["Filesystem state"].strip(),
                "statvfs_bytes": info.f_blocks * info.f_frsize,
                "cryptpilot_root_device": os.stat(device).st_rdev == os.stat("/dev/mapper/rootfs").st_rdev,
                # Topology comes from sysfs. Asking for FSTYPE can probe internal
                # authenticated COW devices whose unused sectors have no tags.
                "device_chain": json.loads(command(label + "-device-chain", "lsblk", "--json", "--bytes",
                    "--inverse", "--output", "NAME,TYPE,SIZE,MAJ:MIN", str(device)))["blockdevices"]}
    except Exception as error:
        evidence["error"] = str(error)
    return evidence


if __name__ == "__main__":
    print(json.dumps(collect()))
