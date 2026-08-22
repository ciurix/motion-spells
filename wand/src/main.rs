#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::delay::Delay;
use esp_hal::i2c::master::{Config, I2c};
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

// MPU6050 I2C address (AD0 pin low = 0x68, AD0 pin high = 0x69)
const MPU6050_ADDR: u8 = 0x68;

// MPU6050 register addresses
const REG_WHO_AM_I: u8 = 0x75;
const REG_PWR_MGMT_1: u8 = 0x6B;
const REG_ACCEL_XOUT_H: u8 = 0x3B;

// --- Gesture thresholds, in raw sensor counts (1 g = 16384, 1 deg/s = 131) ---
const TILT: i16 = 6000; // ~0.37 g on an axis => board tilted ~22 deg that way
const FLAT: i16 = 3500; // both tilt axes below this => board is lying flat
const GYRO_STILL: i32 = 3000; // |gz| below this => not spinning
const GYRO_SPIN: i32 = 14000; // sustained Z rotation this fast => a spin gesture
const SPIN_HOLD: u8 = 3; // spin must last this many samples (rejects a flick)
const PUSH_MAG2: i64 = 700_000_000; // total accel^2 above this => a sharp thrust
const CALM_MAG2: i64 = 500_000_000; // total accel^2 below this => roughly 1 g, calm

/// The six spells. Each one maps a physical motion to a router action.
#[derive(Clone, Copy, PartialEq)]
enum Spell {
    Left,
    Right,
    Up,
    Down,
    Push,
    Circular,
}

impl Spell {
    /// What this spell will do (printed to the console for now).
    fn describe(self) -> (&'static str, &'static str) {
        match self {
            Spell::Left => ("LEFT", "cycle to previous interface"),
            Spell::Right => ("RIGHT", "cycle to next interface"),
            Spell::Up => ("UP", "turn on interface"),
            Spell::Down => ("DOWN", "shutdown interface"),
            Spell::Push => ("PUSH", "stress test interface (iperf3 UDP flood)"),
            Spell::Circular => ("CIRCULAR", "backup router config"),
        }
    }
}

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let delay = Delay::new();

    println!("=== Motion Spells (MPU6050 on ESP32-S3) ===");

    // Wiring: SDA -> GPIO12, SCL -> GPIO13, VCC -> 3.3V, GND -> GND
    let mut i2c = I2c::new(peripherals.I2C0, Config::default())
        .expect("Failed to create I2C")
        .with_sda(peripherals.GPIO12)
        .with_scl(peripherals.GPIO13);

    // Check we are talking to the right chip: WHO_AM_I always reads 0x68.
    let mut who_am_i = [0u8; 1];
    match i2c.write_read(MPU6050_ADDR, &[REG_WHO_AM_I], &mut who_am_i) {
        Ok(_) => println!("Sensor OK (WHO_AM_I = {:#04x})", who_am_i[0]),
        Err(e) => {
            println!("ERROR: no answer from the sensor: {:?}", e);
            loop {}
        }
    }

    // The MPU6050 powers up asleep, so clear the sleep bit in PWR_MGMT_1.
    i2c.write(MPU6050_ADDR, &[REG_PWR_MGMT_1, 0x00])
        .expect("Failed to wake up MPU6050");
    delay.delay_millis(100);

    // Learn the resting orientation. The board rarely sits perfectly level, so
    // we average a moment of stillness and measure every later tilt RELATIVE to
    // this reference. Without it, a board resting at an angle never reads "flat".
    println!("Calibrating - hold the board still in its resting position...");
    let (mut sx, mut sy) = (0i32, 0i32);
    let mut n = 0i32;
    for _ in 0..100 {
        if let Some((ax, ay, ..)) = read_motion(&mut i2c) {
            sx += ax as i32;
            sy += ay as i32;
            n += 1;
        }
        delay.delay_millis(5);
    }
    let (rest_x, rest_y) = if n > 0 { (sx / n, sy / n) } else { (0, 0) };
    println!("Rest reference: X={} Y={}", rest_x, rest_y);

    println!("Ready. Cast a spell:");
    println!("  tilt left/right/up/down, thrust forward (push), or spin it flat (circular).\n");

    // A gesture only fires when the board is "armed" (was resting calm and flat).
    // After each spell we disarm and wait for it to settle before the next one,
    // so one motion casts exactly one spell.
    let mut armed = true;
    let mut spin_count: u8 = 0;
    let mut dbg: u8 = 0;

    loop {
        let (ax, ay, az, gx, gy, gz) = match read_motion(&mut i2c) {
            Some(v) => v,
            None => {
                delay.delay_millis(50);
                continue;
            }
        };

        // Tilt measured RELATIVE to the resting orientation, so how the board
        // happens to sit doesn't matter - only how far you tip it from rest.
        let dx = (ax as i32 - rest_x) as i16;
        let dy = (ay as i32 - rest_y) as i16;
        // Spin = rotation about the vertical (Z) axis only. A tilt rotates about
        // X or Y instead, so keying on Z keeps a fast tilt from reading as a spin.
        let _ = (gx, gy);
        let spin = (gz as i32).abs();
        let mag2 = (ax as i64).pow(2) + (ay as i64).pow(2) + (az as i64).pow(2);

        // TEMP DEBUG: print the live values a few times per second so we can
        // see whether gestures approach the thresholds. Remove once tuned.
        dbg = dbg.wrapping_add(1);
        if dbg % 6 == 0 {
            println!(
                "dbg armed={} dx={:>6} dy={:>6} spin={:>6} mag2/1e6={}",
                armed as u8,
                dx,
                dy,
                spin,
                mag2 / 1_000_000
            );
        }

        if armed {
            // Track how long a fast rotation has lasted (for the spin gesture).
            if spin > GYRO_SPIN {
                spin_count += 1;
            } else {
                spin_count = 0;
            }

            // Priority: a sustained spin, then a sharp thrust, then a plain tilt.
            // Axis mapping matches how the board sits in the hand: its X axis is
            // your left/right, its Y axis is your up/down.
            let spell = if spin_count >= SPIN_HOLD {
                Some(Spell::Circular)
            } else if mag2 > PUSH_MAG2 {
                Some(Spell::Push)
            } else if dx > TILT {
                Some(Spell::Right)
            } else if dx < -TILT {
                Some(Spell::Left)
            } else if dy > TILT {
                Some(Spell::Down)
            } else if dy < -TILT {
                Some(Spell::Up)
            } else {
                None
            };

            if let Some(spell) = spell {
                let (name, action) = spell.describe();
                println!(">>> {:<9}-> {}", name, action);
                armed = false;
                spin_count = 0;
            }
        } else {
            // Re-arm once the board is back near rest, still, and not accelerating.
            let flat = dx.abs() < FLAT && dy.abs() < FLAT;
            if flat && spin < GYRO_STILL && mag2 < CALM_MAG2 {
                armed = true;
            }
        }

        delay.delay_millis(50);
    }
}

/// Reads accel XYZ and gyro XYZ in one burst. Returns raw signed counts.
fn read_motion(i2c: &mut I2c<'_, esp_hal::Blocking>) -> Option<(i16, i16, i16, i16, i16, i16)> {
    // 14 registers from ACCEL_XOUT_H: AccelXYZ(6) Temp(2) GyroXYZ(6), big-endian.
    let mut buf = [0u8; 14];
    i2c.write_read(MPU6050_ADDR, &[REG_ACCEL_XOUT_H], &mut buf).ok()?;
    let value = |hi: usize| i16::from_be_bytes([buf[hi], buf[hi + 1]]);
    Some((
        value(0),  // accel X
        value(2),  // accel Y
        value(4),  // accel Z
        value(8),  // gyro X
        value(10), // gyro Y
        value(12), // gyro Z
    ))
}
