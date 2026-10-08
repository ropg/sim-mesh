/*
 * A stand-in station for the time shim's tests that retunes as reticulum's
 * SX1262 driver does: one chip, each command one whole SPI frame, and before
 * each, BUSY waited out in 10 µs sleeps (the driver's wait_busy, through
 * sim-mesh-hal's StdDelay). A switch is the driver's configure and
 * start_receive: SetStandby, CalibrateImage, SetModulationParams,
 * SetPacketParams, SetRfFrequency, SetTxParams, then SetPacketParams,
 * ClearIrqStatus and SetRx. `stock` goes to STDBY_RC and calibrates the image
 * every time; `xosc` (its fast-retune) stays on the crystal and calibrates
 * only at power-up. The channel alternates between two in the band and the
 * spreading factor between 7 and 8. It prints, in node time:
 *
 *     switch <start> <listening>   SetStandby sent; BUSY seen low after SetRx
 *     done                         the last switch is over
 *
 * usage: retuner <ether host:port> <stock|xosc> <switches>
 */
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "simradio.h"

#define FREQ 869525000u

static simradio_t* chip;

static int64_t mono_us(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000 + ts.tv_nsec / 1000;
}

static void wait_busy(void)
{
    while (simradio_pin(chip, SIMRADIO_PIN_BUSY)) {
        struct timespec ts = { 0, 10 * 1000 };
        nanosleep(&ts, NULL);
    }
}

static void command(const uint8_t* out, size_t len)
{
    uint8_t in[16];
    wait_busy();
    simradio_transfer(chip, out, len, in);
}

#define CMD(...) do { const uint8_t f_[] = { __VA_ARGS__ }; command(f_, sizeof f_); } while (0)

static void on_pin(void* ctx, int pin, int level)
{
    (void)ctx;
    (void)pin;
    (void)level;
}

static void configure(int xosc, uint32_t freq, uint8_t sf)
{
    uint32_t frf = (uint32_t)(((uint64_t)freq << 25) / 32000000u);
    CMD(0x80, xosc ? 0x01 : 0x00);                          /* SetStandby */
    if (!xosc) CMD(0x98, 0xD7, 0xDB);                       /* CalibrateImage, 863-870 MHz */
    CMD(0x8B, sf, 0x04, 0x01, 0x00);                        /* SetModulationParams */
    CMD(0x8C, 0x00, 0x12, 0x00, 0xFF, 0x01, 0x00);          /* SetPacketParams */
    CMD(0x86, (uint8_t)(frf >> 24), (uint8_t)(frf >> 16),
        (uint8_t)(frf >> 8), (uint8_t)frf);                 /* SetRfFrequency */
    CMD(0x8E, 0x16, 0x04);                                  /* SetTxParams */
}

static void start_receive(void)
{
    CMD(0x8C, 0x00, 0x12, 0x00, 0xFF, 0x01, 0x00);          /* SetPacketParams */
    CMD(0x02, 0x43, 0xFF);                                  /* ClearIrqStatus */
    CMD(0x82, 0xFF, 0xFF, 0xFF);                            /* SetRx, continuous */
}

int main(int argc, char** argv)
{
    if (argc < 4) return 2;
    int xosc = strcmp(argv[2], "xosc") == 0;
    int switches = atoi(argv[3]);
    if (simradio_station_open(3, "127.0.0.1", argv[1]) != 0) return 1;
    chip = simradio_open(0, on_pin, NULL);
    if (!chip) return 1;
    CMD(0x97, 0x02, 0x00, 0x01, 0x40);                      /* SetDIO3AsTCXOCtrl, 5 ms */
    configure(0, FREQ, 7);                                  /* power-up, the whole way */
    start_receive();
    for (int i = 0; i < switches; i++) {
        wait_busy();
        int64_t start = mono_us();
        configure(xosc, i % 2 ? FREQ : FREQ + 200000, i % 2 ? 7 : 8);
        start_receive();
        wait_busy();
        printf("switch %lld %lld\n", (long long)start, (long long)mono_us());
        fflush(stdout);
    }
    printf("done\n");
    fflush(stdout);
    int never[2];
    if (pipe(never) != 0) return 1;
    char c;
    while (read(never[0], &c, 1) < 0 && errno == EINTR) {}
    return 0;
}
