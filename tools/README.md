# Setting the project up on another machine

Two scripts. The first installs the toolchains, the second builds and flashes
all three boards and checks each one came up.

```
powershell -ExecutionPolicy Bypass -File tools\install.ps1
# open a new terminal, plug in all three boards
powershell -ExecutionPolicy Bypass -File tools\flash-all.ps1
```

Windows 10 or 11, 64-bit. Both scripts are safe to run again - they skip
whatever is already done.

## install.ps1

Installs, in order:

| Component | Why |
|---|---|
| Visual Studio C++ build tools | cross-compiling still links build scripts and proc-macros for the host |
| rustup + stable + `thumbv8m.main-none-eabihf` | the controller firmware |
| cargo-binstall | fetches the tools below as prebuilt binaries instead of compiling them |
| espflash | flashes the wand |
| probe-rs | flashes the STM32 over the on-board ST-Link |
| espup + the `esp` toolchain | the ESP32-S3 is Xtensa, which upstream Rust does not target |
| arduino-cli + the ESP8266 core | the NodeMCU bridge sketch |
| Python + pyserial + paramiko | the router manager |
| `.env` | credentials, prompted for and written locally |

Expect it to take a while on a clean machine - the Visual Studio tools and the
Xtensa LLVM are a few GB between them. Everything else is quick.

Two things it does that are worth knowing about:

- **It makes the Xtensa environment permanent.** espup normally leaves a
  `export-esp.ps1` in your home directory that has to be sourced in every new
  terminal. The script reads it once and writes the values into the user
  environment instead, so `cargo build` in `wand/` just works.
- **It only touches the user environment**, apart from the Visual Studio
  installer, which raises its own UAC prompt.

Flags: `-SkipMsvc`, `-SkipRust`, `-SkipXtensa`, `-SkipArduino`, `-SkipPython`,
`-NoEnvPrompt`. Useful when one step failed and you do not want to sit through
the others again.

### Credentials

`.env` is gitignored, so it does not come with the clone. The script copies
`.env.example` over it and asks for the WiFi and router details. Nothing is
written anywhere but that file, and `DRY_RUN` stays `true` until you change it -
until then the manager prints the commands it would run instead of running them.

### Drivers

The ESP32-S3 has native USB and needs no driver. The Nucleo's ST-Link presents
itself as a WinUSB device and needs none either.

The NodeMCU is the exception: its USB-to-serial chip does. If it does not appear
as a COM port, install the driver for whichever chip it carries - CH340 or
CP2102, printed on the chip itself.

## flash-all.ps1

Goes board by board, and after each flash listens to that board to confirm it is
actually running rather than trusting that "flashing completed" meant it worked.

| Stage | Flashed with | Considered working when |
|---|---|---|
| wand | `cargo build --features ml,radio` + espflash | console shows the banner, `Radio up`, and `Sensor OK` |
| bridge | `arduino-cli upload` | console shows `esp-now bridge` / `listening for spells` |
| controller | `probe-rs run` | defmt shows `display responded at 0x3c` and `waiting for spells` |
| end-to-end | wand rebuilt with `selftest` | STM32 log shows `cast: <spell>` |

Ports are guessed from the USB device descriptions and offered for
confirmation; `-WandPort COM6 -BridgePort COM9` skips the guessing.

Output from every step is kept under `tools/logs/`, which is gitignored.

### The end-to-end stage

Flashing three boards proves each one runs. It does not prove they are talking
to each other, and the wire from the bridge to the STM32 is the one link nothing
else exercises.

So the last stage rebuilds the wand with the `selftest` feature, which casts
LEFT, RIGHT, UP and DOWN at boot with no gesture, attaches to the STM32, and
watches for them to arrive. Each should appear on the OLED in turn.

Afterwards it puts the normal firmware back - the selftest build casts spells
every time it powers up, which is not what you want mid-demonstration. If that
last flash fails the script says so; rerun it by hand from `wand/`.

If nothing arrives, work backwards from the display: the wire first
(NodeMCU **D4/GPIO2** to Nucleo **D0/PA3**, and a shared ground), then the radio
by watching the bridge's own console while the wand boots, then power - the
NodeMCU wants its own USB or the Nucleo's **5V** pin, never 3V3.

Skip the whole stage with `-SkipEndToEnd`. `-CheckOnly` lists the toolchain and
the detected boards without flashing anything.

## After it passes

```
cd controller\manager
python manager.py
```
