#!/usr/bin/env python3
"""Exercise the rendered TUI against the isolated kind acceptance cluster."""
import codecs
import fcntl
import json
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
import uuid

import pyte

config = os.environ.get('KUBECONFIG', '')
if not config or 'kind-kube-tui-' not in pathlib.Path(config).read_text():
    raise SystemExit('Use the dedicated kind-kube-tui-* kubeconfig for this test.')
binary_args = [arg for arg in sys.argv[1:] if not arg.startswith('--')]
binary = str(pathlib.Path(binary_args[0] if binary_args else 'target/debug/kube').resolve())
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 45, 180, 0, 0))
before = termios.tcgetattr(slave)
stress_name = None
if '--stress' in sys.argv:
    stress_name = 'kube-ui-load-' + uuid.uuid4().hex[:6]
    fixture = {
        'apiVersion': 'v1', 'kind': 'Pod',
        'metadata': {'name': stress_name, 'namespace': 'demo'},
        'spec': {'restartPolicy': 'Never', 'containers': [{
            'name': 'load', 'image': 'nginx:alpine',
            'command': ['sh', '-c', 'while true; do seq 1 1000; sleep 0.1; done']
        }]}
    }
    subprocess.run(['kubectl', '-n', 'demo', 'create', '-f', '-'],
                   input=json.dumps(fixture), text=True, check=True, stdout=subprocess.DEVNULL)
screen = pyte.Screen(180, 45)
stream = pyte.Stream(screen)
decoder = codecs.getincrementaldecoder('utf-8')(errors='replace')
with tempfile.TemporaryDirectory(prefix='kube-ui-') as temp:
    env = dict(os.environ, TERM='xterm-256color', XDG_CONFIG_HOME=temp, KUBECONFIG=str(pathlib.Path(temp) / 'missing-config'))
    process = subprocess.Popen([binary, '--kubeconfig', config, '-n', 'demo'], stdin=slave, stdout=slave, stderr=slave, env=env)

    def pump(seconds):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            if select.select([master], [], [], min(.02, max(0, until - time.monotonic())))[0]:
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
        expect('Pod monitor', 20)
        send('\x1bOR')
        expect('web-', 20)
        populated_ms = (time.monotonic() - start) * 1000
        print(f'First populated view: {populated_ms:.0f} ms', flush=True)
        send('/web-\r')
        pump(.2)
        send('\r')
        expect('Overview')
        expect('Related')
        assert 'kind-kube-tui-' in screen.display[-1] and 'demo' in screen.display[-1], 'Context must remain visible while inspecting'
        if os.environ.get('KUBE_SCREEN_DUMP'):
            pathlib.Path(os.environ['KUBE_SCREEN_DUMP']).write_text('\n'.join(screen.display))
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
        if stress_name:
            send('/\x15' + stress_name + '\r')
            send('l')
            expect('Streaming', 60)
            pump(1)
            samples = []
            for i in range(50):
                expected = 'PAUSED' if i % 2 == 0 else 'FOLLOW'
                started = time.monotonic()
                os.write(master, b'f')
                while expected not in '\n'.join(screen.display):
                    assert time.monotonic() - started < 2, 'Input stalled under log load'
                    pump(.001)
                samples.append((time.monotonic() - started) * 1000)
                pump(.02)
            p95 = sorted(samples)[47]
            print(f'Live log load (~10,000 lines/s): key-to-render p95 {p95:.1f} ms', flush=True)
            assert p95 < 50, f'Input p95 {p95:.1f} ms exceeded 50 ms budget'
            send('\x1b')
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
        if stress_name:
            subprocess.run(['kubectl', '-n', 'demo', 'delete', 'pod', stress_name, '--wait=false'], stdout=subprocess.DEVNULL, check=False)
        os.close(master)
        os.close(slave)
