#!/usr/bin/env python3
"""Real terminal grouping, live metrics and scope isolation on the owned kind fixture."""
import codecs
import fcntl
import json
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
import uuid
import pyte

config = os.environ.get('KUBECONFIG', '')
if not config or 'kind-kube-tui-' not in pathlib.Path(config).read_text():
    raise SystemExit('Use the dedicated kind-kube-tui-* kubeconfig for this test.')
binary = str(pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else 'target/release/kube').resolve())
name = 'kube-dashboard-' + uuid.uuid4().hex[:6]
fixture = {'apiVersion': 'v1', 'kind': 'Pod', 'metadata': {'name': name, 'namespace': 'demo', 'labels': {'app': name}}, 'spec': {'containers': [{'name': 'worker', 'image': 'nginx:alpine'}]}}
subprocess.run(['kubectl', 'create', '-f', '-'], input=json.dumps(fixture), text=True, check=True, stdout=subprocess.DEVNULL)
master, slave = pty.openpty()
before = termios.tcgetattr(slave)
screen = pyte.Screen(150, 40)
stream = pyte.Stream(screen)
decoder = codecs.getincrementaldecoder('utf-8')(errors='replace')
process = None


def pump(seconds=.15):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        if select.select([master], [], [], min(.02, max(0, until-time.monotonic())))[0]:
            stream.feed(decoder.decode(os.read(master, 65536)))


def text():
    return '\n'.join(screen.display)


def send(keys):
    os.write(master, keys.encode())
    pump()


def expect(value, timeout=30):
    until = time.monotonic() + timeout
    while value not in text() and time.monotonic() < until:
        pump(.05)
    assert value in text(), f'Missing {value!r}:\n{text()}'


def resize(width, height):
    screen.resize(height, width)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', height, width, 0, 0))
    if process:
        process.send_signal(signal.SIGWINCH)
        pump(.3)


def capture(label):
    directory = os.environ.get('KUBE_UI_CAPTURE_DIR')
    if directory:
        out = pathlib.Path(directory)
        out.mkdir(parents=True, exist_ok=True)
        cells = [[screen.buffer[y][x]._asdict() for x in range(screen.columns)] for y in range(screen.lines)]
        (out / f'{label}.json').write_text(json.dumps(cells))
        (out / f'{label}.txt').write_text(text())


try:
    with tempfile.TemporaryDirectory(prefix='kube-dashboard-') as temp:
        groups = pathlib.Path(temp) / 'groups.yaml'
        groups.write_text('version: 1\ngroups:\n- name: Web services\n  namespace: demo\n  selector:\n    matchLabels: {app: web}\n- name: Background workers\n  namespace: demo\n  selector:\n    matchLabels: {app: ' + name + '}\n')
        resize(150, 40)
        env = dict(os.environ, TERM='xterm-256color', XDG_CONFIG_HOME=temp)
        env.pop('NO_COLOR', None)
        process = subprocess.Popen([binary, '--kubeconfig', config, '--groups', str(groups), '-n', 'demo'], stdin=slave, stdout=slave, stderr=slave, env=env)
        expect('Fleet radar')
        expect('Web services')
        expect('Background workers')
        expect('mCPU', 90)
        expect('4/4 pods measured', 90)
        pump(10.2)  # Multiple real samples, not a fabricated graph.
        capture('fleet-radar')
        # Alphabetical group order: background first; Enter must not show web pods.
        send('\r')
        expect('Pod · demo / Background workers')
        expect(name)
        assert 'web-' not in text(), text()
        send('\x1b')
        expect('Fleet radar')
        row = next(i for i, line in enumerate(screen.display) if 'Web services' in line)
        send(f'\x1b[<0;45;{row + 1}M\x1b[<0;45;{row + 1}m')
        expect('Pod · demo / Web services')
        expect('web-')
        assert name not in text(), text()
        # Inspect then unwind, with repeated Esc staying in the application.
        send('\r')
        expect('Overview')
        send('\x1b')
        expect('Pod · demo / Web services')
        send('\x1b')
        expect('Fleet radar')
        for _ in range(4):
            send('\x1b')
            assert process.poll() is None
        resize(80, 24)
        expect('Web services')
        capture('fleet-small')
        send('a')
        expect('ATTENTION')
        send('g')
        expect('POD GROUPS')
        resize(150, 40)
        send('\x0e')
        expect('Namespaces')
        send('kube-system\r')
        expect('coredns')
        expect('mCPU')
        assert 'Web services' not in text() and 'Background workers' not in text(), text()
        capture('fleet-system')
        send('\x0e')
        expect('Namespaces')
        send('demo\r')
        expect('Web services')
        send('\x1bOR')  # F3 clears grouping and returns the complete list.
        expect(name)
        expect('web-')
        send('q')
        process.wait(timeout=5)
        pump()
        assert process.returncode == 0
        assert termios.tcgetattr(slave) == before
        print('Dashboard: real metrics, custom groups, scoped drilldown, Esc, narrow layout and namespace reset passed.')
finally:
    if process and process.poll() is None:
        process.kill()
        process.wait()
    os.close(master)
    os.close(slave)
    subprocess.run(['kubectl', '-n', 'demo', 'delete', 'pod', name, '--wait=false'], stdout=subprocess.DEVNULL, check=False)
