#!/usr/bin/env python3
"""Real-terminal acceptance for pickers, custom kubeconfigs and wrapped log search.
Requires pyte and the isolated kind cluster. KUBE_UI_CAPTURE_DIR saves visual QA.
"""
import codecs
import copy
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
binary = str(pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/kube').resolve())
name = 'kube-ui-long-' + uuid.uuid4().hex[:6]
message = 'BEGIN-LONG ' + 'request_id=abc123 service=payments elapsed=42ms ' * 14 + 'TAIL-VISIBLE'
fixture = {
    'apiVersion': 'v1', 'kind': 'Pod', 'metadata': {'name': name, 'namespace': 'demo'},
    'spec': {'restartPolicy': 'Never', 'containers': [{
        'name': 'payments', 'image': 'nginx:alpine',
        'command': ['sh', '-c', 'printf "%s\\n" "$MESSAGE" "context-before" "error: timeout?" "context-after" "error: retry?" " at first.fn(Main.java:1)" " at hidden.fn(Main.java:2)" " at last.fn(Main.java:3)" "retrying request" "retrying request" "$JSON_LOG" "$JSON_WARN"; sleep 3600'],
        'env': [{'name': 'MESSAGE', 'value': message}, {'name': 'JSON_LOG', 'value': json.dumps({'level': 'INFO', 'message': 'Connected to node 2', 'logger_name': 'example.network.client', 'request_id': 'trace-details-42'})}, {'name': 'JSON_WARN', 'value': json.dumps({'level': 'WARN', 'message': 'Fetch session expired; retrying request', 'logger_name': 'example.consumer.Fetcher', 'thread_name': 'consumer-1', 'duration_ms': 42})}],
    }]},
}
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
        if select.select([master], [], [], min(.02, max(0, until - time.monotonic())))[0]:
            stream.feed(decoder.decode(os.read(master, 65536)))


def text():
    return '\n'.join(screen.display)


def send(keys):
    os.write(master, keys.encode())
    pump()


def expect(value, timeout=15):
    until = time.monotonic() + timeout
    while value not in text() and time.monotonic() < until:
        pump(.03)
    assert value in text(), f'Missing {value!r}:\n{text()}'


def resize(width, height):
    screen.resize(height, width)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', height, width, 0, 0))
    if process:
        process.send_signal(signal.SIGWINCH)
        pump(.3)


def click(column, row):
    send(f'\x1b[<0;{column + 1};{row + 1}M\x1b[<0;{column + 1};{row + 1}m')


def capture(label):
    directory = os.environ.get('KUBE_UI_CAPTURE_DIR')
    if directory:
        out = pathlib.Path(directory)
        out.mkdir(parents=True, exist_ok=True)
        cells = [[screen.buffer[y][x]._asdict() for x in range(screen.columns)] for y in range(screen.lines)]
        (out / f'{label}.json').write_text(json.dumps(cells))
        (out / f'{label}.txt').write_text(text())


try:
    with tempfile.TemporaryDirectory(prefix='kube-ux-') as temp:
        source = json.loads(subprocess.check_output(['kubectl', 'config', 'view', '--raw', '--flatten', '-o', 'json']))
        chosen = next(c for c in source['contexts'] if c['name'] == source['current-context'])
        alternate = copy.deepcopy(chosen)
        alternate['name'] = 'kind-kube-tui-alternate'
        source['contexts'].append(alternate)
        selected_file = pathlib.Path(temp) / 'multiple clusters.yaml'
        selected_file.write_text(json.dumps(source))
        original = selected_file.read_bytes()
        # Deliberately invalid environment source: the explicit file must win.
        env = dict(os.environ, TERM='xterm-256color', XDG_CONFIG_HOME=temp, KUBECONFIG=str(pathlib.Path(temp) / 'not-the-source'))
        env.pop('NO_COLOR', None)
        resize(150, 40)
        process = subprocess.Popen([binary, '--kubeconfig', str(selected_file), '--context', chosen['name'], '-n', 'demo'],
                                   stdin=slave, stdout=slave, stderr=slave, env=env)
        expect('Pod monitor', 30)
        assert 'RESTARTS' not in text(), 'Dashboard must clear underlying table'
        capture('fleet-radar')
        send('\x1bOR')
        expect('web-', 30)
        expect('WORKSPACE')
        capture('resources')
        send('s')
        expect('Sort resources')
        send('age')
        expect('Newest first')
        send('\r')
        expect('Age ↑')
        send('srestarts\x1b[B\r')
        expect('Restarts ↓')
        capture('resources-sorted')
        click(screen.display[0].index('ns:') + 2, 0)
        expect('Namespaces')
        expect('kube-system')
        capture('namespaces')
        send('kube-system\r')
        expect('coredns')
        assert 'ns: kube-system' in screen.display[0]
        send('\x0e')
        expect('Namespaces')
        send('demo\r')
        expect('web-')
        click(12, 0)
        expect('Clusters')
        expect('kind-kube-tui-alternate')
        capture('clusters')
        send('alternate\r')
        expect('coredns')
        assert 'kind-kube-tui-alternate' in screen.display[0]
        send('\x0e')
        expect('Namespaces')
        send('demo\r')
        expect('web-')
        send('/' + name + '\r')
        expect(name)
        send('l')
        expect('TAIL-VISIBLE', 60)
        expect('Wrap on')
        expect('2 more stack frames')
        expect('2 repeated entries')
        send('v')
        expect('hidden.fn')
        send('v')
        expect('Connected to node 2')
        expect('INF')
        expect('WRN')
        assert 'logger_name' not in text(), 'Message view must hide JSON metadata'
        capture('logs-wrapped')
        row = next(i for i, line in enumerate(screen.display) if 'Connected to node 2' in line)
        click(75, row)
        expect('logger_name')
        expect('trace-details-42')
        send('\r')
        send('o')
        expect('RAW')
        expect('logger_name')
        send('o')
        send('/trace-details-42\r')
        expect('1 matches')
        expect('trace-details-42')
        send('\x1b')
        send('\x1b[H')
        expect('BEGIN-LONG')
        expect('TAIL-VISIBLE')
        send('/error: timeout?\r')
        expect('1 matches')
        expect('context-after')
        assert 'Keyboard & mouse' not in text()
        capture('logs-search')
        assert any(cell.bg == 'ffc145' and cell.fg == '0f1117'
                   for row in screen.buffer.values() for cell in row.values()), 'Search must highlight the message'
        capture('logs-search')
        send('/\x15error:\r')
        expect('2 matches')
        send('n')
        expect('error: retry?')
        send('N')
        expect('error: timeout?')
        send('F')
        expect('show context')
        assert 'context-after' not in text()
        send('F\x1b')
        expect('Search logs')
        send('w')
        expect('Wrap off')
        send('wz')
        expect('Restore')
        resize(80, 24)
        send('\x1b[H')
        expect('BEGIN-LONG')
        send('\x1b[6~')
        expect('TAIL-VISIBLE')
        capture('logs-small')
        send('\x0e')
        expect('Namespaces')
        capture('namespaces-over-logs')
        send('\x1b')
        expect('Wrap on')
        send('\x0f')
        expect('Clusters')
        send('\x1b')
        resize(150, 40)
        send('z\t')
        # Tab moved focus to the sidebar; return to the active Pod list by click.
        pod_row = next(i for i, row in enumerate(screen.display) if '▤  Pods' in row)
        click(5, pod_row)
        expect('Enter inspect')
        assert 'Search logs' not in text(), 'Sidebar activation did not return to resources'
        for _ in range(4):
            send('\x1b')
            assert process.poll() is None, 'Esc at root unexpectedly quit'
        send('\x03')
        process.wait(timeout=5)
        assert process.returncode == 0
        assert termios.tcgetattr(slave) == before
        assert selected_file.read_bytes() == original, 'Kubeconfig was modified'
        print('PASS: explicit kubeconfig → cluster/namespace pickers → wrap → search/filter → resize → focus → terminal restoration')
finally:
    if process and process.poll() is None:
        process.kill()
        process.wait()
    subprocess.run(['kubectl', '-n', 'demo', 'delete', 'pod', name, '--wait=false'], check=False, stdout=subprocess.DEVNULL)
    os.close(master)
    os.close(slave)
