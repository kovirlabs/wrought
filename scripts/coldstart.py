#!/usr/bin/env python3
"""Cold start of `engine serve`, measured (scripts/check-coldstart.sh).

    coldstart.py MODE RUNS PATH -- ENGINE ARGS...

MODE `activate` does what systemd does for `hammerwright-site@.socket`: binds
and listens on a TCP port, then starts the engine with that socket as file
descriptor 3 (LISTEN_FDS=1, LISTEN_PID = the engine's own pid). A client
connects at once (the kernel queues the connection until the engine
accepts it) and sends one GET.

MODE `bind` starts the engine normally: `{port}` in the arguments is
replaced by a free port, and the client retries connecting every
millisecond until the engine listens.

The time is from just before the engine process is created to the first
byte of the response. The engine is stopped with SIGTERM after each run and
must exit with status 0. Prints one line per run and a summary line:
    median=<ms> max=<ms>
Exits non-zero if any response is not 200 or the engine did not exit
cleanly. COLDSTART_LOG names a file for the engine's output.
"""

import os
import signal
import socket
import statistics
import sys
import time


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


def spawn(argv, listen_fd=None):
    pid = os.fork()
    if pid == 0:
        try:
            env = dict(os.environ)
            if listen_fd is not None:
                os.dup2(listen_fd, 3)
                os.set_inheritable(3, True)
                env["LISTEN_FDS"] = "1"
                env["LISTEN_PID"] = str(os.getpid())
                env["LISTEN_FDNAMES"] = "http"
            log = os.environ.get("COLDSTART_LOG")
            out = (
                os.open(log, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
                if log
                else os.open(os.devnull, os.O_WRONLY)
            )
            os.dup2(out, 1)
            os.dup2(out, 2)
            os.execve(argv[0], argv, env)
        finally:
            os._exit(127)
    return pid


def one(mode, argv, path):
    if mode == "activate":
        ls = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        ls.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        ls.bind(("127.0.0.1", 0))
        ls.listen(128)
        port = ls.getsockname()[1]
        t0 = time.perf_counter()
        pid = spawn(argv, ls.fileno())
        ls.close()
        c = socket.create_connection(("127.0.0.1", port), timeout=30)
    else:
        port = free_port()
        argv = [a.replace("{port}", str(port)) for a in argv]
        t0 = time.perf_counter()
        pid = spawn(argv)
        while True:
            try:
                c = socket.create_connection(("127.0.0.1", port), timeout=30)
                break
            except ConnectionRefusedError:
                if time.perf_counter() - t0 > 30:
                    raise
                time.sleep(0.001)
    c.settimeout(30)
    c.sendall(
        f"GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n".encode()
    )
    first = c.recv(1)
    t1 = time.perf_counter()
    rest = first
    while True:
        b = c.recv(65536)
        if not b:
            break
        rest += b
    c.close()
    os.kill(pid, signal.SIGTERM)
    _, status = os.waitpid(pid, 0)
    line = rest.split(b"\r\n", 1)[0].decode("latin-1")
    code = int(line.split()[1]) if line.startswith("HTTP/") else 0
    return (t1 - t0) * 1000.0, code, os.waitstatus_to_exitcode(status)


def main():
    if "--" not in sys.argv or len(sys.argv) < 5:
        sys.exit(__doc__)
    i = sys.argv.index("--")
    mode, runs, path = sys.argv[1], int(sys.argv[2]), sys.argv[3]
    if mode not in ("activate", "bind"):
        sys.exit(__doc__)
    argv = sys.argv[i + 1 :]
    times = []
    bad = False
    for n in range(runs):
        ms, code, exit_code = one(mode, argv, path)
        print(f"  run {n + 1}: {ms:7.1f} ms  HTTP {code}  exit {exit_code}")
        times.append(ms)
        if code != 200 or exit_code != 0:
            bad = True
    print(f"median={statistics.median(times):.1f} max={max(times):.1f}")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
