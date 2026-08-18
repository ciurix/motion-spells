#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config, I2c};
use esp_hal::Blocking;
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

// The address comes from the bus scan now (AD0 low = 0x68, AD0 high = 0x69).

// MPU6050 register addresses
const REG_WHO_AM_I: u8 = 0x75;
const REG_PWR_MGMT_1: u8 = 0x6B;
const REG_ACCEL_XOUT_H: u8 = 0x3B;

/// Probes every 7-bit address and returns the first device that ACKs.
/// A zero-length write still emits START + address + STOP, so it is a
/// non-destructive way to ask "is anyone there?" without writing registers.
fn scan_bus(i2c: &mut I2c<'_, Blocking>) -> Option<u8> {
    let mut first = None;
    for addr in 0x03..=0x77u8 {
        if i2c.write(addr, &[]).is_ok() {
            println!("  Device found at {:#04x}", addr);
            first.get_or_insert(addr);
        }
    }
    if first.is_none() {
        println!("  No devices found.");
    }
    first
}

fn level_str(high: bool) -> &'static str {
    if high {
        "HIGH"
    } else {
        "LOW "
    }
}

#[esp_hal::main]
fn main() -> ! {
    // Initialize ESP32-S3 peripherals
    let mut peripherals = esp_hal::init(esp_hal::Config::default());

    let delay = Delay::new();

    println!("=== MPU6050 on ESP32-S3 Supermini ===");

    // === Step 0: Electrical line check on SDA/SCL, before any I2C traffic ===
    // The GY-521 module carries ~4.7k pull-up resistors to VCC on both lines.
    // The ESP32's internal pulls are much weaker (~45k), so an external pull-up
    // wins against an internal pull-down. That makes the pull-down test decisive:
    // HIGH means the module is powered AND the wire actually conducts.
    println!("--- Line diagnostics (SDA=GPIO12, SCL=GPIO13) ---");

    let float_cfg = InputConfig::default().with_pull(Pull::None);
    let down_cfg = InputConfig::default().with_pull(Pull::Down);
    let up_cfg = InputConfig::default().with_pull(Pull::Up);

    let (sda_down, scl_down, sda_up, scl_up) = {
        let mut sda = Input::new(peripherals.GPIO12.reborrow(), float_cfg);
        let mut scl = Input::new(peripherals.GPIO13.reborrow(), float_cfg);

        delay.delay_millis(10);
        println!(
            "  floating:      SDA={}  SCL={}",
            level_str(sda.is_high()),
            level_str(scl.is_high())
        );

        // Internal pull-down: an external pull-up on the module overpowers it.
        sda.apply_config(&down_cfg);
        scl.apply_config(&down_cfg);
        delay.delay_millis(10);
        let (sd, cd) = (sda.is_high(), scl.is_high());
        println!("  internal pd:   SDA={}  SCL={}", level_str(sd), level_str(cd));

        // Internal pull-up: LOW here means the line is tied to GND somewhere.
        sda.apply_config(&up_cfg);
        scl.apply_config(&up_cfg);
        delay.delay_millis(10);
        let (su, cu) = (sda.is_high(), scl.is_high());
        println!("  internal pu:   SDA={}  SCL={}", level_str(su), level_str(cu));

        (sd, cd, su, cu)
    };

    // Short test: drive one line low and watch the other (held up internally).
    // If the idle line follows, the two are bridged - e.g. by a solder blob.
    let scl_follows_sda = {
        let _sda_out = Output::new(peripherals.GPIO12.reborrow(), Level::Low, OutputConfig::default());
        let scl_in = Input::new(peripherals.GPIO13.reborrow(), up_cfg);
        delay.delay_millis(10);
        !scl_in.is_high()
    };
    let sda_follows_scl = {
        let _scl_out = Output::new(peripherals.GPIO13.reborrow(), Level::Low, OutputConfig::default());
        let sda_in = Input::new(peripherals.GPIO12.reborrow(), up_cfg);
        delay.delay_millis(10);
        !sda_in.is_high()
    };

    println!("\n--- Diagnosis ---");
    // A line that stays LOW against the internal pull-up is tied to GND by copper.
    // That masks every other test - a grounded line also reads "no external
    // pull-up" and drags its neighbour low - so report it and stop there.
    if !sda_up || !scl_up {
        match (sda_up, scl_up) {
            (false, false) => println!("  SDA and SCL are BOTH stuck LOW against a pull-up."),
            (false, true) => println!("  SDA is stuck LOW against a pull-up."),
            (true, false) => println!("  SCL is stuck LOW against a pull-up."),
            (true, true) => unreachable!(),
        }
        println!("  -> hard short to GND: solder bridging the pads, not a code problem.");
        println!("  (pull-up and short tests below cannot be trusted while a line is grounded)");
    } else {
        if scl_follows_sda || sda_follows_scl {
            println!("  SDA and SCL are SHORTED together (solder bridge between the pads).");
        }
        match (sda_down, scl_down) {
            (true, true) => {
                println!("  Both lines have external pull-ups: module is powered and wired.")
            }
            (false, false) => println!(
                "  No external pull-up on either line -> the module is not powered.\n  \
                 Suspect the VCC or GND joint (both pull-ups are fed from VCC)."
            ),
            (true, false) => println!("  SDA pulled up but SCL is not -> the SCL wire/joint is open."),
            (false, true) => println!("  SCL pulled up but SDA is not -> the SDA wire/joint is open."),
        }
    }
    println!();

    // A grounded line makes I2C impossible, so don't bother initialising the bus.
    // Watch the lines live instead and carry on by itself once they come good -
    // lets you reflow the joints with the board powered and see the fix land.
    if !sda_up || !scl_up {
        println!("Waiting for the short to clear - reflow the joints and watch here.");
        let mut last = (sda_up, scl_up);
        loop {
            let now = {
                let sda = Input::new(peripherals.GPIO12.reborrow(), up_cfg);
                let scl = Input::new(peripherals.GPIO13.reborrow(), up_cfg);
                delay.delay_millis(5);
                (sda.is_high(), scl.is_high())
            };

            if now != last {
                println!(
                    "  SDA={}  SCL={}",
                    level_str(now.0),
                    level_str(now.1)
                );
                last = now;
            }

            if now.0 && now.1 {
                println!("Lines are clear - continuing to I2C.\n");
                break;
            }

            delay.delay_millis(250);
        }
    }

    // === Step 1: Scan the bus, trying both pin orientations ===
    // Swapped SDA/SCL is electrically invisible - both lines carry pull-ups
    // either way - but it makes every address NACK. So probe both ways round
    // rather than assuming the wiring matches the labels.
    println!("Scanning I2C bus (SDA=GPIO12, SCL=GPIO13)...");
    let found = {
        let mut i2c = I2c::new(peripherals.I2C0.reborrow(), Config::default())
            .expect("Failed to create I2C")
            .with_sda(peripherals.GPIO12.reborrow())
            .with_scl(peripherals.GPIO13.reborrow());
        scan_bus(&mut i2c)
    };

    let swapped = if found.is_none() {
        println!("Retrying with the lines swapped (SDA=GPIO13, SCL=GPIO12)...");
        let found_swapped = {
            let mut i2c = I2c::new(peripherals.I2C0.reborrow(), Config::default())
                .expect("Failed to create I2C")
                .with_sda(peripherals.GPIO13.reborrow())
                .with_scl(peripherals.GPIO12.reborrow());
            scan_bus(&mut i2c)
        };
        if found_swapped.is_some() {
            println!("  -> SDA and SCL are swapped! Green/blue wires are the wrong way round.");
        }
        found_swapped
    } else {
        None
    };

    let device_addr = match (found, swapped) {
        (Some(addr), _) | (None, Some(addr)) => addr,
        (None, None) => {
            println!("\nNothing responds either way round - check the module itself.");
            loop {}
        }
    };
    println!("Talking to device at {:#04x}\n", device_addr);

    // Rebuild the bus the way that actually worked.
    let i2c = I2c::new(peripherals.I2C0, Config::default()).expect("Failed to create I2C");
    let mut i2c = if swapped.is_some() {
        i2c.with_sda(peripherals.GPIO13).with_scl(peripherals.GPIO12)
    } else {
        i2c.with_sda(peripherals.GPIO12).with_scl(peripherals.GPIO13)
    };

    // === Step 1: Verify MPU6050 is connected (WHO_AM_I register) ===
    let mut who_am_i = [0u8; 1];
    match i2c.write_read(device_addr, &[REG_WHO_AM_I], &mut who_am_i) {
        Ok(_) => {
            println!("WHO_AM_I register: {:#04x}", who_am_i[0]);
            if who_am_i[0] == 0x68 {
                println!("MPU6050 detected successfully!");
            } else {
                println!("WARNING: Unexpected WHO_AM_I value (expected 0x68)");
            }
        }
        Err(e) => {
            println!("ERROR: Failed to read WHO_AM_I: {:?}", e);
            println!("Check wiring: SDA=GPIO12, SCL=GPIO13, VCC=3.3V, GND=GND");
            loop {}
        }
    }

    // === Step 2: Wake up MPU6050 (it starts in sleep mode) ===
    match i2c.write(device_addr, &[REG_PWR_MGMT_1, 0x00]) {
        Ok(_) => println!("MPU6050 woken up from sleep mode"),
        Err(e) => {
            println!("ERROR: Failed to wake up MPU6050: {:?}", e);
            loop {}
        }
    }

    delay.delay_millis(100); // Wait for sensor to stabilize

    println!("Starting sensor readings...\n");

    // === Step 3: Read sensor data in a loop ===
    loop {
        // Read 14 bytes starting from ACCEL_XOUT_H (0x3B)
        // Layout: AccelX(2) AccelY(2) AccelZ(2) Temp(2) GyroX(2) GyroY(2) GyroZ(2)
        let mut buf = [0u8; 14];
        match i2c.write_read(device_addr, &[REG_ACCEL_XOUT_H], &mut buf) {
            Ok(_) => {
                // Parse accelerometer data (raw 16-bit signed values)
                let accel_x = i16::from_be_bytes([buf[0], buf[1]]);
                let accel_y = i16::from_be_bytes([buf[2], buf[3]]);
                let accel_z = i16::from_be_bytes([buf[4], buf[5]]);

                // Parse temperature (raw value)
                let temp_raw = i16::from_be_bytes([buf[6], buf[7]]);
                // Temperature in degrees C = (temp_raw / 340.0) + 36.53
                // Using integer math: temp_c_x100 = (temp_raw * 100) / 340 + 3653
                let temp_c_x100 = (temp_raw as i32 * 100) / 340 + 3653;

                // Parse gyroscope data (raw 16-bit signed values)
                let gyro_x = i16::from_be_bytes([buf[8], buf[9]]);
                let gyro_y = i16::from_be_bytes([buf[10], buf[11]]);
                let gyro_z = i16::from_be_bytes([buf[12], buf[13]]);

                println!("--- MPU6050 Readings ---");
                println!(
                    "Accel: X={:>6}  Y={:>6}  Z={:>6}  (raw)",
                    accel_x, accel_y, accel_z
                );
                println!(
                    "Gyro:  X={:>6}  Y={:>6}  Z={:>6}  (raw)",
                    gyro_x, gyro_y, gyro_z
                );
                println!("Temp:  {}.{:02} °C", temp_c_x100 / 100, (temp_c_x100 % 100).unsigned_abs());
                println!();
            }
            Err(e) => {
                println!("I2C read error: {:?}", e);
            }
        }

        // Read every 500ms
        delay.delay_millis(500);
    }
}
