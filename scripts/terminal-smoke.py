#!/usr/bin/env python3
"""Exercise terminal setup/cleanup against an unreachable loopback API, never a user cluster."""
import fcntl
import os
import pathlib
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

binary = str(pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/kube').resolve())
for shutdown in ('q', 'SIGTERM'):
    with tempfile.TemporaryDirectory(prefix='kube-terminal-') as temp:
        config = pathlib.Path(temp) / 'config'
        config.write_text('''apiVersion: v1
kind: Config
current-context: acceptance
clusters:
- name: acceptance
  cluster:
    server: http://127.0.0.1:9
contexts:
- name: acceptance
  context:
    cluster: acceptance
    user: acceptance
users:
- name: acceptance
  user: {}
''')
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 140, 0, 0))
        before = termios.tcgetattr(slave)
        env = dict(os.environ, KUBECONFIG=str(config), TERM='xterm-256color')
        process = subprocess.Popen([binary], stdin=slave, stdout=slave, stderr=slave, env=env)
        output = b''
        deadline = time.monotonic() + 10
        try:
            while b'\x1b[?1049h' not in output and time.monotonic() < deadline:
                if select.select([master], [], [], .1)[0]:
                    output += os.read(master, 65536)
            assert b'\x1b[?1049h' in output, f'UI did not enter alternate screen: {output[-500:]!r}'
            time.sleep(.15)
            for _ in range(4):
                os.write(master, b'\x1b')
                time.sleep(.12)
                while select.select([master], [], [], .02)[0]:
                    output += os.read(master, 65536)
                assert process.poll() is None, 'Repeated Esc unexpectedly exited the app'
            if shutdown == 'q':
                os.write(master, b'q')
            else:
                process.send_signal(signal.SIGTERM)
            stop_deadline = time.monotonic() + 5
            while process.poll() is None and time.monotonic() < stop_deadline:
                if select.select([master], [], [], .05)[0]:
                    output += os.read(master, 65536)
            process.wait(timeout=1)
            while select.select([master], [], [], .05)[0]:
                output += os.read(master, 65536)
            assert process.returncode == 0, output[-500:]
            assert termios.tcgetattr(slave) == before, 'terminal settings were not restored'
            assert b'\x1b[?1049l' in output, 'alternate screen was not restored'
            assert b'\x1b[?1000l' in output, 'mouse capture was not released'
            print(f'PASS: repeated Esc stays open; {shutdown} restores terminal settings, screen and mouse')
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)
            os.close(slave)
