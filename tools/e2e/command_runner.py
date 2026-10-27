#!/usr/bin/env python3.11
"""Bound concurrent image builds and record command timings without arguments."""
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import time
from rekor_fixture import configure_spec
from render_case import configure_repositories


@contextmanager
def build_slot(evidence):
    directory = os.environ.get("E2E_BUILD_SLOT_DIR")
    if not directory:
        yield
        return
    count = int(os.environ.get("E2E_BUILD_JOBS", "2"))
    if count < 1:
        raise ValueError("E2E_BUILD_JOBS must be positive")
    root = Path(directory)
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    descriptors = []
    selected = None
    started = time.monotonic()
    try:
        for slot in range(count):
            fd = os.open(root / (str(slot) + ".lock"), os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
            descriptors.append(fd)
            metadata = os.fstat(fd)
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid():
                raise RuntimeError("build slot must be an owned regular file")
        while selected is None:
            for fd in descriptors:
                try:
                    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    selected = fd
                    break
                except BlockingIOError:
                    pass
            if selected is None:
                time.sleep(0.1)
        Path(evidence).write_text(json.dumps({"stage": "build-slot-wait",
            "elapsed_seconds": round(time.monotonic() - started, 3)}) + "\n")
        yield
    finally:
        for fd in descriptors:
            os.close(fd)


def main():
    label, *command = sys.argv[1:]
    if command and command[0] == "--":
        command.pop(0)
    work = Path(os.environ["E2E_WORK_DIR"])
    work.mkdir(parents=True, exist_ok=True, mode=0o700)
    started = time.monotonic()
    code = None
    try:
        if label in {"build", "deploy"} and "--spec" in command:
            configure_spec(command[command.index("--spec") + 1])
        if label == "build" and "--spec" in command:
            # The caller explicitly selects the ECS HTTPS mirror. Apply it to
            # native-init and legacy builders alike before CA freezes inputs.
            configure_repositories(command[command.index("--spec") + 1])
        if label == "build":
            with build_slot(work / ("build-slot-" + str(time.time_ns()) + ".timing.json")):
                code = subprocess.call(command)
        else:
            code = subprocess.call(command)
    finally:
        with (work / "command-timings.jsonl").open("a") as target:
            target.write(json.dumps({"label": label, "elapsed_seconds": round(time.monotonic() - started, 3),
                                     "exit_code": code}) + "\n")
    raise SystemExit(code if code is not None else 1)


if __name__ == "__main__":
    main()
