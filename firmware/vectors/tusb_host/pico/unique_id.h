/* A fixed board ID, so the serial-number string is reproducible. */
#include <string.h>
#define PICO_UNIQUE_BOARD_ID_SIZE_BYTES 8
static inline void pico_get_unique_board_id_string(char *out, unsigned len) {
    strncpy(out, "E6614103E7452D2F", len);
    out[len - 1] = 0;
}
