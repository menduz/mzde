"""Exercise real D-Bus messages and fd ownership without touching host logind."""
import asyncio
import os
import signal
import sys
from dbus_next import Message, MessageType, Variant
from dbus_next.aio import MessageBus

NAME = 'org.freedesktop.impl.portal.desktop.kwm'
PATH = '/org/freedesktop/portal/desktop'
IFACE = 'org.freedesktop.impl.portal.Inhibit'
FRONTEND = 'org.freedesktop.portal.Desktop'
PREFIX = PATH + '/request/test/'

async def connect():
    return await MessageBus(negotiate_unix_fd=True).connect()

async def call(bus, member, signature='', body=None, path=PATH, interface=IFACE):
    return await asyncio.wait_for(bus.call(Message(destination=NAME, path=path,
        interface=interface, member=member, signature=signature, body=body or [])), 5)

def success(reply):
    assert reply.message_type == MessageType.METHOD_RETURN, reply

def failure(reply, suffix):
    assert reply.message_type == MessageType.ERROR, reply
    assert reply.error_name.endswith(suffix), reply.error_name

async def until(predicate):
    for _ in range(200):
        if predicate():
            return
        await asyncio.sleep(.01)
    raise AssertionError('condition timed out')

class Logind:
    def __init__(self, bus):
        self.bus = bus
        self.calls = []
        self.readers = []
        self.closed = set()
        self.delay = False
        self.deny = False
        self.gate = asyncio.Event()
        self.tasks = set()
        bus.add_message_handler(self.handle)

    def handle(self, message):
        if (message.message_type != MessageType.METHOD_CALL or
            message.interface != 'org.freedesktop.login1.Manager' or
            message.member != 'Inhibit'):
            return
        self.calls.append(message.body)
        task = asyncio.create_task(self.reply(message))
        self.tasks.add(task)
        task.add_done_callback(self.tasks.discard)
        return True

    async def reply(self, message):
        if self.deny:
            await self.bus.send(Message.new_error(message,
                'org.freedesktop.DBus.Error.AccessDenied', 'test denial'))
            return
        if self.delay:
            await self.gate.wait()
        read_fd, write_fd = os.pipe2(os.O_NONBLOCK | os.O_CLOEXEC)
        index = len(self.readers)
        self.readers.append(read_fd)
        def readable():
            if os.read(read_fd, 1) == b'':
                self.closed.add(index)
                asyncio.get_running_loop().remove_reader(read_fd)
                os.close(read_fd)
        asyncio.get_running_loop().add_reader(read_fd, readable)
        await self.bus.send(Message.new_method_return(message, 'h', [0], [write_fd]))
        os.close(write_fd)

async def main():
    mock_bus = await connect()
    await mock_bus.request_name('org.freedesktop.login1')
    mock = Logind(mock_bus)
    front = await connect()
    await front.request_name(FRONTEND)
    stranger = await connect()
    env = dict(os.environ, DBUS_SYSTEM_BUS_ADDRESS=os.environ['DBUS_SESSION_BUS_ADDRESS'])
    proc = await asyncio.create_subprocess_exec(sys.argv[1], env=env)
    try:
        for _ in range(200):
            reply = await mock_bus.call(Message(destination='org.freedesktop.DBus',
                path='/org/freedesktop/DBus', interface='org.freedesktop.DBus',
                member='NameHasOwner', signature='s', body=[NAME]))
            if reply.body == [True]:
                break
            assert proc.returncode is None
            await asyncio.sleep(.01)
        else:
            raise AssertionError('backend did not start')

        async def inhibit(token, flags, options=None, bus=None):
            return await call(bus or front, 'Inhibit', 'ossua{sv}',
                [PREFIX + token, 'org.example.Player', '', flags,
                 {'reason': Variant('s', 'Playing media')} if options is None else options])
        async def close(token, bus=None):
            return await call(bus or front, 'Close', path=PREFIX + token,
                              interface='org.freedesktop.impl.portal.Request')

        reply = await call(front, 'Introspect', interface='org.freedesktop.DBus.Introspectable')
        success(reply)
        assert 'org.freedesktop.impl.portal.Inhibit' in reply.body[0]
        failure(await inhibit('outsider', 4, bus=stranger), 'AccessDenied')
        assert not mock.calls

        for token, flags, what in [('sleep', 4, 'sleep'), ('idle', 8, 'idle'), ('both', 12, 'sleep:idle')]:
            success(await inhibit(token, flags))
            assert mock.calls[-1] == [what, 'org.example.Player', 'Playing media', 'block']
            index = len(mock.readers) - 1
            assert index not in mock.closed
            failure(await close(token, stranger), 'AccessDenied')
            failure(await inhibit(token, flags), 'InvalidArgs')
            success(await close(token))
            await until(lambda: index in mock.closed)

        success(await inhibit('first', 4))
        first = len(mock.readers) - 1
        success(await inhibit('second', 8))
        second = len(mock.readers) - 1
        success(await close('first'))
        await until(lambda: first in mock.closed)
        assert second not in mock.closed
        success(await close('second'))
        await until(lambda: second in mock.closed)

        count = len(mock.calls)
        for flags in [1, 2, 3, 5, 15]:
            failure(await inhibit('unsupported', flags), 'NotSupported')
        for flags in [0, 16, 0xffffffff]:
            failure(await inhibit('invalid', flags), 'InvalidArgs')
        failure(await inhibit('badreason', 4, {'reason': Variant('u', 5)}), 'InvalidArgs')
        failure(await call(front, 'Inhibit', 's', ['bad']), 'InvalidArgs')
        assert len(mock.calls) == count
        reply = await call(front, 'CreateMonitor', 'ooss',
            [PREFIX + 'monitor', PATH + '/session/test/monitor', 'org.example.Player', ''])
        success(reply)
        assert reply.body == [2]
        failure(await call(front, 'QueryEndResponse', 'o', [PATH + '/session/test/monitor']), 'NotSupported')

        success(await inhibit('default', 8, {'future-option': Variant('u', 42)}))
        assert mock.calls[-1][2] == 'Application requested inhibition'
        success(await close('default'))
        mock.deny = True
        failure(await inhibit('denied', 4), 'AccessDenied')
        mock.deny = False
        # The failed request must not reserve its handle.
        success(await inhibit('denied', 4))
        success(await close('denied'))

        # Cancellation must work while the system bus reply is outstanding.
        mock.delay = True
        count = len(mock.calls)
        pending = asyncio.create_task(inhibit('pending', 4))
        await until(lambda: len(mock.calls) == count + 1)
        success(await close('pending'))
        failure(await pending, 'Cancelled')
        index = len(mock.readers)
        mock.gate.set()
        await until(lambda: index in mock.closed)
        mock.delay = False

        success(await inhibit('disconnect', 4))
        index = len(mock.readers) - 1
        await front.request_name('org.example.Unrelated')
        await front.release_name('org.example.Unrelated')
        # A round trip ensures the backend has processed the ownership signal.
        success(await call(front, 'Introspect', interface='org.freedesktop.DBus.Introspectable'))
        assert index not in mock.closed
        front.disconnect()
        await front.wait_for_disconnect()
        await until(lambda: index in mock.closed)
        front = await connect()
        await front.request_name(FRONTEND)

        # Losing the frontend name also releases inhibitors even if the socket lives.
        success(await inhibit('name_loss', 4))
        index = len(mock.readers) - 1
        await front.release_name(FRONTEND)
        await until(lambda: index in mock.closed)
        await front.request_name(FRONTEND)

        success(await inhibit('shutdown', 12))
        index = len(mock.readers) - 1
        proc.send_signal(signal.SIGTERM)
        assert await asyncio.wait_for(proc.wait(), 5) == 0
        await until(lambda: index in mock.closed)
        await until(lambda: len(mock.closed) == len(mock.readers))
        print('Inhibit tests passed: forwarding, errors, ownership, cancellation, disconnect, shutdown')
    finally:
        if proc.returncode is None:
            proc.kill()
            await proc.wait()
        for bus in [front, stranger, mock_bus]:
            bus.disconnect()
            await bus.wait_for_disconnect()

asyncio.run(main())
