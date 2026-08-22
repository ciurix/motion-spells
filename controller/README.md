# Controller

STM32U545RE-Q that receives the spell the wand cast and shows it on an OLED.

```
NodeMCU --UART 115200--> STM32U545RE --I2C--> SSD1306
```

The STM32 has no radio of its own, so a NodeMCU listens for the wand's ESP-NOW
broadcast and repeats the spell name over UART as a line of text. See
`nodemcu-bridge/` for that half.

## Wiring

| Signal | Nucleo pin | MCU pin | Goes to |
|---|---|---|---|
| I2C SCL | D15 | PB6 | SSD1306 SCL |
| I2C SDA | D14 | PB7 | SSD1306 SDA |
| UART RX | D0 | PA3 | NodeMCU D4 |
| 3V3 / GND | - | - | SSD1306 power |

Two details this board gets differently from most Nucleo-64s, both worth
knowing before debugging silence:

- **The Arduino I2C header is PB6/PB7**, not the PB8/PB9 that most Nucleo-64
  boards use.
- **USART1 (PA9/PA10) is wired to the ST-Link virtual COM port**, so the D0/D1
  Arduino pins are LPUART1 instead. That is why the link runs on LPUART1.

Only one UART wire is needed - the STM32 never talks back to the NodeMCU.

## Build and flash

```
cargo run --release
```

That flashes over the on-board ST-Link and streams the defmt logs back, via the
runner configured in `.cargo/config.toml`. With more than one probe attached,
name the one you want:

```
probe-rs list
cargo run --release -- --probe 0483:374e:<serial>
```

A healthy start looks like:

```
controller starting
display responded at 0x3c
display initialised
waiting for spells on LPUART1 @115200 (PA3 / D0)
```

## What it does

On boot it probes the I2C bus for the display at 0x3C and 0x3D, and if neither
answers it scans the whole bus and logs whatever it finds - a wiring fault shows
up as data rather than a blank screen.

Then it reads bytes from LPUART1, assembles them into lines, and draws each
recognised spell with the action it maps to. An unrecognised name is still
displayed, so a mismatch between the wand and this table is visible rather than
silently ignored.

The spell-to-action table lives here rather than on the wand, so what a gesture
*means* can change without reflashing the wand.

## Not done yet

The display shows the action; nothing performs it. Carrying it out needs a route
from the STM32 to the router - and the router platform decided, since Cisco IOS,
MikroTik and Linux need quite different commands.
