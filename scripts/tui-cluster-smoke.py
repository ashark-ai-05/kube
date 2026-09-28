#!/usr/bin/env python3
"""Exercise the rendered TUI against the isolated kind acceptance cluster."""
import codecs
import fcntl
import os
import pathlib
import pty
import select
import socket
import struct
import subprocess
import sys
import tempfile
import termios
import time
import urllib.request

import pyte

config = os.environ.get('KUBECONFIG', '')
if not config or 'kind-kube-tui-' not in pathlib.Path(config).read_text():
    raise SystemExit('Use the dedicated kind-kube-tui-* kubeconfig for this test.')
binary = str(pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/kube').resolve())
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 45, 180, 0, 0))
before = termios.tcgetattr(slave)
screen = pyte.Screen(180, 45)
stream = pyte.Stream(screen)
decoder = codecs.getincrementaldecoder('utf-8')(errors='replace')
with tempfile.TemporaryDirectory(prefix='kube-ui-') as temp:
    env = dict(os.environ, TERM='xterm-256color', XDG_CONFIG_HOME=temp)
    process = subprocess.Popen([binary, '-n', 'demo'], stdin=slave, stdout=slave, stderr=slave, env=env)

    def pump(seconds):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            if select.select([master], [], [], .02)[0]:
                stream.feed(decoder.decode(os.read(master, 65536)))

    def send(text):
        os.write(master, text.encode())
        pump(.2)

    def expect(text, timeout=15):
        until = time.monotonic() + timeout
        while text not in '\n'.join(screen.display) and time.monotonic() < until:
            pump(.05)
        assert text in '\n'.join(screen.display), f'Missing {text!r}:\n' + '\n'.join(screen.display)

    try:
        start = time.monotonic()
        expect('web-', 20)
        populated_ms = (time.monotonic() - start) * 1000
        print(f'First populated view: {populated_ms:.0f} ms', flush=True)
        send('/web-\r')
        pump(.2)
        send('\r')
        expect('Overview')
        expect('Related')
        send('4')
        expect('FOLLOW')
        expect('Streaming')
        send(':help\r')
        expect('Keyboard & mouse')
        send('\x1b')
        send('\x1b')
        send(':exec\r')
        pump(1)
        send("printf 'kube-exec-%s\\n' acceptance\r")
        expect('kube-exec-acceptance')
        send('exit\r')
        expect('Shell exited')
        with socket.socket() as unused:
            unused.bind(('127.0.0.1', 0))
            port = unused.getsockname()[1]
        send(f':forward {port}:80\r')
        expect('Forwarding from')
        assert urllib.request.urlopen(f'http://127.0.0.1:{port}', timeout=3).status == 200
        send(':stop-forwards\r')
        expect('Port forwards stopped')
        pump(.3)
        with socket.socket() as stopped:
            assert stopped.connect_ex(('127.0.0.1', port)) != 0, 'Forward still listening'
        send('q')
        pump(.3)
        process.wait(timeout=5)
        assert process.returncode == 0
        assert termios.tcgetattr(slave) == before
        print('PASS: filter → inspector → live logs → palette → exec → port-forward → quit')
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        os.close(master)
        os.close(slave)
