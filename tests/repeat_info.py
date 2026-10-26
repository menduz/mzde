# Prints the key repeat of the seat: "<rate> <delay>". The compositor sends
# wl_keyboard.repeat_info of the active keyboard when a client gets the
# keyboard of the seat.
from pywayland.client import Display
from pywayland.protocol.wayland import WlSeat

display = Display()
display.connect()

seats = []


def on_global(registry, name, interface, version):
    if interface == "wl_seat" and not seats:
        seats.append(registry.bind(name, WlSeat, min(version, 7)))


registry = display.get_registry()
registry.dispatcher["global"] = on_global
display.roundtrip()
if not seats:
    raise SystemExit("repeat_info.py: no wl_seat")

repeat = []
keyboard = seats[0].get_keyboard()
keyboard.dispatcher["repeat_info"] = lambda _keyboard, rate, delay: repeat.append((rate, delay))
display.roundtrip()
display.roundtrip()
display.disconnect()

if not repeat:
    raise SystemExit("repeat_info.py: no repeat_info event")
print(*repeat[-1])
