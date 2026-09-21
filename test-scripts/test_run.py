#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Arm Limited or its affiliates and Contributors. All rights reserved.
"""Generic run and thin manyclangs wrapper regressions."""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time

from test_remotes import BINARY, Fixture


class RunTests(Fixture):
    def setUp(self):
        super().setUp()
        pair = self.make_pair('build-aaaaaaaaaaaaaaa', b'first')
        pair = self.make_pair('build-bbbbbbbbbbbbbbb', b'second')
        self.publish({'month': pair})
        self.clone()

    def run_snapshot(self, term, *command, **kwargs):
        return self.command(self.client, 'run', '--verify', term, '--', *command, **kwargs)

    def test_arguments_io_environment_and_working_directory(self):
        args = ['a b', '', '$HOME; $(false)', '--verbose', '--data-dir', 'no such path']
        script = 'import os,sys,json; print(json.dumps([os.getcwd(),sys.argv[1:],sys.stdin.read(),os.environ["PATH"]]))'
        result = self.run_snapshot('aaaa', sys.executable, '-c', script, *args, input=b'input data')
        actual = json.loads(result.stdout)
        self.assertEqual(actual, [str(self.client.resolve()), args, 'input data', os.environ['PATH']])
        self.assertIn(b'Extracted', result.stderr)
        self.assertEqual(self.requests['/month.pack'], 1)
        result = self.run_snapshot('aaaa', 'sh', '-c', 'printf repeated; exit 27', ok=False)
        self.assertEqual(result.returncode, 27)
        self.assertEqual(result.stdout, b'repeated')
        self.assertEqual(self.requests['/month.pack'], 1)

    def test_switch_snapshot_and_qualified_reference(self):
        self.run_snapshot('aaaa', 'cat', 'value')
        result = self.run_snapshot('origin/month:build-bbbbbbbbbbbbbbb', 'cat', 'value')
        self.assertEqual(result.stdout, b'second')
        result = self.run_snapshot('aaaa', 'cat', 'value')
        self.assertEqual(result.stdout, b'first')

    def test_lookup_failures_do_not_execute_and_do_not_refresh(self):
        requests = self.requests.copy()
        for term in ['absent', 'build-', 'origin/month:missing', 'AAAA']:
            result = self.run_snapshot(term, 'sh', '-c', 'touch executed', ok=False)
            self.assertEqual(result.returncode, 125, result.stderr)
            self.assertFalse((self.client / 'executed').exists())
        ambiguous = self.run_snapshot('build-', 'true', ok=False)
        self.assertIn(b'origin/month:build-aaaaaaaaaaaaaaa', ambiguous.stderr)
        self.assertIn(b'origin/month:build-bbbbbbbbbbbbbbb', ambiguous.stderr)
        self.assertEqual(self.requests, requests)

    def test_duplicate_tags_are_ambiguous_even_with_identical_content(self):
        shutil.copyfile(self.cached('month.pack.idx'), self.client / 'elfshaker_data/packs/copy.pack.idx')
        result = self.run_snapshot('aaaa', 'true', ok=False)
        self.assertEqual(result.returncode, 125)
        self.assertIn(b'copy:build-aaaaaaaaaaaaaaa', result.stderr)
        self.run_snapshot('origin/month:build-aaaaaaaaaaaaaaa', 'true')

    def test_dirty_files_and_reset(self):
        self.run_snapshot('aaaa', 'true')
        value = self.client / 'value'
        value.write_bytes(b'edited')
        later = time.time() + 10
        os.utime(value, (later, later))
        result = self.run_snapshot('bbbb', 'sh', '-c', 'touch executed', ok=False)
        self.assertEqual(result.returncode, 125)
        self.assertFalse((self.client / 'executed').exists())
        self.assertEqual(value.read_bytes(), b'edited')
        self.command(self.client, 'run', '--reset', '--verify', 'bbbb', '--', 'true')
        self.assertEqual(value.read_bytes(), b'second')

    def test_child_status_launch_errors_and_signal(self):
        self.assertEqual(self.run_snapshot('aaaa', 'does-not-exist-elfshaker-test', ok=False).returncode, 125)
        for code in [0, 1, 125, 127, 255]:
            result = self.run_snapshot('aaaa', 'sh', '-c', f'exit {code}', ok=False)
            self.assertEqual(result.returncode, code)
        result = self.run_snapshot('aaaa', 'sh', '-c', 'kill -TERM $$', ok=False)
        self.assertEqual(result.returncode, -signal.SIGTERM)
        self.assertEqual(self.command(self.client, 'run', 'aaaa', ok=False).returncode, 2)

    def test_failed_extract_never_runs_command(self):
        self.resources['/month.pack'] = b'corrupt'
        result = self.run_snapshot('aaaa', 'sh', '-c', 'touch executed', ok=False)
        self.assertEqual(result.returncode, 125)
        self.assertFalse((self.client / 'executed').exists())

    def test_command_holds_lock_against_update_extract_and_run(self):
        script = 'from pathlib import Path; import time; Path("ready").touch()\nwhile not Path("release").exists(): time.sleep(.01)'
        holder = subprocess.Popen([BINARY, 'run', 'aaaa', '--', sys.executable, '-c', script],
                                  cwd=self.client, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        competitors = []
        try:
            deadline = time.monotonic() + 10
            while not (self.client / 'ready').exists():
                if holder.poll() is not None or time.monotonic() > deadline:
                    self.fail('command failed to signal readiness')
                time.sleep(.01)
            for args in [('update',), ('extract', '--verify', 'origin/month:build-bbbbbbbbbbbbbbb'),
                         ('run', 'bbbb', '--', 'true')]:
                proc = subprocess.Popen([BINARY, *args], cwd=self.client,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                competitors.append(proc)
            time.sleep(.2)
            self.assertTrue(all(p.poll() is None for p in competitors))
            self.assertEqual((self.client / 'value').read_bytes(), b'first')
            (self.client / 'release').touch()
            holder.communicate(timeout=10)
            self.assertEqual(holder.returncode, 0)
            for proc in competitors:
                _, err = proc.communicate(timeout=10)
                self.assertEqual(proc.returncode, 0, err)
        finally:
            for proc in [holder, *competitors]:
                if proc.poll() is None:
                    proc.kill()
                proc.communicate()

    def test_parent_signal_is_forwarded_and_lock_survives_child_cleanup(self):
        script = ('from pathlib import Path; import signal,sys,time\n'
                  'def stop(*args):\n'
                  ' Path("stopping").touch()\n'
                  ' while not Path("release").exists(): time.sleep(.01)\n'
                  ' sys.exit(42)\n'
                  'signal.signal(signal.SIGTERM, stop)\n'
                  'Path("ready").touch()\n'
                  'while True: time.sleep(.01)\n')
        holder = subprocess.Popen([BINARY, 'run', 'aaaa', '--', sys.executable, '-c', script],
                                  cwd=self.client, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        competitor = None
        try:
            def wait_file(name):
                deadline = time.monotonic() + 10
                while not (self.client / name).exists():
                    if holder.poll() is not None or time.monotonic() > deadline:
                        self.fail('child did not create ' + name)
                    time.sleep(.01)
            wait_file('ready')
            holder.send_signal(signal.SIGTERM)
            wait_file('stopping')
            competitor = subprocess.Popen([BINARY, 'update'], cwd=self.client,
                                          stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            time.sleep(.1)
            self.assertIsNone(competitor.poll())
            (self.client / 'release').touch()
            holder.communicate(timeout=10)
            self.assertEqual(holder.returncode, 42)
            competitor.communicate(timeout=10)
            self.assertEqual(competitor.returncode, 0)
        finally:
            (self.client / 'release').touch()
            for proc in [holder, competitor]:
                if proc is not None:
                    if proc.poll() is None:
                        proc.kill()
                    proc.communicate()

    def test_existing_read_only_pack_extraction(self):
        # Legacy extract supports read-only catalogs without updating HEAD.
        self.run_snapshot('aaaa', 'true')
        mutex = self.client / 'elfshaker_data/mutex'
        mutex.chmod(0o444)
        try:
            result = self.command(self.client, 'extract', '--reset', '--verify', 'origin/month:build-bbbbbbbbbbbbbbb')
            self.assertEqual(result.returncode, 0)
            self.assertEqual((self.client / 'value').read_bytes(), b'second')
        finally:
            mutex.chmod(0o644)


class WrapperTests(Fixture):
    def setUp(self):
        super().setUp()
        self.git = self.root / 'llvm'
        self.git.mkdir()
        def git(*args):
            return subprocess.check_output(['git', '-C', str(self.git), *args], stderr=subprocess.DEVNULL).decode().strip()
        git('init')
        git('-c', 'user.name=Test', '-c', 'user.email=test@example.invalid', 'commit', '--allow-empty', '-m', 'fixture')
        self.revision = git('rev-parse', 'HEAD')
        (self.producer / 'link.sh').write_text('''#!/usr/bin/env bash
set -eu
[ "${FAIL_LINK:-}" != 1 ] || exit 7
mkdir -p bin
cat > "bin/$1" <<'TOOL'
#!/usr/bin/env python3
import json, sys
print(json.dumps(sys.argv[1:]))
sys.exit(23)
TOOL
chmod +x "bin/$1"
''')
        pair = self.make_pair('build-' + self.revision[:15], b'fixture')
        self.publish({'month': pair})
        self.clone()
        self.wrapper_path = str(Path('contrib/manyclangs-run').resolve())
        self.tools = self.root / 'tools'
        self.tools.mkdir()
        (self.tools / 'elfshaker').symlink_to(BINARY)
        self.env = dict(os.environ, PATH=str(self.tools) + ':' + os.environ['PATH'])

    def wrapper(self, **env):
        return subprocess.run(['bash', self.wrapper_path, str(self.client), 'HEAD', 'bin/clang', 'a b', '$HOME', '--version'],
                              cwd=self.git, env=dict(self.env, **env), capture_output=True, timeout=20)

    def test_native_wrapper_arguments_and_failure_status(self):
        result = self.wrapper()
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertEqual(json.loads(result.stdout), ['a b', '$HOME', '--version'])
        result = self.wrapper(FAIL_LINK='1')
        self.assertEqual(result.returncode, 125)

    def test_docker_dispatch_preserves_arguments(self):
        docker = self.tools / 'docker'
        docker.write_text('#!/usr/bin/env python3\nimport json,os,sys\nopen(os.environ["DOCKER_ARGS"],"a").write(json.dumps(sys.argv[1:])+"\\n")\nsys.exit(0 if sys.argv[1]=="image" else 23)\n')
        docker.chmod(0o755)
        record = self.root / 'docker.json'
        result = self.wrapper(USE_DOCKER_QEMU='aarch64', DOCKER_ARGS=str(record))
        self.assertEqual(result.returncode, 23, result.stderr)
        calls = [json.loads(line) for line in record.read_text().splitlines()]
        self.assertEqual(calls[0], ['image', 'inspect', 'manyclangs-qemu-aarch64'])
        self.assertIn(f'type=bind,source={self.client},target=/elfshaker', calls[1])
        self.assertEqual(calls[1][-4:], ['clang', 'a b', '$HOME', '--version'])
