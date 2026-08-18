//! Gesture data capture for training the spell recogniser.
//!
//! Press the BOOT button, perform one gesture, and this dumps a fixed-length
//! window of motion samples as CSV over serial. Record many repetitions of one
//! spell per session and save the serial output to a file named after it.
//!
//! Equivalent to the `gesture_capture` sketch in the MagicWand project, but the
//! trigger is the button rather than serial commands, so you can keep both hands
//! on the wand.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::i2c::master::{Config, I2c};
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

const MPU6050_ADDR: u8 = 0x68;
const REG_WHO_AM_I: u8 = 0x75;
const REG_PWR_MGMT_1: u8 = 0x6B;
const REG_ACCEL_XOUT_H: u8 = 0x3B;

/// Samples per recorded gesture. Matches the MagicWand window length.
const WINDOW: usize = 128;
/// Milliseconds between samples: 20 ms => ~50 Hz => a 2.56 s window.
const PERIOD_MS: u32 = 20;
/// Printed between gestures so the training script can split the stream.
const SEPARATOR: &str = "-,-,-,-,-,-";

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let delay = Delay::new();

    // BOOT button on GPIO0: pulled up, reads low while held down.
    let button = Input::new(
        peripherals.GPIO0,
        InputConfig::default().with_pull(Pull::Up),
    );

    let mut i2c = I2c::new(peripherals.I2C0, Config::default())
        .expect("Failed to create I2C")
        .with_sda(peripherals.GPIO12)
        .with_scl(peripherals.GPIO13);

    let mut who = [0u8; 1];
    match i2c.write_read(MPU6050_ADDR, &[REG_WHO_AM_I], &mut who) {
        Ok(_) => println!("# sensor OK (WHO_AM_I = {:#04x})", who[0]),
        Err(e) => {
            println!("# ERROR: no answer from sensor: {:?}", e);
            loop {}
        }
    }
    i2c.write(MPU6050_ADDR, &[REG_PWR_MGMT_1, 0x00])
        .expect("Failed to wake up MPU6050");
    delay.delay_millis(100);

    println!("# capture: {} samples @ {} ms (~{} Hz)", WINDOW, PERIOD_MS, 1000 / PERIOD_MS);
    println!("# columns: ax,ay,az,gx,gy,gz (raw counts)");
    println!("# press BOOT, wait for GO, then perform the gesture");

    let mut count = 0u32;

    loop {
        // Wait for a press (button reads low), then for the release, so one
        // press records exactly one gesture.
        while button.is_high() {
            delay.delay_millis(10);
        }
        while button.is_low() {
            delay.delay_millis(10);
        }

        // Brief pause so the press itself is not part of the recording.
        println!("# ready...");
        delay.delay_millis(600);
        println!("# GO");

        println!("{}", SEPARATOR);
        for _ in 0..WINDOW {
            match read_motion(&mut i2c) {
                Some((ax, ay, az, gx, gy, gz)) => {
                    println!("{},{},{},{},{},{}", ax, ay, az, gx, gy, gz)
                }
                // Keep the row count fixed even if a read fails, so every
                // recorded window is exactly WINDOW lines long.
                None => println!("0,0,0,0,0,0"),
            }
            delay.delay_millis(PERIOD_MS);
        }

        count += 1;
        println!("# recorded {} gesture(s)", count);
    }
}

/// Reads accel XYZ and gyro XYZ in one burst. Returns raw signed counts.
fn read_motion(i2c: &mut I2c<'_, esp_hal::Blocking>) -> Option<(i16, i16, i16, i16, i16, i16)> {
    // 14 registers from ACCEL_XOUT_H: AccelXYZ(6) Temp(2) GyroXYZ(6), big-endian.
    let mut buf = [0u8; 14];
    i2c.write_read(MPU6050_ADDR, &[REG_ACCEL_XOUT_H], &mut buf).ok()?;
    let v = |hi: usize| i16::from_be_bytes([buf[hi], buf[hi + 1]]);
    Some((v(0), v(2), v(4), v(8), v(10), v(12)))
}
