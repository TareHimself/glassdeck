/* Runs Keyglass's scanner runtime the way rp2040_main.c's loop does, on a generated board, and
 * prints a hash of the runtime's outputs and state after every scan. The keymaps, profiles,
 * calibration and refinement model are printed too, so the Rust test reads them instead of
 * building them. crates/glassdeck-scanner-dsp/src/testing_runtime.rs replays the board (keep the
 * two in step) and must print the same hashes. */
#include "fw_lifecycle.h"
#include "fw_refine.h"
#include "fw_scanner.h"

#include <stdio.h>
#include <string.h>

static uint32_t rng;

static uint32_t next(void) {
    rng ^= rng << 13;
    rng ^= rng >> 17;
    rng ^= rng << 5;
    return rng;
}

static int32_t pick(uint32_t n) {
    return (int32_t)(next() % n);
}

static uint64_t hash;

static void begin(void) {
    hash = 0xcbf29ce484222325ULL;
}

static void mix_i32(int32_t v) {
    for (int i = 0; i < 4; i++) {
        hash ^= (uint8_t)(v >> (8 * i));
        hash *= 0x100000001b3ULL;
    }
}

static void mix_bytes(const uint8_t *p, size_t n) {
    for (size_t i = 0; i < n; i++)
        mix_i32(p[i]);
}

static void print_hex(const char *name, const uint8_t *p, size_t n) {
    printf("runtime.%s ", name);
    for (size_t i = 0; i < n; i++)
        printf("%02x", p[i]);
    printf("\n");
}

static uint16_t adc(int32_t v) {
    return (uint16_t)(v < 0 ? 0 : v > 4095 ? 4095 : v);
}

/* ---- fixed inputs, printed for the Rust test ---- */

static uint32_t words[FW_MAX_KEYMAP_BYTES / 4];

static uint32_t layer0(unsigned k) {
    switch (k) {
        case 2: return 0x21000001U;  /* momentary layer 1 */
        case 7: return 0x22000002U;  /* toggle layer 2 */
        case 12: return 0x10000000U; /* macro 0 */
        case 17: return 0x400000E9U; /* volume up */
        case 22: return 0x50000004U; /* next profile */
        case 27: return 0x50000006U; /* next actuation mode */
        case 32: return 0x50000009U; /* platform macOS */
        case 37: return 0xF0000000U; /* KC_NO */
        case 42: return 0x000000E1U; /* left shift */
        case 47: return 0x00020004U; /* shift + a */
        case 52: return 0x10000001U; /* macro 1, empty */
        case 57: return 0x23000002U; /* switch to layer 2 */
        case 62: return 0x24000001U; /* toggle only layer 1 */
        default: return 0x04U + k % 40U;
    }
}

/* One section with three layers, two modes and two macros. `all_fixed` gives mode 0 one fixed setting. */
static size_t put_section(uint32_t *w, unsigned keys, bool all_fixed) {
    const unsigned layers = 3, modes = 2, macros = 2;
    const uint32_t m0[4] = {0x80000004U, (20U << 8) | 0x04U, 0x80000000U | (5U << 8) | 0x05U, (30U << 8) | 0x05U};
    uint32_t pool = macros + (1 + 4) + (1 + 0);
    uint32_t kc = pool, act = kc + keys * layers;
    w[0] = layers;
    w[1] = keys;
    w[2] = modes;
    w[3] = macros;
    w[4] = macros;
    w[5] = kc;
    w[6] = act;
    uint32_t *p = w + 7;
    p[0] = macros;
    p[1] = macros + 5;
    p[2] = 4;
    memcpy(p + 3, m0, sizeof(m0));
    p[7] = 0;
    for (unsigned k = 0; k < keys; k++) {
        p[kc + k] = layer0(k);
        p[kc + keys + k] = k % 5 == 0 ? 0x3AU + k % 12U : 0x50000000U;
        p[kc + 2 * keys + k] = k % 7 == 0 ? 0x400000CDU : 0x50000000U;
        uint32_t fixed = (1U << 30) | (150U << 12) | 500U, rapid = (2U << 30) | (60U << 12) | 450U;
        p[act + k] = all_fixed ? (1U << 30) | (200U << 12) | 600U : k % 3 == 0 ? fixed : k % 3 == 1 ? 0U : rapid;
        p[act + keys + k] = (2U << 30) | (40U << 12) | 400U;
    }
    return 7 + act + keys * modes;
}

static size_t build_keymap(bool all_fixed) {
    memset(words, 0, sizeof(words));
    words[0] = 2;
    words[1] = 3;
    size_t pos = 6;
    words[3] = 0;
    pos += put_section(words + pos, 85, all_fixed);
    words[4] = (uint32_t)(pos - 6);
    pos += put_section(words + pos, 87, all_fixed);
    return pos * 4;
}

static uint8_t keymap1[FW_MAX_KEYMAP_BYTES], keymap2[FW_MAX_KEYMAP_BYTES];
static size_t keymap1_len, keymap2_len;
static uint8_t profiles[32 + 3 * 8];
static uint8_t cal_info[20], cal_factors[320];
static uint8_t model[FW_REFINE_MODEL_BYTES];

static void put32(uint8_t *p, uint32_t v) {
    for (int i = 0; i < 4; i++)
        p[i] = (uint8_t)(v >> (8 * i));
}

static void make_inputs(void) {
    keymap1_len = build_keymap(false);
    memcpy(keymap1, words, keymap1_len);
    keymap2_len = build_keymap(true);
    memcpy(keymap2, words, keymap2_len);
    memset(profiles, 0, sizeof(profiles));
    put32(profiles + 16, 3);
    const uint32_t entries[3][2] = {{0, 1}, {1, 1}, {0, 4}};
    for (unsigned i = 0; i < 3; i++) {
        put32(profiles + 32 + 8 * i, entries[i][0]);
        put32(profiles + 36 + 8 * i, entries[i][1]);
    }
    uint32_t sum = 0;
    for (unsigned i = 0; i < 160; i++) {
        uint16_t f = (uint16_t)(230 + (i * 7) % 60);
        cal_factors[2 * i] = (uint8_t)f;
        cal_factors[2 * i + 1] = (uint8_t)(f >> 8);
        sum += f;
    }
    put32(cal_info, FW_CALIB_MAGIC);
    put32(cal_info + 4, 2);
    put32(cal_info + 8, 160);
    put32(cal_info + 12, 0);
    put32(cal_info + 16, sum);
    for (unsigned i = 0; i < sizeof(model); i++)
        model[i] = (uint8_t)((i * 131U + 7U) ^ (i >> 3));
    print_hex("keymap1", keymap1, keymap1_len);
    print_hex("keymap2", keymap2, keymap2_len);
    print_hex("profiles", profiles, sizeof(profiles));
    print_hex("cal_info", cal_info, sizeof(cal_info));
    print_hex("cal_factors", cal_factors, sizeof(cal_factors));
    print_hex("model", model, sizeof(model));
}

/* ---- the board ---- */

typedef struct {
    int32_t rest[160];
    unsigned layout;
    bool frame_on;
    int32_t travel[6][16], target[6][16], speed[6][16], hold[6][16];
} board_t;

static void board_init(board_t *b, unsigned layout, int32_t id0, int32_t id1) {
    memset(b, 0, sizeof(*b));
    b->layout = layout;
    b->frame_on = true;
    for (unsigned i = 0; i < 160; i++)
        b->rest[i] = 1500 + pick(1000);
    b->rest[5] = id0;
    b->rest[106] = id1;
}

static void board_step(board_t *b) {
    for (unsigned r = 0; r < 6; r++)
        for (unsigned c = 0; c + 1 < FW_ROW_LEN[b->layout][r]; c++) {
            if (!b->target[r][c] && !b->travel[r][c] && pick(3000) == 0) {
                b->target[r][c] = 500 + pick(900);
                b->speed[r][c] = 40 + pick(80);
                b->hold[r][c] = 30 + pick(300);
            }
            int32_t d = b->target[r][c] - b->travel[r][c];
            int32_t s = b->speed[r][c];
            b->travel[r][c] += d > s ? s : d < -s ? -s : d;
            if (b->target[r][c] && b->travel[r][c] == b->target[r][c] && b->hold[r][c] && --b->hold[r][c] == 0)
                b->target[r][c] = 0;
        }
}

/* One sweep of all 160 channels. */
static void board_sweep(const board_t *b, uint16_t raw[160]) {
    int32_t v[160];
    for (unsigned i = 0; i < 160; i++)
        v[i] = b->rest[i] + pick(7) - 3;
    if (b->frame_on) {
        for (unsigned i = 0; i < 5; i++)
            v[i] += 450;
        for (unsigned i = 107; i < 112; i++)
            v[i] -= 450;
    } else {
        v[5] = 2300;
        v[106] = 1700;
    }
    for (unsigned r = 0; r < 6; r++)
        for (unsigned c = 0; c + 1 < FW_ROW_LEN[b->layout][r]; c++) {
            v[FW_SENSOR_MAP[b->layout][r][c]] += b->travel[r][c] / 2;
            v[FW_SENSOR_MAP[b->layout][r][c + 1]] -= b->travel[r][c] / 2;
        }
    for (unsigned i = 0; i < 160; i++)
        raw[i] = adc(v[i]);
}

/* ---- the main loop ---- */

static int sink_kbd(uint8_t modifiers, const uint8_t keycodes[6], const uint8_t nkro_bitmap[32], void *ctx) {
    (void)ctx;
    mix_i32(0x4B);
    mix_i32(modifiers);
    mix_bytes(keycodes, 6);
    mix_bytes(nkro_bitmap, 32);
    return 0;
}

static int sink_consumer(uint16_t usage, void *ctx) {
    (void)ctx;
    mix_i32(0x43);
    mix_i32(usage);
    return 0;
}

static int sink_of(uint32_t mask, void *ctx) {
    (void)ctx;
    mix_i32(0x46);
    mix_i32((int32_t)mask);
    return 0;
}

static void runtime_case(const char *name, uint32_t seed, unsigned layout, int32_t id0, int32_t id1, bool refined,
                         bool detach) {
    rng = seed;
    static board_t b;
    board_init(&b, layout, id0, id1);
    static fw_runtime_t rt;
    fw_runtime_init(&rt);
    if (fw_calibration_load(&rt.cal, cal_info, sizeof(cal_info), cal_factors, sizeof(cal_factors)) != 0)
        return;
    if (fw_runtime_load_keymap(&rt, keymap1, keymap1_len) != 0)
        return;
    if (fw_runtime_load_profile_settings(&rt, profiles, sizeof(profiles)) != 0)
        return;
    if (refined && fw_runtime_install_refinement(&rt, model, sizeof(model)) != 0)
        return;
    static int16_t saved[4][3][112];
    static bool has_saved[4][3];
    memset(has_saved, 0, sizeof(has_saved));
    static uint16_t raw[160], sweep[160];
    memset(raw, 0, sizeof(raw));
    static fw_hid_scheduler_t sched;
    memset(&sched, 0, sizeof(sched));
    uint8_t last_bitmap[32] = {0}, last_leds = 0;
    uint32_t last_profile = rt.active_profile, last_mode = rt.active_actuation_mode, last_platform = rt.platform_code;
    uint32_t last_layers = rt.effective_layer_bitfield;
    uint16_t last_event = rt.of_event_seq;
    uint32_t now = 1000, last_full = now, last_uart = now - 1000;
    for (unsigned scan = 0; scan < 9000; scan++) {
        if (scan == 4000)
            rt.qc_mode = 1;
        if (scan == 4300)
            rt.qc_mode = 0;
        if (scan == 5000)
            fw_runtime_set_active_profile(&rt, 1);
        if (scan == 5500)
            fw_runtime_load_keymap(&rt, keymap2, keymap2_len);
        if (scan == 7000)
            rt.host_state_status = 2;
        if (detach)
            b.frame_on = !(scan >= 4500 && scan < 4800);
        begin();
        board_step(&b);
        bool full = now - last_full >= 16;
        board_sweep(&b, sweep);
        memcpy(raw, sweep, (full ? 160 : 112) * sizeof(uint16_t));
        if (full)
            last_full = now;
        bool recal = fw_runtime_poll_frame(&rt, raw, now);
        mix_i32(recal);
        if (recal) {
            uint32_t sums[160] = {0};
            uint16_t low[112], high[112];
            for (unsigned pass = 0; pass < 8; pass++) {
                board_sweep(&b, raw);
                for (unsigned i = 0; i < 160; i++) {
                    sums[i] += raw[i];
                    if (i < 112) {
                        if (!pass || raw[i] < low[i])
                            low[i] = raw[i];
                        if (!pass || raw[i] > high[i])
                            high[i] = raw[i];
                    }
                }
            }
            uint16_t mean[160];
            uint8_t unstable[112];
            for (unsigned i = 0; i < 160; i++)
                mean[i] = (uint16_t)(sums[i] / 8);
            for (unsigned i = 0; i < 112; i++)
                unstable[i] = high[i] - low[i] > 40;
            last_full = now;
            rt.rest_ref_valid = 0;
            if (rt.keyframe_attached && has_saved[rt.keyframe_layout][rt.switch_feel]) {
                memcpy(rt.rest_ref_adc, saved[rt.keyframe_layout][rt.switch_feel], sizeof(rt.rest_ref_adc));
                rt.rest_ref_valid = 1;
            }
            fw_runtime_calibrate(&rt, mean, unstable, now, true);
        }
        bool kbd_ready = scan % 4 != 3;
        fw_hid_report_t hid;
        fw_runtime_set_time(&rt, now);
        fw_runtime_set_hid_ready(&rt, kbd_ready);
        fw_runtime_process_scan(&rt, raw, &hid);
        bool save = fw_runtime_finish_scan(&rt, now);
        mix_i32(save);
        if (save) {
            memcpy(saved[rt.keyframe_layout][rt.switch_feel], rt.rest_ref_adc, sizeof(rt.rest_ref_adc));
            has_saved[rt.keyframe_layout][rt.switch_feel] = true;
        }
        mix_i32(rt.platform_dirty);
        rt.platform_dirty = 0;
        fw_hid_scheduler_step(&sched, &hid, kbd_ready, scan % 2 == 0, sink_kbd, sink_consumer, sink_of, NULL);
        bool changed = memcmp(last_bitmap, rt.key_bitmap, 32) != 0 || rt.host_state_status != last_leds ||
                       rt.active_profile != last_profile || rt.active_actuation_mode != last_mode ||
                       rt.platform_code != last_platform || rt.effective_layer_bitfield != last_layers ||
                       rt.of_event_seq != last_event;
        if (changed || now - last_uart >= 1000) {
            uint8_t msg[FW_UART_MSG_UPDATE_LEN];
            fw_runtime_build_msg_update(&rt, now, msg);
            mix_bytes(msg, sizeof(msg));
            memcpy(last_bitmap, rt.key_bitmap, 32);
            last_event = rt.of_event_seq;
            last_leds = rt.host_state_status;
            last_profile = rt.active_profile;
            last_mode = rt.active_actuation_mode;
            last_platform = rt.platform_code;
            last_layers = rt.effective_layer_bitfield;
            last_uart = now;
        }
        if (rt.qc_mode && full) {
            uint8_t qc[FW_UART_MSG_QC_LEN];
            fw_runtime_build_msg_qc(&rt, qc);
            mix_bytes(qc, sizeof(qc));
        }
        mix_i32(hid.modifiers);
        mix_bytes(hid.keycodes, 6);
        mix_bytes(hid.nkro_bitmap, 32);
        mix_i32(hid.consumer_usage);
        mix_i32((int32_t)hid.of_func_mask);
        for (unsigned k = 0; k < FW_MAX_KEYS_PER_LAYOUT; k++) {
            mix_i32(rt.key_pressed[k]);
            mix_i32(rt.flux[k]);
            mix_i32(rt.latched_valid[k]);
            mix_i32((int32_t)rt.latched_action[k]);
            mix_i32(rt.fixed_transition_scans[k]);
            mix_i32(rt.neighbor_vetoes[k]);
        }
        mix_bytes(rt.key_bitmap, 32);
        for (unsigned i = 0; i < 160; i++)
            mix_i32(rt.baseline_adc[i]);
        mix_i32((int32_t)rt.active_layer_bitfield);
        mix_i32((int32_t)rt.toggled_layer_bitfield);
        mix_i32((int32_t)rt.effective_layer_bitfield);
        mix_i32((int32_t)rt.active_actuation_mode);
        mix_i32((int32_t)rt.active_profile);
        mix_i32((int32_t)rt.platform_code);
        mix_i32(rt.macro_active);
        mix_i32(rt.macro_id);
        mix_i32((int32_t)rt.macro_step_idx);
        mix_i32((int32_t)rt.macro_next_ms);
        mix_i32((int32_t)rt.macro_clock_ms);
        mix_bytes(rt.macro_nkro_bitmap, 32);
        mix_i32(rt.keyframe_attached);
        mix_i32(rt.keyframe_layout);
        mix_i32(rt.switch_feel);
        mix_i32(rt.lifecycle.attach_phase);
        mix_i32(rt.lifecycle.calibrated);
        mix_i32(rt.rest_ref_valid);
        mix_i32(rt.of_event_seq);
        mix_i32((int32_t)rt.uart_tx_seq);
        mix_i32(rt.classic_setup);
        mix_i32(rt.last_scan_mode);
        printf("runtime.%s %016llx\n", name, (unsigned long long)hash);
        now += 1;
    }
}

int main(void) {
    make_inputs();
    runtime_case("ansi_classic", 30, 1, 2700, 2200, false, false);
    runtime_case("iso_refined", 31, 2, 1900, 1400, true, false);
    runtime_case("ansi_detach", 32, 1, 2700, 1400, false, true);
    return 0;
}
