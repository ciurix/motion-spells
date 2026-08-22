# Router manager

Reacts to spells by acting on the OpenWRT router. Runs on the laptop.

```
NodeMCU --USB serial--> manager.py --SSH--> OpenWRT 192.168.100.1
```

The manager reads spell names from the NodeMCU's USB serial port and, for each
one, runs the matching command on the router over SSH.

## Why this runs on the laptop and not a microcontroller

SSH needs a full crypto stack and a fair amount of RAM. The ESP8266 does not
have the memory for it, and there is no `no_std` Rust SSH client for the STM32.
The laptop is already on the router's network, so it does the SSH; the
microcontrollers handle sensing, radio, and display.

## Setup

```
pip install -r requirements.txt
```

Copy `.env.example` (at the repo root) to `.env` and fill in the WiFi and SSH
credentials. `.env` is gitignored - it never gets committed.

paramiko needs a Python it has wheels for; on Python 3.14 use a 3.12/3.13 venv
if the install fails. `pyserial` is pure Python and works anywhere.

## Running

```
python manager.py
```

It starts in **dry run** (the default in `.env`): it reads spells and prints the
command it *would* run, without touching the router. Watch it, cast a few
spells, confirm the mapping looks right.

When you are ready for it to actually run commands, set `DRY_RUN=false` in
`.env`. Be aware that DOWN really does take an interface offline - if you do
that to the interface you are connected through, you lose the connection.

## Spell -> command (OpenWRT 24.10)

Interfaces are the router's logical interfaces (`lan`, `wan`, ...) from
`ubus call network.interface dump`. left/right move a cursor over them;
up/down/push act on the selected one.

| Spell | Command |
|---|---|
| left | select previous interface |
| right | select next interface |
| up | `ifup <selected>` |
| down | `ifdown <selected>` |
| push | `iperf3 -u -b 100M -t 5 -c <IPERF_SERVER>` |
| circular | `sysupgrade -b /tmp/openwrt-backup.tar.gz` |

Notes:

- **push** needs `iperf3` on the router (`opkg install iperf3`) and a host
  running `iperf3 -s` at `IPERF_SERVER`.
- **circular** writes the backup to `/tmp` on the router; copy it off with
  `scp root@<host>:/tmp/openwrt-backup.tar.gz .`

The mapping lives here, so what a gesture does can change without touching any
firmware.
