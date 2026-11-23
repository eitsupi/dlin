"""Run a command with a time limit that stops the processes it started."""

from __future__ import annotations

import os
import signal
import subprocess
from pathlib import Path


def run(argv: list[str], cwd: Path | None = None, timeout: float | None = None) -> subprocess.CompletedProcess[str]:
    """Run like subprocess.run(capture_output=True, text=True, timeout=timeout).

    subprocess.run kills only the command on a timeout, so worker processes it
    started (such as dbt-meta's multiprocessing pool) keep running. The command
    runs here in its own process group, and the whole group is killed when the
    time limit passes or the caller is interrupted.
    """
    with subprocess.Popen(
        argv,
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    ) as process:
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            kill_group(process.pid)
            stdout, stderr = process.communicate()
            raise subprocess.TimeoutExpired(argv, timeout, output=stdout, stderr=stderr) from None
        except BaseException:
            kill_group(process.pid)
            raise
    return subprocess.CompletedProcess(argv, process.returncode, stdout, stderr)


def kill_group(pgid: int) -> None:
    try:
        os.killpg(pgid, signal.SIGKILL)
    except ProcessLookupError:
        pass
