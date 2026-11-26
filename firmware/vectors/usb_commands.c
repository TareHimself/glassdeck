#include "fw_protocol.h"
#include "fw_refinement.h"
#include "fw_usb_cmd.h"
#include "lfs_store.h"

#include <stdio.h>
#include <string.h>

int fw_lfs_host_format_and_mount(void);

enum { P = FW_USB_PACKET_SIZE };

static int first_packet;

static void print_packet(const uint8_t *data, size_t len) {
    printf("%s", first_packet ? "" : "|");
    first_packet = 0;
    if (len == 0)
        printf("-");
    for (size_t i = 0; i < len; i++)
        printf("%02x", data[i]);
}

static int capture(const uint8_t *pkt, size_t len, void *ctx) {
    (void)ctx;
    print_packet(pkt, len);
    return 0;
}

static fw_runtime_t rt;
static fw_usb_rx_t rx;

/* Feeds packets[0..count) and prints what went in and what came out. A packet's length is
 * lens[i]; packets shorter than P stand for short USB packets. */
static void run(const char *name, const uint8_t (*packets)[P], const size_t *lens, size_t count) {
    printf("%s.request ", name);
    first_packet = 1;
    for (size_t i = 0; i < count; i++)
        print_packet(packets[i], lens[i]);
    printf("\n%s.response ", name);
    first_packet = 1;
    for (size_t i = 0; i < count; i++)
        fw_usb_rx_feed(&rx, packets[i], lens[i], capture, NULL);
    printf("\n");
}

static void header(uint8_t out[P], uint8_t cmd, uint32_t packets, uint32_t bytes) {
    memset(out, 0, P);
    out[0] = cmd;
    memcpy(out + 1, &packets, 4);
    memcpy(out + 5, &bytes, 4);
}

static uint32_t zlib_crc32(const uint8_t *data, size_t len) {
    uint32_t crc = UINT32_MAX;
    for (size_t i = 0; i < len; i++) {
        crc ^= data[i];
        for (unsigned bit = 0; bit < 8; bit++)
            crc = (crc >> 1) ^ ((crc & 1U) ? UINT32_C(0xedb88320) : 0U);
    }
    return ~crc;
}

/* Sends cmd with payload, split into full packets. */
static void command(const char *name, uint8_t cmd, const uint8_t *payload, uint32_t len) {
    static uint8_t packets[1 + (FW_REFINEMENT_RECORD_BYTES + P - 1) / P][P];
    static size_t lens[sizeof(packets) / P];
    uint32_t n = (len + P - 1) / P;
    header(packets[0], cmd, n, len);
    lens[0] = P;
    for (uint32_t i = 0; i < n; i++) {
        memset(packets[1 + i], 0, P);
        uint32_t chunk = len - i * P < P ? len - i * P : P;
        memcpy(packets[1 + i], payload + i * P, chunk);
        lens[1 + i] = P;
    }
    run(name, (const uint8_t(*)[P])packets, lens, 1 + n);
}

int main(void) {
    if (fw_lfs_host_format_and_mount() != 0)
        return 1;
    fw_runtime_init(&rt);
    fw_usb_rx_init(&rx, &rt, NULL);
    rt.active_profile = 2;
    rt.active_actuation_mode = 1;
    rt.platform_code = 3;

    command("version", FW_CMD_REQUEST_VERSION, NULL, 0);
    command("runtime_state", FW_CMD_GET_RUNTIME_STATE, NULL, 0);
    static const uint8_t four[4] = {7, 0, 0, 0};
    command("runtime_state.with_payload", FW_CMD_GET_RUNTIME_STATE, four, 4);
    command("set_platform", FW_CMD_SET_PLATFORM, four, 4);
    command("set_platform.short", FW_CMD_SET_PLATFORM, four, 2);
    command("unknown", 0x99, NULL, 0);
    command("get_keymap.none_stored", FW_CMD_GET_KEYMAP, NULL, 0);
    command("refinement_status.absent", FW_CMD_PRESS_REFINEMENT_STATUS, NULL, 0);
    command("refinement_status.with_payload", FW_CMD_PRESS_REFINEMENT_STATUS, four, 4);
    command("put_refinement.wrong_size", FW_CMD_PUT_PRESS_REFINEMENT, four, 4);

    static uint8_t record[FW_REFINEMENT_RECORD_BYTES];
    memcpy(record, "OFRM", 4);
    uint32_t fields[3] = {FW_REFINEMENT_FORMAT, FW_REFINEMENT_PAYLOAD_BYTES, 0};
    for (size_t i = 0; i < FW_REFINEMENT_PAYLOAD_BYTES; i++)
        record[FW_REFINEMENT_HEADER_BYTES + i] = (uint8_t)(i * 13U + 1U);
    fields[2] = zlib_crc32(record + FW_REFINEMENT_HEADER_BYTES, FW_REFINEMENT_PAYLOAD_BYTES);
    memcpy(record + 4, fields, sizeof(fields));
    record[20] ^= 0xFFU;
    command("put_refinement.bad_crc", FW_CMD_PUT_PRESS_REFINEMENT, record, sizeof(record));
    record[20] ^= 0xFFU;
    command("put_refinement", FW_CMD_PUT_PRESS_REFINEMENT, record, sizeof(record));
    command("refinement_status.installed", FW_CMD_PRESS_REFINEMENT_STATUS, NULL, 0);
    command("get_refinement", FW_CMD_GET_PRESS_REFINEMENT, NULL, 0);

    uint8_t pkts[3][P];
    size_t lens[3] = {P, P, P};
    header(pkts[0], FW_CMD_SET_LANGUAGE, 2, 4);
    run("wrong_packet_count", (const uint8_t(*)[P])pkts, lens, 1);
    header(pkts[0], FW_CMD_SET_LANGUAGE, 1, 4);
    run("short_header", (const uint8_t(*)[P])pkts, (size_t[]){8}, 1);
    header(pkts[0], FW_CMD_SEND_KEYMAP, 2, FW_MAX_KEYMAP_BYTES + 1U);
    header(pkts[0], FW_CMD_SEND_KEYMAP, (FW_MAX_KEYMAP_BYTES + P) / P, FW_MAX_KEYMAP_BYTES + 1U);
    printf("too_large.header ");
    first_packet = 1;
    print_packet(pkts[0], P);
    printf("\ntoo_large.response ");
    first_packet = 1;
    fw_usb_rx_feed(&rx, pkts[0], P, capture, NULL);
    memset(pkts[1], 0, P);
    for (uint32_t i = 0; i < (FW_MAX_KEYMAP_BYTES + P) / P; i++) {
        if (i == (FW_MAX_KEYMAP_BYTES + P) / P - 1)
            printf("last:");
        fw_usb_rx_feed(&rx, pkts[1], P, capture, NULL);
    }
    printf("\n");
    header(pkts[0], FW_CMD_SET_LANGUAGE, 2, 100);
    memset(pkts[1], 0x11, P);
    memset(pkts[2], 0x22, P);
    run("short_payload_packet", (const uint8_t(*)[P])pkts, (size_t[]){P, P, 20}, 3);
    return 0;
}
