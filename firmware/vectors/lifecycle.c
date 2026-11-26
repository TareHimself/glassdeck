/* Runs Keyglass's keyframe detection, thermal drift tracking and rest recovery on generated
 * board readings and prints a hash of every output and state after every step.
 * crates/glassdeck-scanner-dsp/src/testing_lifecycle.rs generates the same readings (keep the two
 * in step) and must print the same hashes. */
#include "fw_keyframe.h"
#include "fw_rest.h"
#include "fw_rest_runtime.h"
#include "fw_scanner.h"
#include "fw_thermal.h"

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

static uint16_t adc(int32_t v) {
    return (uint16_t)(v < 0 ? 0 : v > 4095 ? 4095 : v);
}

/* ---- keyframe ---- */

static void keyframe_case(const char *name, uint32_t seed) {
    rng = seed;
    static fw_keyframe_detector_t d;
    memset(&d, 0, sizeof(d));
    unsigned phase = 0, burst = 0, id_burst = 0;
    int32_t id_value[2] = {2048, 2048};
    uint32_t now = 1000;
    for (unsigned scan = 0; scan < 3000; scan++) {
        if (scan % 600 == 0) {
            phase = (unsigned)pick(5);
            static const int32_t id0[3] = {2700, 1900, 2300}, id1[3] = {2200, 1400, 1700};
            id_value[0] = id0[pick(3)];
            id_value[1] = id1[pick(3)];
        }
        if (pick(30) == 0)
            burst = 2;
        if (pick(400) == 0)
            id_burst = 3;
        bool attached = phase != 0;
        uint16_t raw[112];
        for (unsigned i = 0; i < 112; i++) {
            int32_t v = 2048 + pick(21) - 10;
            if (i < 5)
                v += attached ? 350 : 0;
            if (i >= 107)
                v -= attached ? 450 : 0;
            if (burst && i % 23 == 7)
                v += 200;
            raw[i] = adc(v);
        }
        raw[5] = adc(id_value[0] + pick(21) - 10 + (id_burst ? 400 : 0));
        raw[106] = adc(id_value[1] + pick(21) - 10);
        if (burst)
            burst--;
        if (id_burst)
            id_burst--;
        d.stress_frame = (seed & 1) && phase == 0;
        fw_keyframe_result_t f = fw_keyframe_poll(&d, raw, now);
        now += 25;
        begin();
        mix_i32(f.attached);
        mix_i32(f.layout);
        mix_i32(f.feel);
        mix_i32(f.stage);
        mix_i32(f.changed);
        mix_i32(f.calibrate);
        for (unsigned g = 0; g < 2; g++)
            for (unsigned j = 0; j < 30; j++)
                mix_i32(d.history[g][j]);
        for (unsigned i = 0; i < 112; i++)
            mix_i32(d.previous[i]);
        mix_i32((int32_t)d.last_poll_ms);
        mix_i32(d.next);
        mix_i32(d.samples);
        mix_i32(d.stable_id[0]);
        mix_i32(d.stable_id[1]);
        mix_i32(d.stable_scans);
        mix_i32(d.wait_scans);
        mix_i32(d.missing_scans);
        printf("keyframe.%s %016llx\n", name, (unsigned long long)hash);
    }
}

/* ---- a board: resting values, a thermal gain and offset, and keys pressed on top ---- */

typedef struct {
    int32_t rest[160];
    int32_t gain_q10, offset;
    int32_t press_left[6][16]; /* scans left in the current press, 0 when up */
    int32_t press_depth[6][16];
} board_t;

static void board_init(board_t *b) {
    memset(b, 0, sizeof(*b));
    for (unsigned i = 0; i < 160; i++)
        b->rest[i] = 1400 + pick(1300);
}

/* Starts and ends presses: each key at rest starts one with odds 1 in `odds` a scan. Returns keys down. */
static unsigned board_keys(board_t *b, unsigned layout, uint32_t odds, fw_thermal_key_t keys[6][16]) {
    unsigned held = 0;
    for (unsigned r = 0; r < 6; r++)
        for (unsigned c = 0; c < 16; c++) {
            fw_thermal_key_t *k = &keys[r][c];
            if (c + 1 >= FW_ROW_LEN[layout][r]) {
                memset(k, 0, sizeof(*k));
                continue;
            }
            if (b->press_left[r][c]) {
                b->press_left[r][c]--;
            } else if (pick(odds) == 0) {
                b->press_left[r][c] = 50 + pick(150);
                b->press_depth[r][c] = 600 + pick(600);
            }
            uint16_t flux = (uint16_t)(b->press_left[r][c] ? b->press_depth[r][c] : pick(30));
            k->flux = flux;
            k->down = flux > 650;
            k->pending = k->down || (flux > 600 && pick(2) == 0);
            k->masked = pick(1000) == 0;
            held += k->down;
        }
    return held;
}

static void board_raw(const board_t *b, unsigned layout, const fw_thermal_key_t keys[6][16], uint16_t raw[160]) {
    int32_t v[160];
    for (unsigned i = 0; i < 160; i++)
        v[i] = b->rest[i] + (b->rest[i] - 2048) * b->gain_q10 / 1024 + b->offset + pick(7) - 3;
    for (unsigned r = 0; r < 6; r++)
        for (unsigned c = 0; c + 1 < FW_ROW_LEN[layout][r]; c++) {
            int32_t f = keys[r][c].flux;
            v[FW_SENSOR_MAP[layout][r][c]] += f / 2;
            v[FW_SENSOR_MAP[layout][r][c + 1]] -= f / 2;
        }
    for (unsigned i = 0; i < 160; i++)
        raw[i] = adc(v[i]);
}

/* ---- thermal ---- */

static void thermal_case(const char *name, uint32_t seed, unsigned layout, bool modules) {
    rng = seed;
    static board_t b;
    board_init(&b);
    static fw_thermal_state_t s;
    memset(&s, 0, sizeof(s));
    uint16_t baseline[160], raw[160];
    for (unsigned i = 0; i < 160; i++)
        baseline[i] = (uint16_t)b.rest[i];
    fw_thermal_acquired(&s, baseline);
    fw_thermal_key_t keys[6][16];
    uint32_t now = 5000;
    for (unsigned scan = 0; scan < 3200; scan++) {
        if (scan >= 300 && scan < 700 && scan % 8 == 0)
            b.gain_q10++;
        if (scan >= 1400 && scan < 1800 && scan % 10 == 0)
            b.offset++;
        if (scan == 1200)
            fw_thermal_acquired(&s, baseline);
        board_keys(&b, layout, 20000, keys);
        board_raw(&b, layout, keys, raw);
        bool module = modules && scan >= 800 && scan < 1600;
        bool refreshed = fw_thermal_after_scan(&s, (uint8_t)layout, now, raw, baseline, keys, module);
        now += 1;
        begin();
        mix_i32(refreshed);
        for (unsigned i = 0; i < 160; i++)
            mix_i32(baseline[i]);
        for (unsigned j = 0; j < 12; j++)
            mix_i32(s.sum[j]);
        for (unsigned g = 0; g < 2; g++)
            for (unsigned k = 0; k < 4; k++) {
                for (unsigned j = 0; j < 12; j++)
                    mix_i32(s.history[g][k][j]);
                for (unsigned i = 0; i < 112; i++)
                    mix_i32(s.frames[g][k][i]);
            }
        mix_i32((int32_t)s.clock_ms);
        mix_i32((int32_t)s.history_ms[0]);
        mix_i32((int32_t)s.history_ms[1]);
        mix_i32(s.next[0]);
        mix_i32(s.next[1]);
        mix_i32(s.windows);
        mix_i32(s.initialized);
        mix_i32(s.module_mode);
        mix_i32(s.evidence);
        mix_i32(s.layout);
        mix_i32(s.reference_count);
        for (unsigned j = 0; j < 12; j++)
            mix_i32(s.reference_channel[j]);
        for (unsigned i = 0; i < 112; i++)
            mix_i32(s.inverse_center[i]);
        mix_i32(s.gain_q10);
        mix_i32((int32_t)s.disturbance_until);
        mix_i32((int32_t)s.chord_until);
        mix_i32((int32_t)s.last_drift);
        mix_i32((int32_t)s.last_chord);
        mix_i32((int32_t)s.last_refresh);
        mix_i32(s.latched_drift);
        mix_i32(s.sustained_drift);
        mix_i32(s.deferred_held);
        mix_i32(s.moving);
        mix_i32(s.require_strong);
        mix_i32(s.refreshed);
        for (unsigned r = 0; r < 6; r++)
            for (unsigned c = 0; c < 16; c++) {
                mix_i32(s.protect_held[r][c]);
                mix_i32(s.previous_flux[r][c]);
                mix_i32(s.window_flux[r][c]);
            }
        for (unsigned i = 0; i < 112; i++)
            mix_i32(s.collected[i]);
        mix_i32(s.collected_count);
        mix_i32(s.refresh_count);
        printf("thermal.%s %016llx\n", name, (unsigned long long)hash);
    }
}

/* ---- rest: acquisitions against a stored reference ---- */

typedef struct {
    unsigned layout;
    uint16_t mean[160];
    uint8_t unstable[112];
    int16_t reference[112];
    bool has_reference, has_unstable;
    int32_t held_row[3], held_col[3];
    unsigned held;
} acquisition_t;

static void acquisition(acquisition_t *a) {
    memset(a, 0, sizeof(*a));
    a->layout = 1 + (unsigned)pick(3);
    a->has_reference = pick(4) != 0;
    a->has_unstable = pick(5) != 0;
    int32_t gain = pick(81) - 40, offset = pick(21) - 10;
    bool other = pick(8) == 0;
    for (unsigned i = 0; i < 112; i++)
        a->reference[i] = (int16_t)(1500 + pick(1100));
    for (unsigned i = 0; i < 160; i++) {
        int32_t v;
        if (i < 112 && a->has_reference && !other)
            v = a->reference[i] + (a->reference[i] - 2048) * gain / 1024 + offset + pick(9) - 4;
        else
            v = 1500 + pick(1100);
        a->mean[i] = adc(v);
    }
    if (pick(3) == 0) {
        a->held = 1 + (unsigned)pick(3);
        for (unsigned h = 0; h < a->held; h++) {
            unsigned r = (unsigned)pick(6);
            unsigned c = (unsigned)pick(FW_ROW_LEN[a->layout][r] - 1U);
            int32_t bump = 300 + pick(500);
            a->held_row[h] = (int32_t)r;
            a->held_col[h] = (int32_t)c;
            unsigned left = FW_SENSOR_MAP[a->layout][r][c], right = FW_SENSOR_MAP[a->layout][r][c + 1];
            a->mean[left] = adc(a->mean[left] + bump);
            a->mean[right] = adc(a->mean[right] - bump);
        }
    }
    for (unsigned i = 0; i < 112; i++)
        a->unstable[i] = pick(30) == 0;
}

static void mix_guard(const fw_rest_guard_t *g) {
    mix_i32(g->match);
    mix_i32(g->thermal_gain_q10);
    mix_i32(g->thermal_offset);
    mix_i32(g->thermal_shift);
    mix_i32(g->off_keys);
    for (unsigned r = 0; r < 6; r++)
        for (unsigned c = 0; c < 16; c++)
            mix_i32(g->key[r][c]);
}

static void rest_apply_cases(uint32_t seed) {
    rng = seed;
    for (unsigned trial = 0; trial < 400; trial++) {
        static acquisition_t a;
        acquisition(&a);
        fw_rest_guard_t g;
        fw_rest_apply((uint8_t)a.layout, a.mean, a.has_unstable ? a.unstable : NULL,
                      a.has_reference ? a.reference : NULL, &g);
        begin();
        mix_guard(&g);
        for (unsigned i = 0; i < 160; i++)
            mix_i32(a.mean[i]);
        printf("rest.apply %016llx\n", (unsigned long long)hash);
    }
}

static void print_record(const char *name, const uint8_t record[FW_REST_BYTES]) {
    printf("rest.%s ", name);
    for (unsigned i = 0; i < FW_REST_BYTES; i++)
        printf("%02x", record[i]);
    printf("\n");
}

static void print_decode(const char *name, const uint8_t record[FW_REST_BYTES], uint8_t layout, uint8_t feel) {
    int16_t out[112];
    printf("rest.%s.decode ", name);
    if (fw_rest_decode(record, layout, feel, out) != 0) {
        printf("refused\n");
        return;
    }
    begin();
    for (unsigned i = 0; i < 112; i++)
        mix_i32(out[i]);
    printf("%016llx\n", (unsigned long long)hash);
}

static void rest_record_cases(uint32_t seed) {
    rng = seed;
    int16_t reference[112];
    for (unsigned i = 0; i < 112; i++)
        reference[i] = (int16_t)(pick(5000) - 500);
    uint8_t record[FW_REST_BYTES];
    fw_rest_encode(2, 1, reference, record);
    print_record("record", record);
    print_decode("record", record, 2, 1);
    print_decode("wrong_layout", record, 1, 1);
    print_decode("wrong_feel", record, 2, 2);
    uint8_t broken[FW_REST_BYTES];
    memcpy(broken, record, sizeof(broken));
    broken[40] ^= 1;
    print_record("corrupt", broken);
    print_decode("corrupt", broken, 2, 1);
    uint32_t bytes = 0;
    for (unsigned i = 0; i < 232; i++)
        bytes = bytes * 31U + broken[i];
    for (unsigned i = 0; i < 4; i++)
        broken[232 + i] = (uint8_t)(bytes >> (8 * i));
    print_record("byte_checksum", broken);
    print_decode("byte_checksum", broken, 2, 1);
}

/* ---- rest session: recovering from keys held at boot, then idle re-learning ---- */

static void rest_session_case(const char *name, uint32_t seed) {
    rng = seed;
    static acquisition_t a;
    do
        acquisition(&a);
    while (!a.has_reference || !a.held);
    unsigned layout = a.layout;
    uint16_t baseline[160];
    memcpy(baseline, a.mean, sizeof(baseline));
    fw_rest_guard_t g;
    fw_rest_apply((uint8_t)layout, baseline, a.unstable, a.reference, &g);
    static fw_rest_session_t s;
    fw_rest_begin(&s, (uint8_t)layout, &g, true, false, 0, 1000);
    static board_t b;
    memset(&b, 0, sizeof(b));
    for (unsigned i = 0; i < 160; i++)
        b.rest[i] = i < 112 ? a.reference[i] : a.mean[i];
    uint16_t generation = 0;
    uint32_t now = 1000;
    for (unsigned scan = 0; scan < 3540; scan++) {
        bool idle_phase = scan >= 3500;
        now += idle_phase ? 60000 : 10;
        if (scan == 1500)
            generation++;
        fw_thermal_key_t keys[6][16];
        if (idle_phase)
            memset(keys, 0, sizeof(keys));
        else
            board_keys(&b, layout, 400, keys);
        uint16_t raw[160];
        board_raw(&b, layout, keys, raw);
        if (scan < 300)
            for (unsigned h = 0; h < a.held; h++) {
                unsigned r = (unsigned)a.held_row[h], c = (unsigned)a.held_col[h];
                unsigned left = FW_SENSOR_MAP[layout][r][c], right = FW_SENSOR_MAP[layout][r][c + 1];
                raw[left] = adc(raw[left] + 500);
                raw[right] = adc(raw[right] - 500);
            }
        int32_t delta[160];
        uint8_t confirmed[6][16], proposed[6][16];
        bool any = false;
        for (unsigned i = 0; i < 160; i++)
            delta[i] = (int32_t)raw[i] - baseline[i];
        for (unsigned r = 0; r < 6; r++)
            for (unsigned c = 0; c < 16; c++) {
                confirmed[r][c] = keys[r][c].down;
                proposed[r][c] = keys[r][c].pending;
                any |= keys[r][c].down;
            }
        bool busy = scan >= 900 && scan < 1000;
        fw_rest_recover(&s, now, raw, delta, confirmed, proposed, busy, generation, baseline);
        bool save = fw_rest_idle(&s, now, any, raw, baseline);
        begin();
        mix_i32(save);
        for (unsigned i = 0; i < 160; i++)
            mix_i32(baseline[i]);
        mix_i32(s.layout);
        mix_i32(s.inhibit_all_new);
        for (unsigned r = 0; r < 6; r++)
            for (unsigned c = 0; c < 16; c++) {
                mix_i32(s.inhibit[r][c]);
                mix_i32(s.key[r][c]);
                mix_i32(s.quiet[r][c]);
                mix_i32((int32_t)s.quiet_from_ms[r][c]);
                mix_i32((int32_t)s.suppress_key_from_ms[r][c]);
            }
        mix_i32(s.thermal_shift);
        mix_i32(s.no_match);
        mix_i32(s.finished);
        mix_i32(s.tracked_keys);
        mix_i32(s.reacquiring);
        mix_i32(s.thermal_generation);
        mix_i32((int32_t)s.start_ms);
        mix_i32((int32_t)s.expire_from_ms);
        mix_i32((int32_t)s.last_adoption_ms);
        mix_i32(s.suppress_window);
        for (unsigned r = 0; r < 6; r++)
            mix_i32(s.suppress_mask[r]);
        for (unsigned i = 0; i < 112; i++) {
            mix_i32(s.watch[i].initial_delta);
            mix_i32((int32_t)s.watch[i].since_ms);
            mix_i32((int32_t)s.watch[i].raw_sum);
            mix_i32(s.watch[i].samples);
            mix_i32(s.watch[i].reacquire);
        }
        mix_i32((int32_t)s.last_pressed_ms);
        mix_i32((int32_t)s.last_idle_sample_ms);
        mix_i32(s.idle_next);
        mix_i32(s.idle_count);
        mix_i32(s.rest_saved);
        for (unsigned j = 0; j < 9; j++)
            for (unsigned i = 0; i < 160; i++)
                mix_i32(s.idle[j][i]);
        printf("session.%s %016llx\n", name, (unsigned long long)hash);
    }
}

int main(void) {
    keyframe_case("even", 20);
    keyframe_case("odd", 21);
    thermal_case("ansi", 22, 1, false);
    thermal_case("iso_modules", 23, 2, true);
    thermal_case("jis", 24, 3, false);
    rest_apply_cases(25);
    rest_record_cases(26);
    rest_session_case("a", 27);
    rest_session_case("b", 28);
    return 0;
}
