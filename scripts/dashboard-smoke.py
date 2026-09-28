#!/usr/bin/env python3
"""Real terminal flat pod navigation, live metrics and scope isolation on the owned kind fixture."""
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
incident = 'kube-incident-' + uuid.uuid4().hex[:6]
fixture = {'apiVersion': 'v1', 'kind': 'Pod', 'metadata': {'name': name, 'namespace': 'demo', 'labels': {'app': name}}, 'spec': {'containers': [{'name': 'worker', 'image': 'nginx:alpine', 'resources': {'requests': {'cpu': '10m', 'memory': '16Mi'}, 'limits': {'cpu': '200m', 'memory': '128Mi'}}, 'readinessProbe': {'httpGet': {'path': '/', 'port': 80}}, 'livenessProbe': {'httpGet': {'path': '/', 'port': 80}}}]}}
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
        resize(150, 40)
        env = dict(os.environ, TERM='xterm-256color', XDG_CONFIG_HOME=temp)
        env.pop('NO_COLOR', None)
        process = subprocess.Popen([binary, '--kubeconfig', config, '-n', 'demo'], stdin=slave, stdout=slave, stderr=slave, env=env)
        expect('POD MONITOR')
        expect(name)
        expect('web-')
        expect('mCPU', 90)
        expect('limit 128Mi', 90)
        assert 'POD GROUPS' not in text()
        pump(10.2)
        capture('pod-monitor')
        send('d')
        expect('live set / ready set')
        expect('nginx:alpine')
        capture('pod-containers')
        send('d')
        send('D')
        assert 'request 10m' not in text()
        send('D')
        expect('request 10m')
        # Sorting must preserve the same pod for inspection, even after the row moves.
        send('s')
        expect('Sort pods')
        send('Memory')
        send('\x1b[B\r')
        expect('Sorted: Memory')
        send('\r')
        expect('Overview')
        expect(name)
        send('\x1b')
        expect('POD MONITOR')
        for _ in range(4):
            send('\x1b')
            assert process.poll() is None
        send('2')
        expect('No pods match')
        send('0')
        expect(name)
        send('/' + name + '\r')
        expect(name)
        assert 'web-' not in text(), text()
        send('l')
        expect('Logs')
        expect(name)
        send('\x1b')
        expect('POD MONITOR')
        send('0')
        expect('web-')
        # Click the actual pod row, then inspect that selection.
        row = next(i for i,line in enumerate(screen.display) if 'web-' in line and 'Running' in line)
        send(f'\x1b[<0;40;{row+1}M\x1b[<0;40;{row+1}m')
        send('\r')
        expect('Overview')
        expect('Pod · demo/web-')
        send('\x1b')
        resize(80,24)
        expect('POD MONITOR')
        expect('web-')
        capture('pod-small')
        resize(150,40)
        send('\x0e')
        expect('Namespaces')
        send('kube-system\r')
        expect('coredns')
        expect('mCPU',90)
        assert name not in text(),text()
        capture('pod-system')
        send('\x0e')
        expect('Namespaces')
        send('demo\r')
        expect(name)
        # Combined live metric predicates and explicit syntax errors.
        send(':filter ready=true cpu>=0m memory<128Mi label:app=' + name + '\r')
        expect('1/1 ready',90)
        expect(name)
        assert 'web-' not in text(), text()
        send(':filter cpu>\r')
        expect('Filter error:')
        send(':filter label:app=' + name + '\r')
        expect(name)
        # Column preferences and saved query/sort survive an application restart.
        send('C')
        expect('Pod columns')
        send('CPU\r')
        expect('hidden · Enter shows')
        send('\x1b')
        send('s')
        send('Restarts\x1b[B\r')
        expect('Sorted: Restarts')
        expect('Restarts ↓')
        send('S')
        send('Fixture view\r')
        expect('Saved view: Fixture view')
        capture('pod-saved-view')
        saved = json.loads((pathlib.Path(temp) / 'kube/views.json').read_text())
        assert saved['views'][0]['settings']['query'] == 'label:app=' + name
        assert saved['views'][0]['settings']['descending']
        assert 'cpu' not in saved['views'][0]['settings']['columns']
        send('\x1bORq')
        process.wait(timeout=5)
        pump()
        assert termios.tcgetattr(slave) == before
        screen.reset()
        process = subprocess.Popen([binary, '--kubeconfig', config, '-n', 'demo'], stdin=slave, stdout=slave, stderr=slave, env=env)
        expect('POD MONITOR')
        expect('web-')
        send('v')
        expect('Saved pod views')
        send('Fixture view\r')
        expect('View: Fixture view')
        expect(name)
        assert 'web-' not in text(), text()
        send('\x0e')
        send('kube-system\r')
        send('0')
        expect('coredns')
        send('v')
        send('Fixture view\r')
        expect('No pods match')
        assert 'Standalone pod' not in text() and 'kube-system' in text(), text()
        send('\x0e')
        send('demo\r')
        expect(name)
        # A real failing container provides an exit code and retained previous logs.
        crash = {'apiVersion':'v1','kind':'Pod','metadata':{'name':incident,'namespace':'demo','labels':{'app':incident}},'spec':{'containers':[{'name':'failing','image':'nginx:alpine','command':['sh','-c','echo KUBE_INCIDENT_EVIDENCE; sleep 3; exit 23'],'readinessProbe':{'exec':{'command':['false']},'periodSeconds':1}}]}}
        subprocess.run(['kubectl','create','-f','-'],input=json.dumps(crash),text=True,check=True,stdout=subprocess.DEVNULL)
        deadline=time.monotonic()+120
        while time.monotonic()<deadline:
            pod=json.loads(subprocess.check_output(['kubectl','-n','demo','get','pod',incident,'-o','json']))
            statuses=pod.get('status',{}).get('containerStatuses',[])
            if statuses and statuses[0].get('restartCount',0)>0:
                break
            pump(.5)
        else:
            raise AssertionError('Fixture did not produce a previous container instance')
        send(':filter ready=false restarts>0 label:app=' + incident + '\r')
        expect(incident)
        send('t')
        expect('TROUBLESHOOT')
        expect('Exit code: 23')
        expect('previous instance')
        expect('Recorded:')
        capture('pod-troubleshooting')
        send('\r')
        expect('Logs')
        expect('KUBE_INCIDENT_EVIDENCE',30)
        expect('Previous logs loaded:')
        send('\x1b')
        expect('TROUBLESHOOT')
        resize(80,24)
        expect('TROUBLESHOOT')
        send('\x1b[6~')
        capture('pod-troubleshooting-small')
        send('\x1b')
        expect('POD MONITOR')
        resize(150,40)
        send(':delete-view Fixture view\r')
        expect('Deleted saved view: Fixture view')
        assert json.loads((pathlib.Path(temp)/'kube/views.json').read_text())['views'] == []
        send('\x1bOR')
        expect('web-')
        send('q')
        process.wait(timeout=5)
        pump()
        assert process.returncode == 0
        assert termios.tcgetattr(slave) == before
        print('Pod monitor: live metrics, combined queries, invalid filters, persistent views/columns, sorting, UID-scoped evidence, previous-container logs, mouse, Esc, narrow layout and scope isolation passed.')
finally:
    if process and process.poll() is None:
        process.kill()
        process.wait()
    os.close(master)
    os.close(slave)
    subprocess.run(['kubectl', '-n', 'demo', 'delete', 'pod', name, '--wait=false'], stdout=subprocess.DEVNULL, check=False)
    subprocess.run(['kubectl', '-n', 'demo', 'delete', 'pod', incident, '--wait=false', '--ignore-not-found'], stdout=subprocess.DEVNULL, check=False)
