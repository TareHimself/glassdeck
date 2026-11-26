#include "fw_hdlc.h"
#include "fw_scanner.h"

#include <stdio.h>
#include <string.h>

static void print_hex(const char *name, const uint8_t *data, size_t len) {
    printf("%s ", name);
    for (size_t i = 0; i < len; i++)
        printf("%02x", data[i]);
    printf("\n");
}

static void print_frame(const char *name, const uint8_t *payload, size_t len) {
    static uint8_t wire[FW_HDLC_MAX_LEN * 2U + 16U];
    int n = fw_hdlc_encode(payload, len, wire, sizeof(wire));
    if (n < 0) {
        printf("%s refused\n", name);
        return;
    }
    print_hex(name, wire, (size_t)n);
}

static void on_frame(const uint8_t *payload, size_t len, void *ctx) {
    (void)ctx;
    print_hex("decoded", payload, len);
}

int main(void) {
    static uint8_t payload[FW_HDLC_MAX_LEN + 1U];
    for (size_t i = 0; i < sizeof(payload); i++)
        payload[i] = (uint8_t)(i * 37U + 11U);

    printf("crc32.check %08x\n", fw_hdlc_crc32((const uint8_t *)"123456789", 9));
    print_frame("frame.empty", NULL, 0);
    static const uint8_t escapes[] = {0x7E, 0x7D, 0x00, 0xFF, 0x5E, 0x5D};
    print_frame("frame.escapes", escapes, sizeof(escapes));
    print_frame("frame.max", payload, FW_HDLC_MAX_LEN);
    print_frame("frame.too_long", payload, FW_HDLC_MAX_LEN + 1U);

    static fw_runtime_t rt;
    memset(&rt, 0, sizeof(rt));
    for (size_t i = 0; i < sizeof(rt.key_bitmap); i++)
        rt.key_bitmap[i] = (uint8_t)(i * 29U + 3U);
    rt.keyframe_layout = 2;
    rt.keyframe_attached = 1;
    rt.switch_feel = 2;
    rt.effective_layer_bitfield = 0x00000105U;
    rt.active_actuation_mode = 3;
    rt.of_event_seq = 0x7E21;
    for (size_t i = 0; i < 16; i++) {
        rt.of_event_ring[i] = (uint8_t)(i + 2U);
        rt.of_event_profiles[i] = (uint8_t)(i % 3U);
    }
    rt.host_state_status = 0x05;
    rt.language_code = 0x0409;
    rt.platform_code = 3;
    rt.active_profile = 1;
    rt.lifecycle.attach_phase = 3;
    rt.uart_tx_seq = 0x12347D7DU;
    uint8_t update[FW_UART_MSG_UPDATE_LEN];
    fw_runtime_build_msg_update(&rt, 0xDEADBEEFU, update);
    print_hex("key_state", update, sizeof(update));
    print_frame("key_state.frame", update, sizeof(update));

    for (size_t i = 0; i < FW_NUM_SENSORS; i++)
        rt.raw_adc[i] = (uint16_t)((i * 97U + 5U) & 0x0FFFU);
    uint8_t qc[FW_UART_MSG_QC_LEN];
    fw_runtime_build_msg_qc(&rt, qc);
    print_hex("sensor_readings", qc, sizeof(qc));

    /* Garbage, a frame cut short by a new flag, a good frame, a bad CRC, a length over
     * the limit, an empty frame, then a frame with escapes. */
    static uint8_t stream[4096];
    size_t pos = 0;
    static const uint8_t garbage[] = {0x01, 0x02, 0x7D, 0x5E};
    memcpy(stream + pos, garbage, sizeof(garbage));
    pos += sizeof(garbage);
    static uint8_t wire[FW_HDLC_MAX_LEN * 2U + 16U];
    int n = fw_hdlc_encode(payload, 40, wire, sizeof(wire));
    memcpy(stream + pos, wire, 20);
    pos += 20;
    n = fw_hdlc_encode(payload, 40, wire, sizeof(wire));
    memcpy(stream + pos, wire, (size_t)n);
    pos += (size_t)n;
    n = fw_hdlc_encode(payload + 1, 10, wire, sizeof(wire));
    wire[n - 1] ^= 0x01U;
    memcpy(stream + pos, wire, (size_t)n);
    pos += (size_t)n;
    static const uint8_t oversize[] = {0x7E, 0x02, 0x01, 0xAA, 0xBB};
    memcpy(stream + pos, oversize, sizeof(oversize));
    pos += sizeof(oversize);
    n = fw_hdlc_encode(NULL, 0, wire, sizeof(wire));
    memcpy(stream + pos, wire, (size_t)n);
    pos += (size_t)n;
    n = fw_hdlc_encode(escapes, sizeof(escapes), wire, sizeof(wire));
    memcpy(stream + pos, wire, (size_t)n);
    pos += (size_t)n;
    print_hex("stream", stream, pos);
    fw_hdlc_decoder_t dec;
    fw_hdlc_decoder_init(&dec);
    for (size_t i = 0; i < pos; i++)
        fw_hdlc_decoder_feed(&dec, stream + i, 1, on_frame, NULL);
    printf("crc_errors %u\n", (unsigned)dec.crc_errors);
    return 0;
}
