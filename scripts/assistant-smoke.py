#!/usr/bin/env python3
"""Exercise scoped action previews and navigation against the isolated kind fixture."""
import codecs
import fcntl
import os
import pathlib
import pty
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
import pyte

config = os.environ.get('KUBECONFIG', '')
if not config or 'kind-kube-tui-' not in pathlib.Path(config).read_text():
    raise SystemExit('Use the dedicated kind-kube-tui-* fixture kubeconfig.')
binary = str(pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/kube').resolve())
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 35, 130, 0, 0))
before = termios.tcgetattr(slave)
screen = pyte.Screen(130, 35)
stream = pyte.Stream(screen)
decoder = codecs.getincrementaldecoder('utf-8')(errors='replace')

def pump(seconds=.15):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if select.select([master], [], [], .02)[0]:
            stream.feed(decoder.decode(os.read(master, 65536)))

def send(value):
    os.write(master, value.encode())
    pump()

def text():
    return '\n'.join(screen.display)

def expect(value):
    end = time.monotonic() + 15
    while value not in text() and time.monotonic() < end:
        pump()
    assert value in text(), f'Missing {value!r}:\n{text()}'

with tempfile.TemporaryDirectory(prefix='kube-ask-') as temp:
    env = dict(os.environ, TERM='xterm-256color', XDG_CONFIG_HOME=temp)
    env.pop('NO_COLOR', None)
    process = subprocess.Popen([binary, '--kubeconfig', config, '-n', 'demo'], stdin=slave, stdout=slave, stderr=slave, env=env)
    try:
        expect('Pod monitor')
        send('\x1bOR')
        expect('web-')
        send('/web-\r')
        send('\x00')
        expect('Ask Kube')
        send('\x1b')
        send(':ask Show failing pods in demo\r')
        expect('Enter apply this action')
        expect('Show unhealthy Pod')
        assert 'health:unhealthy' not in text(), 'Preview must not apply automatically'
        if os.environ.get('KUBE_UI_CAPTURE_DIR'):
            import json
            directory = pathlib.Path(os.environ['KUBE_UI_CAPTURE_DIR'])
            directory.mkdir(parents=True, exist_ok=True)
            (directory / 'ask-preview.json').write_text(json.dumps([[screen.buffer[y][x]._asdict() for x in range(screen.columns)] for y in range(screen.lines)]))
        send('\r')
        expect('health:unhealthy')
        send(':ask Show deployments\r')
        expect('Show Deployment')
        send('\r')
        expect('Deployment')
        expect('web')
        send(':ask Switch namespace kube-system\r')
        expect('Select namespace kube-system')
        assert 'ns: demo' in screen.display[0]
        send('\r')
        expect('ns: kube-system')
        send(':ask please delete all pods\r')
        expect('supports reading and navigation')
        assert 'Enter apply this action' not in text()
        send('\x1b')
        send(':ask Switch namespace demo\r')
        expect('Select namespace demo')
        send('\r')
        expect('ns: demo')
        send(':ask Show pods\r')
        send('\r')
        expect('web-')
        send('/web-\r')
        send(':ask Why is this pod stuck?\r')
        expect('Open overview')
        send('\r')
        expect('Observed Kubernetes state')
        send(':ask Previous logs for this pod\r')
        expect('Open previous container logs')
        send('\x1b')
        expect('Observed Kubernetes state')
        if os.environ.get('KUBE_TEST_LOCAL_MODEL'):
            send(':ask I want the manifest of the selected deployment\r')
            expect('local model available')
            expect('Interpreting locally')
            started=time.monotonic()
            send('\x1b')
            expect('Observed Kubernetes state')
            assert time.monotonic()-started < 1.0, 'Local inference cancellation must not block the UI'
            send(':ask I want the manifest of the selected deployment\r')
            expect('Open yaml for the selected resource')
            expect('Enter apply this action')
            send('\r')
            expect('apiVersion:')
        send('\x03')
        process.wait(timeout=5)
        assert process.returncode == 0
        assert termios.tcgetattr(slave) == before
        print('PASS: preview → scoped health filter → kind → namespace → refused mutation → selected diagnosis → cancel → terminal restoration')
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        os.close(master)
        os.close(slave)
