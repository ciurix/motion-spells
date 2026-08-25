[![Review Assignment Due Date](https://classroom.github.com/assets/deadline-readme-button-22041afd0340ce965d47ae6ef1cefeee28c7c493a6346c4f15d667ab976d596c.svg)](https://classroom.github.com/a/sVB0pKHF)

# Motion Spells

Wave a wand, and a router does something about it.

An ESP32-S3 with an accelerometer recognises hand gestures using a neural
network running on the microcontroller itself - no cloud, no phone - and
broadcasts the recognised "spell" over ESP-NOW. An STM32 picks it up and
displays the network operation it maps to.

```
   wand                          bridge                    controller
+-----------+   ESP-NOW ch1   +-----------+   UART     +--------------+
| ESP32-S3  | --------------> | NodeMCU   | ---------> | STM32U545RE  |
| MPU6050   |    broadcast    | ESP8266   |  115200    | SSD1306 OLED |
| CNN       |                 |           |            |              |
+-----------+                 +-----------+            +--------------+
   Rust                          C++                       Rust
```

| Gesture | Action |
|---|---|
| sweep left | cycle to previous interface |
| sweep right | cycle to next interface |
| sweep up | turn on interface |
| sweep down | shutdown interface |
| thrust forward | stress test interface (iperf3 UDP flood) |
| draw a circle | backup router config |

## Layout

```
wand/         ESP32-S3 firmware, gesture model, and training pipeline
controller/   STM32 firmware and the NodeMCU radio bridge
tools/        one-shot setup and flash scripts for a new Windows machine
```

Each directory is its own Cargo project with its own toolchain - `wand/` builds
for Xtensa with the `esp` channel, `controller/` for Cortex-M33 with `stable`.
Build from inside them, not from the repository root.

## Hardware

| Board | Role | Connections |
|---|---|---|
| ESP32-S3 Supermini | wand | GY-521 (MPU6050): SDA=GPIO12, SCL=GPIO13, 3V3, GND |
| NodeMCU (ESP8266) | radio bridge | D4 -> STM32 D0, GND -> STM32 GND |
| NUCLEO-U545RE-Q | controller | SSD1306: SCL=D15 (PB6), SDA=D14 (PB7), 3V3, GND |

Power the NodeMCU from its own USB or the Nucleo's 5V pin. Its transmit current
spikes will brown out the Nucleo's 3.3V regulator.

## Setting up on a new machine

Three boards means three toolchains. On 64-bit Windows, both are scripted:

```
powershell -ExecutionPolicy Bypass -File tools\install.ps1
# open a new terminal, plug in all three boards
powershell -ExecutionPolicy Bypass -File tools\flash-all.ps1
```

`install.ps1` installs the Rust toolchains (stable and Xtensa), espflash,
probe-rs, arduino-cli with the ESP8266 core, Python with the manager's
dependencies, and prompts for the credentials that go in the gitignored `.env`.

`flash-all.ps1` builds and flashes each board in turn, listens to it afterwards
to confirm it came up, and finishes by casting a spell from the wand with no
gesture to check the whole chain. See `tools/README.md`.

## Running it

```
cd wand
cargo build --release --bin spells --features ml,radio
espflash flash --monitor --port COM6 target/xtensa-esp32s3-none-elf/release/spells
```

```
cd controller
cargo run --release
```

The NodeMCU is flashed once and then left alone - see
`controller/nodemcu-bridge/README.md`.

## How the gesture recognition works

The wand samples acceleration and rotation at 50 Hz and keeps the last 128
samples - a 2.56 second window. Several times a second it runs that window
through a small convolutional network compiled into the firmware, and if one
class comes back above 80% confidence it announces that spell.

The model is trained offline from recordings made with the wand itself. See
`wand/train/README.md` for the pipeline, and `wand/README.md` for how inference
is wired into the firmware.

## Status

Working end to end: a gesture on the wand appears on the controller's display.

The model currently knows four gestures - left, right, up and down. Still to
record are *push*, *circular*, and the *negative* class that lets the model
decide a movement was not a spell at all; without it every movement is forced
into one of the four it knows.

The controller displays the action but does not yet carry it out. Doing that
needs a route from the STM32 to the router, and the router platform decided.
