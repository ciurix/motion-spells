# Wand

ESP32-S3 with an MPU6050. Recognises a hand gesture with a neural network
running on the chip itself, then broadcasts the spell over ESP-NOW.

## Wiring

| MPU6050 (GY-521) | ESP32-S3 |
|---|---|
| SDA | GPIO12 |
| SCL | GPIO13 |
| VCC | 3V3 |
| GND | GND |

## Binaries

| Binary | What it is | Build |
|---|---|---|
| `spells` | the real thing: CNN recogniser + radio | `--features ml,radio` |
| `capture` | records gestures for training | default |
| `fils-project` | earlier threshold recogniser, kept as a fallback | default |

```
cargo build --release --bin spells --features ml,radio
espflash flash --monitor --port COM6 target/xtensa-esp32s3-none-elf/release/spells
```

The two features are separable on purpose: `--features ml` alone builds the
recogniser without bringing up the radio, which is easier to debug when only the
gesture side is in question.

`spells` needs `model/spells.tflite` to exist, because the model is compiled
into the firmware at build time. It is gated behind the `ml` feature so a
missing model cannot break an ordinary `cargo build`.

## How inference works

The MPU6050 is sampled at 50 Hz into a rolling buffer of the last 128 samples -
2.56 seconds of accelerometer and gyroscope data, six channels. Every ten
samples the window is fed through a quantised convolutional network, and a class
above 80% confidence is announced and broadcast.

Two guards keep it from firing constantly:

- a **motion gate** skips windows that hold no real movement, measured as mean
  absolute deviation from the window's own average, so a wand resting at any
  angle counts as still
- a **cooldown** blocks new casts for about 1.5 seconds after one fires, since
  the same gesture would otherwise be recognised repeatedly as it slides through
  the window

The `negative` class is what properly teaches the model to reject non-gestures;
the guards above only reduce the damage while it is missing.

## The model

Trained offline from recordings made with `capture`, then compiled into the
firmware by [MicroFlow](https://crates.io/crates/microflow)'s `model!` macro,
which turns a `.tflite` file into Rust at build time. `model/labels.txt` is read
with `include_str!`, so the firmware's class order cannot drift from what the
training script produced.

See `train/README.md` for recording and training.

MicroFlow constrains the architecture: it runs Conv2D, DepthwiseConv2D,
FullyConnected, AveragePool2D, Reshape and ReLU/ReLU6/Softmax, all quantised.
No MaxPool2D, and `fully_connected` accepts only per-tensor quantisation. The
training script checks both and refuses to produce a model the firmware cannot
run.

## The radio

`esp-radio` rather than the older `esp-wifi`: esp-wifi 0.15 pins
`xtensa-lx-rt` 0.20 while esp-hal 1.1 needs 0.22, and that crate declares
`links`, so only one version can exist in a build.

Spells are broadcast rather than unicast, which avoids hard-coding the
receiver's MAC address, on **channel 1** to match the NodeMCU bridge. ESP-NOW
only reaches peers on the same channel - if packets never arrive, check that
first.

Two setup requirements that are easy to miss: the scheduler has to be started
before the radio is initialised, and `esp-rtos` only provides the symbols the
radio links against when its own `esp-radio` feature is enabled.
