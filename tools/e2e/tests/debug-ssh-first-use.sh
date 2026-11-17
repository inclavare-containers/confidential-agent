#!/usr/bin/env bash
set -euo pipefail
python3.11 - "$(cd "$(dirname "$0")/../../.." && pwd)" <<'PY'
import getpass
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(sys.argv[1]) / "tools/e2e"))
from cloud_case import Case, run

sshd = shutil.which("sshd") or "/usr/sbin/sshd"
assert Path(sshd).is_file() and os.geteuid() == 0, "this integration check requires a root SSH test host"
assert Path("/run/sshd").is_dir(), "create the host's /run/sshd directory before this check"

with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    for name in ["client", "host-a", "host-b"]:
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(root / name)], check=True)
    (root / "authorized_keys").write_bytes((root / "client.pub").read_bytes())
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]

    class LocalCase(Case):
        def __init__(self, name):
            self.work = root / name
            self.work.mkdir()

        def command(self, label, *arguments, **options):
            # Keep the real SSH transport while replacing the cloud address.
            extra = arguments[arguments.index("--") + 1:]
            return run(["ssh", "-F", "/dev/null", "-p", str(port), "-i", root / "client",
                        "-o", "GlobalKnownHostsFile=/dev/null", getpass.getuser() + "@127.0.0.1", *extra],
                       self.work / label, timeout=15, **options)

    def start(host):
        config = root / "sshd_config"
        config.write_text(f"""ListenAddress 127.0.0.1
Port {port}
HostKey {root / host}
PidFile {root / 'sshd.pid'}
AuthorizedKeysFile {root / 'authorized_keys'}
StrictModes no
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin yes
UsePAM yes
LogLevel ERROR
""")
        # Connection children can retain stderr after the listener exits.
        with (root / f"{host}-sshd.log").open("wb") as log:
            process = subprocess.Popen([sshd, "-D", "-e", "-f", str(config)],
                                       stdout=subprocess.DEVNULL, stderr=log)
        try:
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                assert process.poll() is None, "local SSH test server exited"
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                        return process
                except OSError:
                    time.sleep(0.05)
            raise RuntimeError("local SSH test server did not start")
        except BaseException:
            process.terminate()
            process.communicate(timeout=5)
            raise

    def stop(process):
        process.terminate()
        process.communicate(timeout=5)

    first = LocalCase("first-case")
    process = start("host-a")
    try:
        assert first.ssh("debug", "first-use", "printf ready").stdout == b"ready"
        known = first.work / "debug-ssh-known-hosts"
        original = known.read_bytes()
        assert original
        assert first.ssh("debug", "reconnect", "printf ready").stdout == b"ready"
        assert known.read_bytes() == original
    finally:
        stop(process)
    process = start("host-b")
    try:
        rejected = first.ssh("debug", "changed-host-key", "printf unsafe", check=False)
        assert rejected.returncode != 0 and b"unsafe" not in rejected.stdout
        assert b"Host key verification failed" in rejected.stderr
        assert known.read_bytes() == original
        second = LocalCase("second-case")
        assert second.ssh("debug", "independent-first-use", "printf ready").stdout == b"ready"
        assert (second.work / "debug-ssh-known-hosts").read_bytes() != original
    finally:
        stop(process)
print("debug SSH first use, reconnect, changed-key rejection, and case isolation passed")
PY
