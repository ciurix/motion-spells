//! Controller: STM32U545RE-Q showing the spell the wand just cast.
//!
//! The wand broadcasts over ESP-NOW; a NodeMCU receives it and forwards the
//! spell name as a line of text over UART. This end waits on that line and
//! draws it to an SSD1306 OLED.
//!
//! Wiring, to the Arduino header on the Nucleo:
//!   OLED   SCL -> D15 (PB6)   SDA -> D14 (PB7)   VCC -> 3V3   GND -> GND
//!   NodeMCU TX  -> D0  (PA3)  RX  -> D1  (PA2)   GND -> GND
//!
//! Note the crossover: the NodeMCU's TX goes to the STM32's RX.
//!
//! Run with `cargo run --release`, which flashes over the on-board ST-Link and
//! streams the defmt logs back.

#![no_std]
#![no_main]

use defmt::{info, warn};
use embassy_executor::Spawner;
use embassy_stm32::i2c::{self, I2c};
use embassy_stm32::rcc::Sysclk;
use embassy_stm32::time::Hertz;
use embassy_stm32::usart::{self, UartRx};
use embassy_time::{Duration, Timer};
use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, ascii::FONT_9X18_BOLD, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use ssd1306::{mode::BufferedGraphicsMode, prelude::*, I2CDisplayInterface, Ssd1306};

use defmt_rtt as _;
use panic_probe as _;

/// SSD1306 modules are strapped to 0x3C almost always, 0x3D occasionally.
const CANDIDATE_ADDRS: [u8; 2] = [0x3C, 0x3D];

/// Longest line we will accept before assuming the sender is confused.
const LINE_MAX: usize = 32;

/// Network being managed. Injected from the gitignored .env by build.rs; the
/// fallback is only used when .env is absent (e.g. a fresh checkout).
const NETWORK: &str = match option_env!("WIFI_SSID") {
    Some(name) => name,
    None => "network",
};

/// What each spell does. The wand sends the name; this end owns the meaning,
/// so the mapping can change without reflashing the wand.
fn action_for(spell: &str) -> Option<&'static str> {
    Some(match spell {
        "LEFT" => "prev interface",
        "RIGHT" => "next interface",
        "UP" => "interface up",
        "DOWN" => "interface down",
        "PUSH" => "iperf3 flood",
        "CIRCULAR" => "backup config",
        _ => return None,
    })
}

type Display<'a> = Ssd1306<
    I2CInterface<I2c<'a, embassy_stm32::mode::Blocking, i2c::Master>>,
    DisplaySize128x64,
    BufferedGraphicsMode<DisplaySize128x64>,
>;

/// Draws a heading with a rule under it and a line of detail beneath.
fn draw_screen(display: &mut Display<'_>, heading: &str, detail: &str) {
    let title_style = MonoTextStyleBuilder::new()
        .font(&FONT_9X18_BOLD)
        .text_color(BinaryColor::On)
        .build();
    let body_style = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();

    display.clear(BinaryColor::Off).ok();
    Text::with_baseline(heading, Point::new(4, 8), title_style, Baseline::Top)
        .draw(display)
        .ok();
    Rectangle::new(Point::new(0, 30), Size::new(128, 1))
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(display)
        .ok();
    Text::with_baseline(detail, Point::new(4, 38), body_style, Baseline::Top)
        .draw(display)
        .ok();
    display.flush().ok();
}

/// Boot screen: "managing <network>" over a progress bar that fills once, then
/// the screen is left showing the network. Cosmetic - the STM32 does not join
/// WiFi itself; it announces which network the system is looking after.
async fn boot_animation(display: &mut Display<'_>) {
    let heading = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();
    let name_style = MonoTextStyleBuilder::new()
        .font(&FONT_9X18_BOLD)
        .text_color(BinaryColor::On)
        .build();

    // Bar geometry: an outlined track that fills left to right.
    let bar_x = 4i32;
    let bar_y = 48i32;
    let bar_w = 120u32;
    let bar_h = 10u32;

    const STEPS: u32 = 48;
    for step in 0..=STEPS {
        display.clear(BinaryColor::Off).ok();

        Text::with_baseline("managing", Point::new(4, 6), heading, Baseline::Top)
            .draw(display)
            .ok();
        Text::with_baseline(NETWORK, Point::new(4, 22), name_style, Baseline::Top)
            .draw(display)
            .ok();

        // Track outline.
        Rectangle::new(Point::new(bar_x, bar_y), Size::new(bar_w, bar_h))
            .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
            .draw(display)
            .ok();
        // Fill proportional to progress, inset by one pixel so it sits inside
        // the outline.
        let fill = (bar_w - 2) * step / STEPS;
        if fill > 0 {
            Rectangle::new(
                Point::new(bar_x + 1, bar_y + 1),
                Size::new(fill, bar_h - 2),
            )
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(display)
            .ok();
        }

        display.flush().ok();
        Timer::after(Duration::from_millis(45)).await;
    }
}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    // The chip boots on MSI at 4 MHz, but embassy's I2C driver asserts that the
    // peripheral clock is at least 8 MHz before it will set up 400 kHz
    // fast-mode. Running from the 16 MHz HSI clears that and is plenty for
    // driving a small OLED - no PLL or voltage-scaling juggling needed.
    let mut config = embassy_stm32::Config::default();
    config.rcc.hsi = true;
    config.rcc.sys = Sysclk::HSI;

    let p = embassy_stm32::init(config);
    info!("controller starting");

    // --- OLED on I2C1 ---
    // 400 kHz is the SSD1306's fast-mode rating; drop to 100 kHz if the wiring
    // is long or flaky. The module carries its own pull-ups, so the internal
    // ones stay off.
    let mut i2c_config = i2c::Config::default();
    i2c_config.frequency = Hertz(400_000);

    // D15/D14 on this board are PB6/PB7 - not the PB8/PB9 that most Nucleo-64
    // boards use for the Arduino I2C header.
    let mut i2c = I2c::new_blocking(
        p.I2C1,
        p.PB6, // SCL - D15
        p.PB7, // SDA - D14
        i2c_config,
    );

    // Probe before assuming: a zero-length write is answered only if something
    // is actually listening at that address.
    let mut found = false;
    for candidate in CANDIDATE_ADDRS {
        if i2c.blocking_write(candidate, &[]).is_ok() {
            info!("display responded at {=u8:#04x}", candidate);
            found = true;
            break;
        }
    }
    if !found {
        warn!("no display at 0x3C or 0x3D - scanning the whole bus");
        for candidate in 0x03..=0x77u8 {
            if i2c.blocking_write(candidate, &[]).is_ok() {
                warn!("something is at {=u8:#04x}", candidate);
            }
        }
        warn!("check SCL=D15/PB6, SDA=D14/PB7, VCC=3V3, GND");
    }

    let interface = I2CDisplayInterface::new(i2c);
    let mut display = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();

    if display.init().is_err() {
        // Nothing more to do without a screen, but keep the logs alive so the
        // failure is visible over RTT rather than looking like a hang.
        defmt::error!("display init failed - check wiring and power");
        loop {
            Timer::after(Duration::from_secs(1)).await;
        }
    }
    info!("display initialised");

    // --- Link to the NodeMCU on LPUART1 ---
    // USART1 is wired to the ST-Link virtual COM port on this board, so the
    // Arduino D0/D1 pins are LPUART1 instead.
    let mut usart_config = usart::Config::default();
    usart_config.baudrate = 115_200;
    let mut uart = match UartRx::new_blocking(p.LPUART1, p.PA3 /* RX - D0 */, usart_config) {
        Ok(uart) => uart,
        Err(_) => {
            defmt::error!("could not configure LPUART1");
            loop {
                Timer::after(Duration::from_secs(1)).await;
            }
        }
    };

    boot_animation(&mut display).await;
    draw_screen(&mut display, "SPELLS", "waiting for wand");
    info!("managing {}, waiting for spells on LPUART1 @115200 (PA3 / D0)", NETWORK);

    // Read a byte at a time and act on each complete line. Displaying is all
    // this board does, so blocking here costs nothing.
    let mut line = [0u8; LINE_MAX];
    let mut len = 0usize;

    loop {
        let mut byte = [0u8; 1];
        if uart.blocking_read(&mut byte).is_err() {
            // A framing or overrun error usually means the baud rates disagree
            // or the wiring is noisy. Drop the partial line and resynchronise.
            warn!("uart read error - discarding partial line");
            len = 0;
            continue;
        }

        match byte[0] {
            b'\n' | b'\r' => {
                if len == 0 {
                    continue; // bare newline, or the \r of a \r\n pair
                }
                let text = core::str::from_utf8(&line[..len]).unwrap_or("");
                let spell = text.trim();

                match action_for(spell) {
                    Some(action) => {
                        info!("cast: {}", spell);
                        draw_screen(&mut display, spell, action);
                    }
                    None => {
                        // Show it anyway: an unknown name means the wand and
                        // this table disagree, which is worth seeing on screen.
                        warn!("unknown spell: {}", spell);
                        draw_screen(&mut display, "?", spell);
                    }
                }
                len = 0;
            }
            b => {
                if len < LINE_MAX {
                    line[len] = b;
                    len += 1;
                } else {
                    // Overlong line: drop it rather than truncate into a wrong
                    // spell name.
                    warn!("line too long - dropping");
                    len = 0;
                }
            }
        }
    }
}
