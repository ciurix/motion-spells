# NodeMCU ESP-NOW bridge

The STM32 has no radio, so a NodeMCU acts as one: it receives the wand's
ESP-NOW broadcast and repeats the spell name over UART.

```
wand (ESP32-S3) --ESP-NOW broadcast--> NodeMCU --UART 115200--> STM32 --> OLED
```

## Why this part is C++

Everything else in the project is Rust. The ESP8266's WiFi is a closed-source
binary blob that only links against Espressif's C SDK, and the Rust driver for
Espressif radios (`esp-wifi`) supports the ESP32 family only - asking it for an
ESP8266 gives:

```
error: unrecognized feature for crate esp-wifi: esp8266
```

Rust itself compiles for the ESP8266 (`xtensa-esp8266-none-elf` exists), so
GPIO and UART would be fine. It is specifically the radio that has no Rust
driver.

## Wiring

| NodeMCU | STM32 |
|---|---|
| D4 (GPIO2, UART1 TX) | D0 (PA3, LPUART1 RX) |
| GND | GND |

One data wire is enough - the STM32 never replies. Both boards are 3.3V, so no
level shifter.

UART1 on the ESP8266 is transmit-only, which suits a one-way link and leaves
`Serial` (UART0, on the USB port) free for debugging. Watch it at 115200 while
testing to see each packet arrive.

Power the NodeMCU from its own USB cable, or from the Nucleo's **5V** pin - not
3V3. It draws current spikes of a few hundred mA when the radio transmits, and
the Nucleo's 3.3V regulator will brown out. If powering from a separate supply,
tie the grounds together.

## Flashing

Either the Arduino IDE or `arduino-cli`. No libraries to install - `ESP8266WiFi`
and `espnow` ship with the core.

With the IDE, having added the Boards Manager URL
`https://arduino.esp8266.com/stable/package_esp8266com_index.json`:

1. Tools > Board > NodeMCU 1.0 (ESP-12E Module)
2. Tools > Port > the NodeMCU's port
3. Upload

With `arduino-cli`, from the `controller/` directory:

```
arduino-cli config add board_manager.additional_urls https://arduino.esp8266.com/stable/package_esp8266com_index.json
arduino-cli core update-index
arduino-cli core install esp8266:esp8266
arduino-cli compile --fqbn esp8266:esp8266:nodemcuv2 nodemcu-bridge
arduino-cli upload -p COM9 --fqbn esp8266:esp8266:nodemcuv2 nodemcu-bridge
```

## Watching it run

The USB console runs at 115200 and should show:

```
nodemcu esp-now bridge
mac: 24:4C:AB:6C:7F:05
channel: 1
listening for spells
```

Unreadable characters before that are the ESP8266's bootloader, which prints at
74880 baud regardless of what the sketch later selects. Not a fault.

## Channel

ESP-NOW only works between devices on the same WiFi channel. This sits on
channel 1, and the wand must transmit on channel 1 too. If packets never
arrive, that is the first thing to check.
