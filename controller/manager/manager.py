#!/usr/bin/env python3
"""Router manager: turns a cast spell into an action on the OpenWRT router.

The wand recognises a gesture and broadcasts it over ESP-NOW; the NodeMCU
receives it and prints the spell name on its USB serial port. This reads that
port, and for each spell runs the matching command on the OpenWRT router over
SSH.

SSH lives here, on the laptop, rather than on a microcontroller: the ESP8266
has nowhere near the RAM for an SSH client and there is no no_std Rust SSH
stack for the STM32. The laptop is already on the router's network, so it is
the natural place for it.

Credentials come from the gitignored .env at the repo root. Nothing is
hard-coded here.

    pip install -r requirements.txt
    python manager.py            # dry run by default - prints, does not touch the router
    # set DRY_RUN=false in .env to actually run commands

Target: OpenWRT 24.10.
"""

import pathlib
import re
import sys
import time

# --- .env loader (no external dependency) ---------------------------------

def load_env():
    """Read key=value pairs from the repo-root .env into a dict."""
    root = pathlib.Path(__file__).resolve().parents[2]
    env_path = root / ".env"
    if not env_path.exists():
        sys.exit(
            f"No .env found at {env_path}.\n"
            f"Copy .env.example to .env and fill in your values."
        )
    env = {}
    for line in env_path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        env[key.strip()] = value.strip().strip('"')
    return env


# --- Spell -> OpenWRT command ---------------------------------------------

class Router:
    """Runs spell actions against the OpenWRT router over SSH.

    Interfaces are the router's logical network interfaces (lan, wan, ...) as
    reported by ubus. left/right move a selection cursor over them; up/down/push
    act on whatever is selected.
    """

    def __init__(self, env, ssh):
        self.env = env
        self.ssh = ssh
        self.interfaces = []
        self.index = 0

    def refresh_interfaces(self):
        # ubus lists the logical interfaces; fall back to a sensible default if
        # the parse fails so the demo still does something.
        out = self.ssh.run("ubus call network.interface dump")
        names = re.findall(r'"interface"\s*:\s*"([^"]+)"', out or "")
        names = [n for n in names if n != "loopback"]
        self.interfaces = names or ["lan", "wan"]
        self.index %= len(self.interfaces)

    @property
    def selected(self):
        if not self.interfaces:
            self.refresh_interfaces()
        return self.interfaces[self.index]

    def handle(self, spell):
        spell = spell.upper()
        if not self.interfaces:
            self.refresh_interfaces()

        if spell == "LEFT":
            self.index = (self.index - 1) % len(self.interfaces)
            print(f"  selected interface: {self.selected}")
        elif spell == "RIGHT":
            self.index = (self.index + 1) % len(self.interfaces)
            print(f"  selected interface: {self.selected}")
        elif spell == "UP":
            print(f"  bringing up {self.selected}")
            print(self.ssh.run(f"ifup {self.selected}"))
        elif spell == "DOWN":
            print(f"  shutting down {self.selected}")
            print(self.ssh.run(f"ifdown {self.selected}"))
        elif spell == "PUSH":
            server = self.env.get("IPERF_SERVER", "")
            print(f"  iperf3 UDP flood -> {server}")
            print(self.ssh.run(f"iperf3 -u -b 100M -t 5 -c {server}"))
        elif spell == "CIRCULAR":
            path = "/tmp/openwrt-backup.tar.gz"
            print(f"  backing up config to {path} on the router")
            print(self.ssh.run(f"sysupgrade -b {path} && ls -la {path}"))
        else:
            print(f"  unknown spell: {spell}")


# --- SSH -------------------------------------------------------------------

class Ssh:
    """Thin SSH wrapper. In dry-run mode it prints commands instead of running
    them, so the whole pipeline can be exercised without touching the router."""

    def __init__(self, env):
        self.host = env["SSH_HOST"]
        self.user = env["SSH_USER"]
        self.password = env["SSH_PASSWORD"]
        self.dry_run = env.get("DRY_RUN", "true").lower() != "false"
        self._client = None

    def connect(self):
        if self.dry_run:
            print(f"[dry run] would connect to {self.user}@{self.host}")
            return
        try:
            import paramiko
        except ImportError:
            sys.exit(
                "paramiko is needed for live SSH: pip install -r requirements.txt\n"
                "(or leave DRY_RUN=true in .env to test without it)"
            )
        client = paramiko.SSHClient()
        client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
        client.connect(
            self.host,
            username=self.user,
            password=self.password,
            look_for_keys=False,
            allow_agent=False,
            timeout=10,
        )
        self._client = client
        print(f"connected to {self.user}@{self.host}")

    def run(self, command):
        if self.dry_run:
            return f"[dry run] {command}"
        _in, out, err = self._client.exec_command(command, timeout=30)
        result = out.read().decode(errors="replace")
        error = err.read().decode(errors="replace")
        return (result + error).rstrip() or "(no output)"

    def close(self):
        if self._client:
            self._client.close()


# --- Serial spell source ---------------------------------------------------

def spells_from_serial(port, baud):
    """Yield spell names as they arrive on the NodeMCU's USB serial.

    The bridge prints lines like `24:4C:.. -> LEFT`; we pull the name off the
    end. Falling back to any bare word keeps it working if that format changes.
    """
    try:
        import serial
    except ImportError:
        sys.exit("pyserial is needed: pip install -r requirements.txt")

    known = {"LEFT", "RIGHT", "UP", "DOWN", "PUSH", "CIRCULAR"}
    with serial.Serial(port, baud, timeout=1) as ser:
        print(f"listening for spells on {port} @ {baud}")
        buffer = b""
        while True:
            chunk = ser.read(64)
            if not chunk:
                continue
            buffer += chunk
            while b"\n" in buffer:
                raw, buffer = buffer.split(b"\n", 1)
                text = raw.decode(errors="replace").strip()
                # Last whitespace-separated token, upper-cased.
                token = text.split()[-1].upper() if text.split() else ""
                if token in known:
                    yield token


def main():
    env = load_env()
    ssh = Ssh(env)
    router = Router(env, ssh)

    mode = "DRY RUN (no router changes)" if ssh.dry_run else "LIVE"
    print(f"router manager - {mode}")
    print(f"router: {env['SSH_USER']}@{env['SSH_HOST']}  network: {env.get('WIFI_SSID', '?')}")

    ssh.connect()
    try:
        router.refresh_interfaces()
        print(f"interfaces: {', '.join(router.interfaces)}")
    except Exception as exc:  # noqa: BLE001 - report and carry on
        print(f"could not list interfaces yet: {exc}")

    port = env.get("SPELL_SERIAL_PORT", "COM9")
    baud = int(env.get("SPELL_SERIAL_BAUD", "115200"))

    try:
        for spell in spells_from_serial(port, baud):
            print(f"\n>>> {spell}  ({time.strftime('%H:%M:%S')})")
            try:
                router.handle(spell)
            except Exception as exc:  # noqa: BLE001 - one bad command shouldn't stop the loop
                print(f"  error: {exc}")
    except KeyboardInterrupt:
        print("\nstopping")
    finally:
        ssh.close()


if __name__ == "__main__":
    main()
