/* Builds Keyglass's TinyUSB config on the host: same classes and buffer sizes, no OS layer. */
#include_next "tusb_config.h"
#undef CFG_TUSB_OS
#define CFG_TUSB_OS OPT_OS_NONE
