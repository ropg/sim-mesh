# simradio for a Portduino firmware

A PlatformIO library that puts sim-mesh's simulated SX1262 (`radio/`) under a
firmware built on Portduino, Meshtastic's Arduino API for Linux, in place of
`/dev/spidev` and libgpiod. The firmware's radio driver is unchanged: it
talks SPI and pins as on a board, and each transfer reaches the chip model
whole.

```
firmware ── SPI.transfer(frame) ─────► SimChip ── simradio_transfer ──► chip model
firmware ── digitalWrite(RESET) ─────► ResetPin: rising edge ── simradio_reset
firmware ◄── digitalRead(BUSY/DIO1) ── LevelPin ◄── on_pin ── chip model
firmware ── rnode_idle(max_ms) ──────► wait until max_ms, DIO1 rises or rnode_wake() (node time)
```

## What a firmware calls

Three functions, each a weak function of the firmware's own whose strong
definition is here:

- `void native_radio_backend_init()`, once, after the firmware knows its
  pins and before its `SPI.begin()` and its first touch of the radio. It
  opens the station's link to the ether (`SIM_MESH_NODE_ID`,
  `SIM_MESH_BIND_ADDR`, `SIM_MESH_ETHER`) and chip slot 0, installs its
  `SPIChip` into the global `SPI`, and binds the chip's lines with `gpioBind`.
- `void rnode_idle(uint32_t max_ms)`, where the firmware has nothing to do
  for up to `max_ms`. It waits on a condition variable until then or until
  DIO1 rises; in a virtual-time run the time shim answers the wait in node
  time, and Portduino's next `gpioIdle()` runs the DIO1 interrupt handler.
  `max_ms == 0` returns at once.
- `void rnode_wake()`, from any thread that has work for the firmware's loop
  (a reader thread that took console input, say): the idle in progress
  returns at once, or the next one does.

The firmware must hand every SPI transaction to `SPI.transfer` as one frame,
NSS low to NSS high, which is what a driver written for spidev already does.
Binding a pin also turns off Portduino's 100 ms sleep between `loop()` calls,
so the firmware's own idle is the only wait in its loop.

## Its pins

The chip's four lines are bound at `SIMRADIO_PIN_NSS`, `SIMRADIO_PIN_RESET`,
`SIMRADIO_PIN_BUSY` and `SIMRADIO_PIN_DIO1` (1, 2, 3 and 4 when unset), which
must be the pins the firmware drives the chip on. NSS does nothing, since
each frame is already one transfer.

Portduino finds an interrupt's edge by reading the pin once a loop, so a
firmware that idles between loops could miss DIO1 going low and high again
(one reception cleared, the next arrived) and with it the interrupt. DIO1
never loses a rise: when the line rose since Portduino's last read and that
read saw it high, it reads low once and the idle in progress ends, so the
next loop's read sees it rise.

## Building with it

`sim` builds `radio/build/libsimradio-sx1262.so` as it starts, which
`link.py` links the program with by name; the firmware's zip does not carry
it, since sim-mesh provides it when it starts the station. A firmware's
environment lists this library with a `symlink://` entry in `lib_deps`.
`casefold.py`, as a `pre:` extra script, lets Portduino build on a
case-insensitive filesystem, where its `String.h` would otherwise answer
`#include <string.h>`. This library is linked as objects, not as an archive,
so its strong definitions win over the firmware's weak ones.

**The other architecture.** Pre-built firmware comes for aarch64 and x86_64,
so a firmware builds both, the one that is not the machine's with that
architecture's cross g++ (`<arch>-linux-gnu-g++`) and its libc from the
multiarch packages. `SIM_MESH_ARCH=<arch>` in the build's environment names
it; `cross.py`, as a `post:` extra script, swaps the cross tools into the
program's, the sources' and every library's environment, and `link.py`
compiles the radio with them into `radio/build.linux-<arch>/` and links that
copy instead. Each architecture wants a build directory of its own
(`PLATFORMIO_BUILD_DIR`).
