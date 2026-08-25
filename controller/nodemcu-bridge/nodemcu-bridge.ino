// Radio bridge: NodeMCU (ESP8266) between the wand and the STM32 controller.
//
// The wand broadcasts a spell name over ESP-NOW. This receives it and repeats
// it as a line of text on UART1, which the STM32 reads.
//
// This is the one part of the project that is not Rust. The ESP8266's WiFi is
// a closed-source blob usable only from Espressif's C SDK; the Rust driver for
// it (esp-wifi) supports the ESP32 family only, so ESP-NOW here has to be C++.
//
// Wiring:
//   NodeMCU D4 (GPIO2, UART1 TX) -> STM32 D0 (PA3, LPUART1 RX)
//   NodeMCU GND                  -> STM32 GND
//
// Only one data wire is needed: the STM32 never talks back. Both boards are
// 3.3V, so no level shifting.
//
// UART1 is TX-only on the ESP8266, which suits a one-way link and leaves
// Serial (UART0, on the USB port) free for debugging.

#include <ESP8266WiFi.h>
extern "C" {
#include <espnow.h>
}

// Must match the channel the wand transmits on. An ESP8266 in station mode
// that has not joined a network sits on channel 1.
static const uint8_t ESPNOW_CHANNEL = 1;

// Longest spell name we will pass on; anything larger is a malformed packet.
static const size_t MAX_SPELL = 32;

// The wand broadcasts each spell three times, because an ESP-NOW broadcast is
// never acknowledged or retried and a single frame lost to a collision would be
// lost silently. Those copies must not become three casts downstream - LEFT
// would step three interfaces, and the router manager would run the command
// three times.
//
// Identical spells arriving inside this window are therefore treated as the
// sender's repeats. That is safe because the wand cannot legitimately repeat a
// spell any faster than its own cooldown, which is about 1.2 seconds - comfort-
// ably longer than this - while its three copies land within about 30 ms.
static const unsigned long DEDUP_MS = 400;

static char lastSpell[MAX_SPELL + 1] = "";
static unsigned long lastSpellAt = 0;

void onSpellReceived(uint8_t *mac, uint8_t *data, uint8_t len) {
  if (len == 0 || len > MAX_SPELL) {
    Serial.printf("ignoring %u byte packet\n", len);
    return;
  }

  // Copy out and strip anything that is not printable, so a corrupted packet
  // cannot inject stray control characters into the STM32's line parser.
  char spell[MAX_SPELL + 1];
  size_t n = 0;
  for (size_t i = 0; i < len; i++) {
    char c = (char)data[i];
    if (c >= 32 && c < 127) {
      spell[n++] = c;
    }
  }
  spell[n] = '\0';
  if (n == 0) {
    return;
  }

  // Unsigned subtraction, so this stays correct across the millis() rollover.
  unsigned long now = millis();
  if (strcmp(spell, lastSpell) == 0 && (now - lastSpellAt) < DEDUP_MS) {
    // A repeat of the one just handled. Logged, not forwarded: seeing these on
    // the console is how you know the retries are arriving at all.
    Serial.printf("  (repeat of %s, ignored)\n", spell);
    return;
  }
  strncpy(lastSpell, spell, MAX_SPELL);
  lastSpell[MAX_SPELL] = '\0';
  lastSpellAt = now;

  // To the STM32, and to the USB console so it can be watched while testing.
  Serial1.print(spell);
  Serial1.print('\n');

  Serial.printf("%02X:%02X:%02X:%02X:%02X:%02X -> %s\n", mac[0], mac[1], mac[2],
                mac[3], mac[4], mac[5], spell);
}

void setup() {
  Serial.begin(115200);   // USB, for debugging
  Serial1.begin(115200);  // D4 (GPIO2) -> STM32
  delay(100);

  Serial.println();
  Serial.println("nodemcu esp-now bridge");

  // ESP-NOW needs the radio up but no access point association.
  WiFi.mode(WIFI_STA);
  WiFi.disconnect();
  wifi_set_channel(ESPNOW_CHANNEL);

  Serial.print("mac: ");
  Serial.println(WiFi.macAddress());
  Serial.printf("channel: %u\n", ESPNOW_CHANNEL);

  if (esp_now_init() != 0) {
    Serial.println("esp_now_init failed - resetting");
    delay(1000);
    ESP.restart();
  }

  // SLAVE means receive-only. The wand broadcasts, so no peer needs adding
  // here - and broadcasting saves having to hard-code MAC addresses.
  esp_now_set_self_role(ESP_NOW_ROLE_SLAVE);
  esp_now_register_recv_cb(onSpellReceived);

  Serial.println("listening for spells");
}

void loop() {
  // Everything happens in the receive callback.
  delay(1000);
}
