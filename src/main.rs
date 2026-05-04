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

#[esp_hal::main]
fn main() -> ! {
    // Initialize ESP32-S3 peripherals
    let peripherals = esp_hal::init(esp_hal::Config::default());

    println!("=== MPU6050 on ESP32-S3 Supermini ===");
    println!("Initializing I2C on SDA=GPIO12, SCL=GPIO13...");

    // Create I2C instance on I2C0 peripheral
    // Wiring: SDA -> GPIO12, SCL -> GPIO13, VCC -> 3.3V, GND -> GND
    let mut i2c = I2c::new(peripherals.I2C0, Config::default())
        .expect("Failed to create I2C")
        .with_sda(peripherals.GPIO12)
        .with_scl(peripherals.GPIO13);

    let delay = Delay::new();

    // === Step 1: Verify MPU6050 is connected (WHO_AM_I register) ===
    let mut who_am_i = [0u8; 1];
    match i2c.write_read(MPU6050_ADDR, &[REG_WHO_AM_I], &mut who_am_i) {
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
    match i2c.write(MPU6050_ADDR, &[REG_PWR_MGMT_1, 0x00]) {
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
        match i2c.write_read(MPU6050_ADDR, &[REG_ACCEL_XOUT_H], &mut buf) {
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
