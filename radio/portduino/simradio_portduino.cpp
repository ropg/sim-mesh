/**
 * simradio for a Portduino firmware: the SX1262 on SPI and its four lines,
 * and the firmware's idle wait, on the simulated chip.
 *
 * A firmware built on Portduino (Meshtastic's Arduino API for Linux) links
 * this library and calls two hooks, each a weak function of its own whose
 * strong definition is here:
 *
 *   native_radio_backend_init()   once, before its SPI.begin() and before it
 *                                 touches the radio's pins
 *   rnode_idle(max_ms)            where it has nothing to do for up to max_ms
 *   rnode_wake()                  from any thread with work for the loop (input
 *                                 arrived): ends the idle now, or the next one
 *
 * The init opens the station's link to the ether and chip slot 0, installs an
 * SPIChip into the global SPI that hands each transfer to the chip whole, and
 * binds the chip's pins with gpioBind: NSS (nothing to do, every transfer is
 * already one frame), RESET (its rising edge resets the chip), BUSY and DIO1
 * (the levels the chip drives; a rise of DIO1 is never lost between two of
 * Portduino's reads of it). Binding a pin also turns off Portduino's
 * 100 ms sleep between loop() calls, so the firmware's own idle is the only
 * wait in its loop.
 *
 * The idle (idle.cpp) ends when max_ms has passed or DIO1 has risen, which
 * wakes it; the time shim answers the wait in node time, and Portduino's next
 * gpioIdle() runs the DIO1 interrupt handler.
 *
 * Environment: SIM_MESH_NODE_ID, SIM_MESH_BIND_ADDR and SIM_MESH_ETHER (the
 * station contract), and the pin numbers the firmware drives the chip on:
 * SIMRADIO_PIN_NSS, SIMRADIO_PIN_RESET, SIMRADIO_PIN_BUSY, SIMRADIO_PIN_DIO1
 * (1, 2, 3 and 4 when unset).
 */
#include <Arduino.h>
#include <PortduinoGPIO.h>
#include <SPIChip.h>

// By its path: a firmware may have a simradio.h of its own on the include path (Meshtastic does).
#include "../include/simradio.h"

#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <pthread.h>
#include <sys/time.h>
#include <vector>

void rnode_wake();

namespace {

simradio_t* s_chip;

pthread_mutex_t s_mu = PTHREAD_MUTEX_INITIALIZER;
unsigned s_rises;               /* DIO1's rising edges, ever */
volatile int s_dio1;
volatile int s_busy;

void onPin(void*, int pin, int level)
{
    if (pin == SIMRADIO_PIN_BUSY) {
        s_busy = level;
        return;
    }
    if (pin != SIMRADIO_PIN_DIO1)
        return;
    pthread_mutex_lock(&s_mu);
    bool rose = level && !s_dio1;
    if (rose)
        s_rises++;
    s_dio1 = level;
    pthread_mutex_unlock(&s_mu);
    if (rose)
        rnode_wake();
}

int envInt(const char* name, int fallback)
{
    const char* v = getenv(name);
    return v && *v ? atoi(v) : fallback;
}

/* Each transfer is one frame, NSS low to NSS high. The driver transfers in
 * place, so the frame going out is copied before the reply overwrites it. */
class SimChip : public SPIChip {
public:
    int transfer(const uint8_t* out, uint8_t* in, size_t len, bool) override
    {
        out_.assign(len, 0);
        if (out)
            memcpy(out_.data(), out, len);
        in_.assign(len, 0);
        simradio_transfer(s_chip, out_.data(), len, in_.data());
        if (in)
            memcpy(in, in_.data(), len);
        return 0;
    }

private:
    std::vector<uint8_t> out_, in_;
};

/* HardwareSPI keeps its chip protected; a pointer to the member, taken
 * through a derived class, reaches it on the global SPI. */
struct SpiAccess : HardwareSPI {
    static void install(HardwareSPI& spi, std::shared_ptr<SPIChip> chip)
    {
        spi.*(&SpiAccess::spiChip) = std::move(chip);
    }
};

class NssPin : public GPIOPin {
public:
    NssPin(pin_size_t n) : GPIOPin(n, "NSS") { setSilent(); }
};

class ResetPin : public GPIOPin {
public:
    ResetPin(pin_size_t n) : GPIOPin(n, "RESET") { setSilent(); }
    void writePin(PinStatus s) override
    {
        bool rising = s == HIGH && level_ == LOW;
        level_ = s;
        GPIOPin::writePin(s);
        if (rising)
            simradio_reset(s_chip);
    }

private:
    PinStatus level_ = HIGH;
};

class LevelPin : public GPIOPin {
public:
    LevelPin(pin_size_t n, const char* name, volatile int* level)
        : GPIOPin(n, name), level_(level) { setSilent(); }

protected:
    PinStatus readPinHardware() override { return *level_ ? HIGH : LOW; }

private:
    volatile int* level_;
};

/* DIO1, whose edges Portduino finds by reading the level once a loop
 * (gpioIdle): a firmware that idles between loops can miss the line going
 * low and high again, and its interrupt with it. A rise since the last read
 * is reported as an edge: from a read that saw it high, the line reads low
 * once and the idle is ended, so the next loop's read sees it rise. */
class Dio1Pin : public GPIOPin {
public:
    Dio1Pin(pin_size_t n) : GPIOPin(n, "DIO1") { setSilent(); }

protected:
    PinStatus readPinHardware() override
    {
        pthread_mutex_lock(&s_mu);
        unsigned rises = s_rises;
        int level = s_dio1;
        pthread_mutex_unlock(&s_mu);
        if (rises != seen_) {
            if (last_ == HIGH) {
                last_ = LOW;
                rnode_wake();
                return LOW;
            }
            seen_ = rises;
            last_ = HIGH;
            return HIGH;
        }
        last_ = level ? HIGH : LOW;
        return last_;
    }

private:
    unsigned seen_ = 0;
    PinStatus last_ = LOW;
};

}  // namespace

void native_radio_backend_init()
{
    int sid = envInt("SIM_MESH_NODE_ID", 0);
    const char* bind = getenv("SIM_MESH_BIND_ADDR");
    const char* ether = getenv("SIM_MESH_ETHER");
    if (simradio_station_open(sid, bind ? bind : "127.0.0.1", ether ? ether : "") != 0) {
        fprintf(stderr, "[simradio] could not open the link to the ether at %s\n",
                ether ? ether : "(none)");
        exit(1);
    }
    s_chip = simradio_open(0, onPin, nullptr);
    if (!s_chip) {
        fprintf(stderr, "[simradio] could not open chip slot 0\n");
        exit(1);
    }
    s_busy = simradio_pin(s_chip, SIMRADIO_PIN_BUSY);
    s_dio1 = simradio_pin(s_chip, SIMRADIO_PIN_DIO1);

    SpiAccess::install(SPI, std::make_shared<SimChip>());
    gpioBind(new NssPin(envInt("SIMRADIO_PIN_NSS", 1)));
    gpioBind(new ResetPin(envInt("SIMRADIO_PIN_RESET", 2)));
    gpioBind(new LevelPin(envInt("SIMRADIO_PIN_BUSY", 3), "BUSY", &s_busy));
    gpioBind(new Dio1Pin(envInt("SIMRADIO_PIN_DIO1", 4)));
}
