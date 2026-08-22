//! Controller: STM32U545RE-Q driving an SSD1306 OLED over I2C.
//!
//! Wiring, to the Arduino header on the Nucleo:
//!   SCL -> D15 (PB6)    SDA -> D14 (PB7)    VCC -> 3V3    GND -> GND
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
use embassy_time::{Duration, Timer};
use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, ascii::FONT_9X18_BOLD, MonoTextStyleBuilder},
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use ssd1306::{prelude::*, I2CDisplayInterface, Ssd1306};

use defmt_rtt as _;
use panic_probe as _;

/// SSD1306 modules are strapped to 0x3C almost always, 0x3D occasionally.
const CANDIDATE_ADDRS: [u8; 2] = [0x3C, 0x3D];

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
    let mut addr = None;
    for candidate in CANDIDATE_ADDRS {
        if i2c.blocking_write(candidate, &[]).is_ok() {
            info!("display responded at {=u8:#04x}", candidate);
            addr = Some(candidate);
            break;
        }
    }
    if addr.is_none() {
        warn!("no display found at 0x3C or 0x3D - scanning the whole bus");
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

    match display.init() {
        Ok(()) => info!("display initialised"),
        Err(_) => {
            // Nothing more to do without a screen, but keep the logs alive so
            // the failure is visible over RTT rather than looking like a hang.
            defmt::error!("display init failed - check wiring and power");
            loop {
                Timer::after(Duration::from_secs(1)).await;
            }
        }
    }

    let title_style = MonoTextStyleBuilder::new()
        .font(&FONT_9X18_BOLD)
        .text_color(BinaryColor::On)
        .build();
    let body_style = MonoTextStyleBuilder::new()
        .font(&FONT_6X10)
        .text_color(BinaryColor::On)
        .build();

    // Splash screen, so it is obvious at a glance that the panel is alive.
    display.clear(BinaryColor::Off).ok();
    Text::with_baseline("SPELLS", Point::new(28, 8), title_style, Baseline::Top)
        .draw(&mut display)
        .ok();
    Rectangle::new(Point::new(0, 30), Size::new(128, 1))
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(&mut display)
        .ok();
    Text::with_baseline(
        "waiting for wand",
        Point::new(10, 38),
        body_style,
        Baseline::Top,
    )
    .draw(&mut display)
    .ok();
    display.flush().ok();

    info!("splash drawn");
    Timer::after(Duration::from_secs(2)).await;

    // Until the wand link exists, cycle through the spells so the display has
    // something to show and the render path gets exercised.
    let spells: [(&str, &str); 6] = [
        ("LEFT", "prev interface"),
        ("RIGHT", "next interface"),
        ("UP", "interface up"),
        ("DOWN", "interface down"),
        ("PUSH", "iperf3 flood"),
        ("CIRCULAR", "backup config"),
    ];

    let mut i = 0usize;
    loop {
        let (name, action) = spells[i % spells.len()];
        i += 1;

        display.clear(BinaryColor::Off).ok();
        Text::with_baseline(name, Point::new(4, 6), title_style, Baseline::Top)
            .draw(&mut display)
            .ok();
        Rectangle::new(Point::new(0, 28), Size::new(128, 1))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(&mut display)
            .ok();
        Text::with_baseline(action, Point::new(4, 36), body_style, Baseline::Top)
            .draw(&mut display)
            .ok();
        display.flush().ok();

        info!("showing {}", name);
        Timer::after(Duration::from_millis(1500)).await;
    }
}
