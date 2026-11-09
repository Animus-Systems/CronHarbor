#!/usr/bin/env python3
"""Real user-bus frontier proof; creates and removes only disposable owned units."""
import json
import subprocess
import tempfile
import uuid
from pathlib import Path

import dbus


def main():
    source = Path(__file__).resolve().parents[1] / 'src/external/systemd_start.py'
    name = 'cronharbor-proof-frontier-' + uuid.uuid4().hex[:12] + '.service'
    path = Path.home() / '.config/systemd/user' / name
    with tempfile.TemporaryDirectory(prefix='cronharbor-frontier-') as temporary:
        root = Path(temporary)
        effects = root / 'effects'
        path.write_text('[Service]\nType=oneshot\nExecStart=/usr/bin/touch ' + str(effects) + '\nExecStart=/usr/bin/sleep 1\n')
        digest = path.read_bytes()

        def ctl(*args):
            return subprocess.run(['systemctl', '--user', *args], capture_output=True, text=True, timeout=10)

        try:
            assert ctl('daemon-reload').returncode == 0
            bus = dbus.SessionBus()
            manager = dbus.Interface(bus.get_object('org.freedesktop.systemd1', '/org/freedesktop/systemd1'), 'org.freedesktop.systemd1.Manager')
            manager.LoadUnit(name)
            manager.RefUnit(name)
            normal = subprocess.run(['/usr/bin/python3', str(source), 'user', name], capture_output=True, text=True, timeout=14)
            receipt = json.loads(normal.stdout)
            assert receipt['kind'] == 'started' and receipt['jobId'] > 0
            assert effects.exists()
            assert ctl('stop', name).returncode == 0
            effects.unlink()
            wrapper = root / 'race.py'
            wrapper.write_text('''import dbus.proxies, subprocess, sys, runpy
original = dbus.proxies.ProxyObject.get_dbus_method
def method(self, member, *args, **kwargs):
    call = original(self, member, *args, **kwargs)
    if member != 'StartUnit':
        return call
    def raced(*values, **options):
        subprocess.run(['systemctl', '--user', 'start', '--no-block', sys.argv[2]], check=True)
        return call(*values, **options)
    return raced
dbus.proxies.ProxyObject.get_dbus_method = method
source = sys.argv.pop()
runpy.run_path(source, run_name='__main__')
''')
            raced = subprocess.run(['/usr/bin/python3', str(wrapper), 'user', name, str(source)], capture_output=True, text=True, timeout=14)
            assert json.loads(raced.stdout) == {'kind': 'uncertain'}
            assert effects.exists()
            assert ctl('show', name, '--property=ActiveState', '--value').stdout.strip() == 'activating'
            print(json.dumps({'check': 'scheduled-start-frontier', 'status': 'pass', 'realForeignStart': 'uncertain', 'foreignInvocationNotStopped': True}))
        finally:
            assert path.read_bytes() == digest
            ctl('stop', name)
            path.unlink()
            ctl('daemon-reload')


if __name__ == '__main__':
    main()
