//! Gesture data capture for training the spell recogniser.
//!
//! Press the BOOT button, perform one gesture, and this dumps a recording of
//! motion samples as CSV over serial. Record many repetitions of one spell per
//! session and save the serial output to a file named after it.
//!
//! The recording is deliberately longer than the window the model sees. A fast
//! swipe is over in a few hundred milliseconds - far quicker than anyone can
//! time against a cue - so the extra length is slack for reaction time, and the
//! training script crops the busiest part out of each recording.
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
use esp_hal::time::{Duration, Instant};
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

const MPU6050_ADDR: u8 = 0x68;
const REG_WHO_AM_I: u8 = 0x75;
const REG_PWR_MGMT_1: u8 = 0x6B;
const REG_CONFIG: u8 = 0x1A;
const REG_GYRO_CONFIG: u8 = 0x1B;
const REG_ACCEL_CONFIG: u8 = 0x1C;
const REG_ACCEL_XOUT_H: u8 = 0x3B;

/// Samples per recording, and the gap between them: 150 at 10 ms is 1.5 s at
/// 100 Hz. The model is trained on a 0.9 s window cropped out of this.
const CAPTURE: usize = 150;
const PERIOD_MS: u64 = 10;

/// Full-scale ranges - deliberately not the defaults. A hard swipe passes
/// +-2 g easily, and gravity has already spent one of those two; a wrist flick
/// goes well past 250 deg/s. A clipped reading is flat-topped and looks the
/// same whichever way the wand was moving, so the default ranges throw away
/// exactly the part of a fast gesture that says which one it was.
const ACCEL_CONFIG_8G: u8 = 0x10; // AFS_SEL = 2 -> +-8 g, 4096 LSB/g
const GYRO_CONFIG_1000DPS: u8 = 0x10; // FS_SEL = 2 -> +-1000 deg/s, 32.8 LSB/deg/s

/// Digital low-pass at 44 Hz (accel) / 42 Hz (gyro). Sampling at 100 Hz, the
/// sensor's default 260 Hz bandwidth would fold everything above 50 Hz back
/// into the band the model looks at.
const DLPF_44HZ: u8 = 0x03;

/// A reading this large has run out of range, whatever the true value was.
const CLIP_LEVEL: i16 = 32_000;

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

    for (reg, value) in [
        (REG_ACCEL_CONFIG, ACCEL_CONFIG_8G),
        (REG_GYRO_CONFIG, GYRO_CONFIG_1000DPS),
        (REG_CONFIG, DLPF_44HZ),
    ] {
        i2c.write(MPU6050_ADDR, &[reg, value])
            .expect("Failed to configure MPU6050");
    }

    println!(
        "# capture: {} samples @ {} ms ({} Hz)",
        CAPTURE,
        PERIOD_MS,
        1000 / PERIOD_MS
    );
    println!("# ranges: accel +-8 g, gyro +-1000 deg/s, DLPF 44 Hz");
    println!("# columns: ax,ay,az,gx,gy,gz (raw counts)");
    println!("# press BOOT, wait for GO, then perform the gesture");

    let period = Duration::from_millis(PERIOD_MS);
    let mut samples = [[0i16; 6]; CAPTURE];
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

        // Sample into RAM and print afterwards. Printing inside the loop would
        // put the serial port in the timing path: a host that stopped reading
        // for a moment would stretch the sample interval, and nothing in the
        // resulting file would show that it had happened.
        let mut deadline = Instant::now();
        for slot in samples.iter_mut() {
            *slot = read_motion(&mut i2c).unwrap_or([0; 6]);
            deadline += period;
            while Instant::now() < deadline {}
        }

        println!("{}", SEPARATOR);
        let mut clipped = 0u32;
        for s in samples.iter() {
            println!("{},{},{},{},{},{}", s[0], s[1], s[2], s[3], s[4], s[5]);
            if s.iter().any(|v| *v >= CLIP_LEVEL || *v <= -CLIP_LEVEL) {
                clipped += 1;
            }
        }

        count += 1;
        if clipped > 0 {
            println!(
                "# WARNING: {} of {} samples ran out of range - swing slightly softer",
                clipped, CAPTURE
            );
        }
        println!("# recorded {} gesture(s)", count);
    }
}

/// Reads accel XYZ and gyro XYZ in one burst. Returns raw signed counts.
fn read_motion(i2c: &mut I2c<'_, esp_hal::Blocking>) -> Option<[i16; 6]> {
    // 14 registers from ACCEL_XOUT_H: AccelXYZ(6) Temp(2) GyroXYZ(6), big-endian.
    let mut buf = [0u8; 14];
    i2c.write_read(MPU6050_ADDR, &[REG_ACCEL_XOUT_H], &mut buf).ok()?;
    let v = |hi: usize| i16::from_be_bytes([buf[hi], buf[hi + 1]]);
    Some([v(0), v(2), v(4), v(8), v(10), v(12)])
}
