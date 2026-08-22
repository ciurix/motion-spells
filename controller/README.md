# Controller

Host side of the project. Nothing here yet.

The wand recognises a gesture and prints the spell over USB serial. This is
where the program that acts on that goes: read the serial line, and carry out
the router action the spell maps to.

| Spell | Action |
|---|---|
| left | cycle to previous interface |
| right | cycle to next interface |
| up | turn on interface |
| down | shutdown interface |
| push | stress test interface (iperf3 UDP flood) |
| circular | backup router config |

The split exists because the wand runs `no_std` Rust on a microcontroller and
cannot open an SSH session or run iperf3 - those need an operating system. The
wand decides *what* was cast; the controller decides *what happens*.
