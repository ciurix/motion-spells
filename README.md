[![Review Assignment Due Date](https://classroom.github.com/assets/deadline-readme-button-22041afd0340ce965d47ae6ef1cefeee28c7c493a6346c4f15d667ab976d596c.svg)](https://classroom.github.com/a/sVB0pKHF)

# Motion Spells

Wave a wand, and a router does something about it. An ESP32-S3 with an MPU6050
recognises hand gestures with a neural network running on the microcontroller
itself, and each gesture maps to a network operation.

```
wand/          ESP32-S3 firmware (no_std Rust) + gesture model and training
controller/    host side: turns a recognised spell into a router action
```

## wand/

Reads the MPU6050 over I2C and classifies motion with a quantised CNN compiled
into the firmware by MicroFlow. Three binaries:

| Binary | Purpose |
|---|---|
| `spells` | neural-network recogniser (needs `--features ml` and a trained model) |
| `capture` | records gesture windows as CSV for training |
| `fils-project` | earlier threshold-based recogniser, kept as a fallback |

```
cd wand
cargo build --release --bin spells --features ml
espflash flash --monitor --port COM6 target/xtensa-esp32s3-none-elf/release/spells
```

`wand/train/` holds the training pipeline and `wand/data/` the recordings -
see `wand/train/README.md`.

## controller/

Not written yet. See `controller/README.md`.

## Hardware

ESP32-S3 Supermini and a GY-521 (MPU6050) breakout, wired SDA to GPIO12, SCL to
GPIO13, VCC to 3V3, GND to GND.
