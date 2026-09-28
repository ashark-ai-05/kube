#!/usr/bin/env python3
"""Check a packaged CPU runtime before compiling; report startup logs on failure."""
import argparse
import os
import pathlib
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request


def check(bundle):
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    started = time.monotonic()
    with tempfile.TemporaryFile() as log:
        worker = subprocess.Popen([
            str(bundle / 'runtime/llama-server'), '-m', str(bundle / 'model.gguf'),
            '--host', '127.0.0.1', '--port', str(port), '-c', '4096', '-np', '1',
            '-ngl', '0', '--device', 'none', '--no-op-offload', '--no-kv-offload',
            '-t', '2', '--no-webui', '--no-warmup', '--reasoning', 'off',
            '--chat-template-kwargs', '{"enable_thinking":false}', '-b', '256', '-ub', '128',
        ], env={'PATH': '/usr/bin:/bin'}, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        ready = False
        try:
            # Ignore proxy environment settings for this loopback-only check.
            client = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            while time.monotonic() - started < 60:
                if worker.poll() is not None:
                    raise RuntimeError(f'CPU runtime exited with {worker.returncode}')
                try:
                    with client.open(f'http://127.0.0.1:{port}/health', timeout=1) as response:
                        ready = response.status == 200
                    if ready:
                        print(f'CPU runtime ready in {time.monotonic() - started:.2f}s', flush=True)
                        return
                except (urllib.error.URLError, TimeoutError):
                    pass
                time.sleep(0.1)
            raise RuntimeError('CPU runtime did not become ready within 60 seconds')
        finally:
            worker.terminate()
            try:
                worker.wait(timeout=3)
            except subprocess.TimeoutExpired:
                worker.kill()
                worker.wait()
            if not ready:
                log.seek(0, os.SEEK_END)
                log.seek(max(0, log.tell() - 16_384))
                print(log.read().decode(errors='replace'), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('bundle', type=pathlib.Path)
    check(parser.parse_args().bundle.resolve())
