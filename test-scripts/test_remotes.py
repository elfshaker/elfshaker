#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Arm Limited or its affiliates and Contributors. All rights reserved.
"""Real-pack remote regressions. Usage: TEST_ELFSHAKER=... python3 -m unittest discover -s test-scripts -p test_remotes.py -v"""
import collections
import hashlib
import http.server
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest

BINARY = str(Path(os.environ.get('TEST_ELFSHAKER', 'target/debug/elfshaker')).resolve())


def sha(data):
    return hashlib.sha1(data).hexdigest()


class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.producer = self.root / 'producer'
        self.producer.mkdir()
        self.client = self.root / 'client'
        self.resources = {}
        self.requests = collections.Counter()
        self.headers = []
        self.no_length = set()
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                fixture.requests[self.path] += 1
                fixture.headers.append(dict(self.headers))
                if self.path not in fixture.resources:
                    self.send_error(404)
                    return
                # A conditional request would incorrectly hide our changed data.
                if self.headers.get('If-Modified-Since'):
                    self.send_response(304)
                    self.end_headers()
                    return
                data = fixture.resources[self.path]
                self.send_response(200)
                if self.path not in fixture.no_length:
                    self.send_header('Content-Length', str(len(data)))
                self.send_header('Last-Modified', 'Thu, 01 Jan 1970 00:00:00 GMT')
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *_):
                pass

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.stop_server)
        self.url = f'http://127.0.0.1:{self.server.server_port}'

    def stop_server(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    def command(self, cwd, *args, ok=True, **kwargs):
        env = os.environ.copy()
        env.pop('ELFSHAKER_DATA', None)
        result = subprocess.run([BINARY, *args], cwd=cwd, env=env,
                                capture_output=True, timeout=20, **kwargs)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr.decode())
        return result

    def make_pair(self, tag, content):
        (self.producer / 'value').write_bytes(content)
        self.command(self.producer, 'store', tag)
        # Repack all captured snapshots; use a fresh target for each generation.
        name = 'generation-' + tag
        self.command(self.producer, 'pack', '--compression-level', '1', '-T', '1',
                     '--frames', '1', name, *[
                         'loose/' + p.name[:-9]
                         for p in sorted((self.producer / 'elfshaker_data/packs/loose').glob('*.pack.idx'))])
        base = self.producer / 'elfshaker_data/packs'
        return ((base / (name + '.pack')).read_bytes(),
                (base / (name + '.pack.idx')).read_bytes())

    def publish(self, pairs, manifest='/index.esi'):
        lines = ['meta v1', f'url {self.url}{manifest}']
        for name, (pack, index) in pairs.items():
            self.resources[f'/{name}.pack'] = pack
            self.resources[f'/{name}.pack.idx'] = index
            lines.append(f'{sha(index)} {sha(pack)} {name}.pack')
        self.resources[manifest] = ('\n'.join(lines) + '\n').encode()

    def clone(self):
        self.command(self.root, 'clone', self.url + '/index.esi', 'client')

    def extract(self, tag, **kwargs):
        return self.command(self.client, 'extract', '--verify', tag, **kwargs)

    def cached(self, name='month.pack'):
        return self.client / 'elfshaker_data/packs/origin' / name


class RemoteTests(Fixture):
    def test_refresh_preserves_unchanged_and_lazily_replaces_changed(self):
        old = self.make_pair('old', b'old')
        self.publish({'month': old, 'stable': old})
        self.clone()
        self.extract('origin/month:old')
        self.extract('origin/stable:old')
        original = self.cached().stat().st_mtime_ns
        stable = self.cached('stable.pack').stat().st_mtime_ns
        self.command(self.client, 'update')
        self.assertEqual(self.cached().stat().st_mtime_ns, original)
        self.assertEqual(self.requests['/month.pack'], 1)
        new = self.make_pair('new', b'new content')
        self.publish({'month': new, 'stable': old})
        self.command(self.client, 'update')
        self.assertFalse(self.cached().exists())
        self.assertEqual(self.cached('stable.pack').stat().st_mtime_ns, stable)
        self.assertEqual(self.requests['/month.pack'], 1)
        self.extract('origin/month:new')
        self.assertEqual((self.client / 'value').read_bytes(), b'new content')
        self.extract('origin/month:old')
        self.assertEqual((self.client / 'value').read_bytes(), b'old')
        self.assertEqual(self.requests['/month.pack'], 2)
        self.assertEqual(self.requests['/stable.pack'], 1)
        self.assertEqual(self.requests['/stable.pack.idx'], 1)
        self.assertFalse(any('If-Modified-Since' in h for h in self.headers))

    def test_bad_index_preserves_installed_catalog(self):
        old = self.make_pair('old', b'old')
        self.publish({'month': old})
        self.clone()
        self.extract('origin/month:old')
        manifest = self.client / 'elfshaker_data/remotes/origin.esi'
        before = manifest.read_bytes()
        new = self.make_pair('new', b'new')
        self.publish({'month': new})
        self.resources['/month.pack.idx'] = old[1]  # Valid index, wrong hash.
        self.assertNotEqual(self.command(self.client, 'update', ok=False).returncode, 0)
        self.assertEqual(manifest.read_bytes(), before)
        self.assertEqual(self.cached('month.pack.idx').read_bytes(), old[1])
        self.assertEqual(self.cached().read_bytes(), old[0])
        # Correct hash but invalid serialization must also fail before installing.
        self.publish({'month': (new[0], b'invalid index')})
        self.assertNotEqual(self.command(self.client, 'update', ok=False).returncode, 0)
        self.assertEqual(manifest.read_bytes(), before)
        self.publish({'month': new})
        self.command(self.client, 'update')
        self.extract('origin/month:new')

    def test_payload_only_change_and_corrupt_download(self):
        pair = self.make_pair('old', b'old')
        self.publish({'month': pair})
        self.clone()
        self.extract('origin/month:old')
        # Payload corruption in an older client's cache is found even when the
        # manifest and index are unchanged.
        self.cached().write_bytes(b'corrupt')
        self.command(self.client, 'update')
        self.assertFalse(self.cached().exists())
        self.resources['/month.pack'] = b'bad download'
        result = self.command(self.client, 'extract', '--reset', '--verify', 'origin/month:old', ok=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.cached().exists())
        self.assertEqual(list(self.cached().parent.iterdir()), [self.cached('month.pack.idx')])
        self.resources['/month.pack'] = pair[0]
        self.command(self.client, 'extract', '--reset', '--verify', 'origin/month:old')
        # A changed payload hash with identical index must invalidate as well.
        self.publish({'month': (pair[0] + b'new generation', pair[1])})
        self.command(self.client, 'update')
        self.assertFalse(self.cached().exists())

    def test_interrupted_index_install_is_rejected_and_update_repairs(self):
        old = self.make_pair('old', b'old')
        self.publish({'month': old})
        self.clone()
        self.extract('origin/month:old')
        new = self.make_pair('new', b'new')
        self.publish({'month': new})
        # Simulate termination after payload invalidation/index rename, before
        # manifest rename. Even the already-extracted HEAD must be checked.
        self.cached().unlink()
        self.cached('month.pack.idx').write_bytes(new[1])
        result = self.extract('origin/month:old', ok=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b'elfshaker update', result.stderr)
        self.command(self.client, 'update')
        self.command(self.client, 'update')
        self.extract('origin/month:new')
        self.assertEqual((self.client / 'value').read_bytes(), b'new')

    def test_payload_without_content_length(self):
        self.publish({'month': self.make_pair('old', b'old')})
        self.no_length.add('/month.pack')
        self.clone()
        self.extract('origin/month:old')
        self.assertEqual((self.client / 'value').read_bytes(), b'old')

    def test_download_uses_owning_remote(self):
        first = self.make_pair('first', b'first')
        self.publish({'month': first})
        self.clone()
        second = self.make_pair('second', b'second')
        # Another remote has the same basename but a different URL and payload.
        self.resources['/other/month.pack'] = second[0]
        self.resources['/other/month.pack.idx'] = second[1]
        manifest = (f'meta v1\nurl {self.url}/other/index.esi\n'
                    f'{sha(second[1])} {sha(second[0])} month.pack\n').encode()
        self.resources['/other/index.esi'] = manifest
        (self.client / 'elfshaker_data/remotes/other.esi').write_bytes(manifest)
        self.command(self.client, 'update')
        self.extract('other/month:second')
        self.assertEqual((self.client / 'value').read_bytes(), b'second')
        self.assertEqual(self.requests['/other/month.pack'], 1)
        self.assertEqual(self.requests['/month.pack'], 0)


if __name__ == '__main__':
    unittest.main()
