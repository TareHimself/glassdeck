/* Prints every USB descriptor the stock firmware serves, from Keyglass's own usb_descriptors.c. */
#include "/kg/keyboard/firmware/src/usb_descriptors.c"

#include <stdio.h>

static const void *g_ctrl_data;
static uint16_t g_ctrl_len;

bool tud_control_xfer(uint8_t rhport, tusb_control_request_t const *request, void *buffer, uint16_t len) {
    (void)rhport;
    (void)request;
    g_ctrl_data = buffer;
    g_ctrl_len = len;
    return true;
}

uint16_t fw_on_hid_get_report(uint8_t instance, uint8_t report_id, uint8_t *buffer, uint16_t reqlen) {
    (void)instance;
    (void)report_id;
    (void)buffer;
    (void)reqlen;
    return 0;
}
void fw_on_hid_led_report(uint8_t led_mask) {
    (void)led_mask;
}
void fw_on_hid_protocol_changed(uint8_t protocol) {
    (void)protocol;
}

static void print_hex(const char *name, const uint8_t *p, size_t n) {
    printf("%s ", name);
    for (size_t i = 0; i < n; i++)
        printf("%02x", p[i]);
    printf("\n");
}

int main(void) {
    print_hex("device", tud_descriptor_device_cb(), sizeof(tusb_desc_device_t));
    const uint8_t *config = tud_descriptor_configuration_cb(0);
    print_hex("configuration", config, (size_t)(config[2] | (config[3] << 8)));
    print_hex("hid_report.0", tud_hid_descriptor_report_cb(0), sizeof(desc_hid_kbd_report));
    print_hex("hid_report.1", tud_hid_descriptor_report_cb(1), sizeof(desc_hid_of_report));
    const uint8_t *bos = tud_descriptor_bos_cb();
    print_hex("bos", bos, (size_t)(bos[2] | (bos[3] << 8)));
    tusb_control_request_t ms = {
        .bmRequestType = 0xC0, .bRequest = VENDOR_REQUEST_MICROSOFT, .wValue = 0, .wIndex = 7, .wLength = 0xB2};
    tud_vendor_control_xfer_cb(0, CONTROL_STAGE_SETUP, &ms);
    print_hex("ms_os_20", g_ctrl_data, g_ctrl_len);
    for (uint8_t i = 0; i < 6; i++) {
        const uint16_t *s = tud_descriptor_string_cb(i, 0x0409);
        char name[16];
        snprintf(name, sizeof name, "string.%u", i);
        if (!s) {
            printf("%s none\n", name);
            continue;
        }
        print_hex(name, (const uint8_t *)s, s[0] & 0xFF);
    }
    return 0;
}
