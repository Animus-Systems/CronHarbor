"""Prove a scheduled StartUnit frontier without claiming a coalesced foreign job."""
import json
import os
import sys
import time

import dbus
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

NAME = 'org.freedesktop.systemd1'
PATH = '/org/freedesktop/systemd1'
UNIT = NAME + '.Unit'
PROPS = 'org.freedesktop.DBus.Properties'
PARENTS = ('RequiredBy', 'WantedBy', 'BoundBy', 'UpheldBy', 'ConsistsOf', 'OnFailureOf', 'OnSuccessOf')
READ_ONLY = {'Get', 'GetAll', 'Introspect', 'Ping', 'LoadUnit', 'RefUnit', 'UnrefUnit', 'Subscribe', 'Unsubscribe', 'GetUnit'}


def main():
    requested = False
    control = monitor = None
    result = {'kind': 'rejected'}
    try:
        scope, unit = sys.argv[1:]
        address = 'unix:path=/run/user/%d/bus' % os.geteuid() if scope == 'user' else dbus.bus.BUS_SYSTEM
        loop = DBusGMainLoop()
        control = dbus.bus.BusConnection(address, mainloop=loop)
        monitor = dbus.bus.BusConnection(address, mainloop=loop)
        own = control.get_unique_name()
        owner = control.get_name_owner(NAME)
        manager = dbus.Interface(control.get_object(NAME, PATH, introspect=False), NAME + '.Manager')
        manager.Subscribe(timeout=2)
        context = GLib.MainContext.default()
        calls = []
        jobs = []
        pings = []
        overflow = False

        def receive(_connection, message):
            nonlocal overflow
            # No payloads from service properties or unrelated calls enter the receipt.
            if message.get_type() == dbus.lowlevel.MESSAGE_TYPE_METHOD_CALL:
                if len(calls) >= 256:
                    overflow = True
                    return
                if message.get_sender() == own and message.get_member() == 'GetUnit':
                    pings.append(len(calls))
                calls.append((message.get_sender(), message.get_member()))
            elif message.get_sender() == owner:
                if len(jobs) >= 256:
                    overflow = True
                    return
                jobs.append((message.get_member(), message.get_args_list()))

        monitor.add_message_filter(receive)
        monitoring = dbus.Interface(monitor.get_object('org.freedesktop.DBus', '/org/freedesktop/DBus', introspect=False), 'org.freedesktop.DBus.Monitoring')
        monitoring.BecomeMonitor([
            "type='method_call',path_namespace='/org/freedesktop/systemd1'",
            "type='signal',sender='org.freedesktop.systemd1',interface='org.freedesktop.systemd1.Manager',member='JobNew'",
            "type='signal',sender='org.freedesktop.systemd1',interface='org.freedesktop.systemd1.Manager',member='JobRemoved'",
        ], dbus.UInt32(0), timeout=2)

        def drain():
            limit = time.monotonic() + 1
            while context.pending():
                context.iteration(False)
                if time.monotonic() > limit:
                    raise RuntimeError('bus backlog')

        def barrier():
            count = len(pings)
            manager.GetUnit(unit, timeout=2)
            limit = time.monotonic() + 2
            while len(pings) == count:
                drain()
                if time.monotonic() > limit:
                    raise RuntimeError('missing monitor barrier')
                time.sleep(.005)

        def props(path):
            return control.call_blocking(NAME, path, PROPS, 'GetAll', 's', (UNIT,), timeout=2)

        path = manager.LoadUnit(unit, timeout=2)
        manager.RefUnit(unit, timeout=2)
        barrier()
        before = props(path)
        if before['ActiveState'] not in ('inactive', 'failed') or int(before['Job'][0]) != 0:
            raise RuntimeError('busy unit')
        if any(before[key] for key in PARENTS):
            raise RuntimeError('automatic dependency starter')
        restart = control.call_blocking(NAME, path, PROPS, 'Get', 'ss', (NAME + '.Service', 'Restart'), timeout=2)
        if restart != 'no':
            raise RuntimeError('scheduled unit has restart policy')
        triggers = {}
        for trigger in before['TriggeredBy']:
            if len(triggers) >= 16:
                raise RuntimeError('too many triggers')
            value = props(manager.LoadUnit(trigger, timeout=2))
            if value['ActiveState'] != 'inactive' or int(value['Job'][0]) or value['UnitFileState'] not in ('disabled', 'static', 'masked'):
                raise RuntimeError('automatic trigger still enabled')
            triggers[str(trigger)] = value['ActiveState']
        previous = bytes(before['InvocationID']).hex()
        requested = True
        job = manager.StartUnit(unit, 'fail', timeout=3)
        job_id = int(str(job).rsplit('/', 1)[1])
        deadline = time.monotonic() + 3
        while True:
            after = props(path)
            invocation = bytes(after['InvocationID']).hex()
            drain()
            new_job = any(member == 'JobNew' and int(args[0]) == job_id and args[2] == unit for member, args in jobs)
            completed = any(member == 'JobRemoved' and int(args[0]) == job_id and args[2] == unit for member, args in jobs)
            if new_job and invocation != previous and len(invocation) == 32 and int(invocation, 16) and (int(after['Job'][0]) == job_id or completed):
                break
            if time.monotonic() >= deadline:
                raise RuntimeError('missing invocation frontier')
            time.sleep(.01)
        barrier()
        if overflow:
            raise RuntimeError('control window overflow')
        if any(sender != own and member not in READ_ONLY for sender, member in calls[pings[0]:pings[-1]]):
            raise RuntimeError('competing systemd mutation')
        final = props(path)
        if bytes(final['InvocationID']).hex() != invocation:
            raise RuntimeError('invocation changed after frontier')
        if any(final[key] for key in PARENTS) or set(final['TriggeredBy']) != set(before['TriggeredBy']) or final['Names'] != before['Names']:
            raise RuntimeError('activation graph changed')
        for trigger in triggers:
            value = props(manager.LoadUnit(trigger, timeout=2))
            if value['ActiveState'] != 'inactive' or int(value['Job'][0]) or value['UnitFileState'] not in ('disabled', 'static', 'masked'):
                raise RuntimeError('trigger changed')
        result = {'kind': 'started', 'invocationId': invocation, 'jobId': job_id}
    except Exception:
        result = {'kind': 'uncertain' if requested else 'rejected'}
    finally:
        if monitor is not None:
            monitor.close()
        if control is not None:
            control.close()
    print(json.dumps(result, separators=(',', ':')), flush=True)


if __name__ == '__main__':
    main()
