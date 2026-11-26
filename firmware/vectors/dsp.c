/* Runs Keyglass's key-decision code on generated key presses and prints a hash of every output
 * after every scan. glassdeck-scanner-dsp's tests generate the same presses (keep `Sim` in
 * crates/glassdeck-scanner-dsp/src/testing.rs in step with this file) and must print the same hashes. */
#include "fw_classic_keys.h"
#include "fw_classic_neighbors.h"
#include "fw_classic_row.h"
#include "fw_press_guard.h"
#include "fw_rapid.h"
#include "fw_row_condition.h"

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

static void mix(const void *data, size_t len) {
    const uint8_t *p = data;
    for (size_t i = 0; i < len; i++) {
        hash ^= p[i];
        hash *= 0x100000001b3ULL;
    }
}

static void mix_i32(int32_t v) {
    uint8_t b[4] = {(uint8_t)v, (uint8_t)(v >> 8), (uint8_t)(v >> 16), (uint8_t)(v >> 24)};
    mix(b, 4);
}

static void begin(void) {
    hash = 0xcbf29ce484222325ULL;
}

/* Key travel that wanders between targets, so presses, holds, partial releases and re-presses all happen. */
typedef struct {
    int32_t travel[17], target[17], speed[17];
    int32_t drift;
} sim_t;

static void sim_step(sim_t *s, unsigned keys) {
    for (unsigned k = 0; k < keys; k++) {
        if (pick(40) == 0) {
            s->target[k] = pick(4) == 0 ? 0 : pick(1400);
            s->speed[k] = 20 + pick(120);
        }
        int32_t d = s->target[k] - s->travel[k];
        int32_t step = d > s->speed[k] ? s->speed[k] : d < -s->speed[k] ? -s->speed[k] : d;
        s->travel[k] += step;
    }
    s->drift += pick(3) - 1;
    if (s->drift > 150)
        s->drift = 150;
    if (s->drift < -150)
        s->drift = -150;
}

/* Sensor j sits between keys j-1 and j; a pressed key raises the sensor on its left and lowers
 * the one on its right. */
static void sim_sensors(const sim_t *s, unsigned width, int32_t out[17]) {
    unsigned keys = width - 1;
    for (unsigned j = 0; j < width; j++) {
        int32_t left = j > 0 ? s->travel[j - 1] : 0;
        int32_t right = j < keys ? s->travel[j] : 0;
        out[j] = (right - left) / 2 + pick(61) - 30 + s->drift;
    }
}

static uint32_t random_word(void) {
    uint32_t type = (uint32_t)pick(4);
    uint32_t release = (uint32_t)(40 + pick(400));
    uint32_t actuation = (uint32_t)(300 + pick(900));
    return (type << 30) | (release << 12) | actuation;
}

static void classic_case(const char *name, uint32_t seed, unsigned layout, unsigned row, unsigned width,
                         bool has_table) {
    rng = seed;
    sim_t sim;
    memset(&sim, 0, sizeof(sim));
    fw_classic_settings_t cfg;
    fw_classic_keys_t keys;
    fw_classic_neighbors_t neighbors;
    memset(&cfg, 0, sizeof(cfg));
    memset(&keys, 0, sizeof(keys));
    memset(&neighbors, 0, sizeof(neighbors));
    unsigned n = width - 1;
    for (unsigned c = 0; c < n; c++)
        fw_classic_key_load(&cfg, &keys, layout, row, c, has_table, random_word());
    for (unsigned scan = 0; scan < 800; scan++) {
        if (scan == 400)
            for (unsigned c = 0; c < n; c++)
                fw_classic_key_load(&cfg, &keys, layout, row, c, has_table, random_word());
        if (scan == 600)
            for (unsigned c = 0; c < n; c += 3)
                fw_classic_key_weight(&cfg, layout, row, c);
        sim_step(&sim, n);
        int32_t field[17], travel[16];
        sim_sensors(&sim, width, field);
        fw_classic_row(row, width, field, field, travel);
        fw_classic_neighbors(&neighbors, layout, row, n, field, travel, keys.down, has_table ? cfg.actuation : NULL,
                             has_table ? cfg.hysteresis : NULL);
        unsigned held = 0;
        for (unsigned c = 0; c < n; c++)
            held += keys.down[c];
        fw_classic_keys_step(&keys, &cfg, row, n, held + (unsigned)pick(3), travel);

        begin();
        for (unsigned i = 0; i < width; i++)
            mix_i32(field[i]);
        for (unsigned i = 0; i < n; i++) {
            mix_i32(travel[i]);
            mix_i32(keys.down[i]);
            mix_i32(keys.pending[i]);
            mix_i32(keys.scans[i]);
            mix_i32(keys.reversal[i].peak);
            mix_i32(keys.reversal[i].trough);
            mix_i32(keys.reversal[i].armed);
            mix_i32(keys.reversal[i].proposed);
            mix_i32(keys.reversal[i].scans);
            mix_i32(neighbors.below_release[i]);
            mix_i32(cfg.actuation[i]);
            mix_i32(cfg.hysteresis[i]);
            mix_i32(cfg.sensitivity[i]);
            mix_i32(cfg.rapid[i]);
        }
        printf("classic.%s %016llx\n", name, (unsigned long long)hash);
    }
}

static void rapid_case(const char *name, uint32_t seed, unsigned n) {
    rng = seed;
    sim_t sim;
    memset(&sim, 0, sizeof(sim));
    fw_key_motion_t motion[16];
    fw_rapid_state_t state[16];
    bool down[16], fixed_down[16];
    uint8_t protected_held[16], vetoes[16];
    uint16_t act[16], hyst[16], sens[16], gain[16];
    memset(motion, 0, sizeof(motion));
    memset(state, 0, sizeof(state));
    memset(down, 0, sizeof(down));
    memset(fixed_down, 0, sizeof(fixed_down));
    memset(protected_held, 0, sizeof(protected_held));
    memset(vetoes, 0, sizeof(vetoes));
    for (unsigned k = 0; k < n; k++) {
        gain[k] = (uint16_t)(k % 4 == 0 ? 0 : 200 + pick(60));
        act[k] = (uint16_t)(300 + pick(700));
        hyst[k] = (uint16_t)(20 + pick(300));
        sens[k] = (uint16_t)(20 + pick(200));
    }
    for (unsigned scan = 0; scan < 800; scan++) {
        if (scan == 300)
            for (unsigned k = 0; k < n; k += 2)
                fw_rapid_rearm(&state[k]);
        sim_step(&sim, n);
        uint16_t flux[16];
        for (unsigned k = 0; k < n; k++) {
            int32_t noisy = sim.travel[k] + pick(41) - 20;
            flux[k] = (uint16_t)(noisy < 0 ? 0 : noisy);
        }
        fw_update_row_motion(motion, flux, n);
        bool strong = scan >= 200 && scan < 260;
        uint8_t row_active = 0;
        for (unsigned k = 0; k < n; k++)
            row_active += down[k];
        begin();
        for (unsigned k = 0; k < n; k++) {
            uint16_t guarded = fw_guard_key_signal(flux[k], gain[k], down[k], true, state[k].peak, strong,
                                                   &protected_held[k], scan % 97 == 0);
            int32_t nd = fw_neighbor_delta(motion, k, n);
            fw_rapid_input_t in = {
                .flux = guarded,
                .actuation = act[k],
                .hysteresis = hyst[k],
                .sensitivity = sens[k],
                .weight_q8 = gain[k],
                .row_active = row_active,
                .motion = &motion[k],
                .neighbor_delta = nd,
            };
            down[k] = fw_rapid_down(&state[k], down[k], &in);
            fixed_down[k] = fw_fixed_candidate(fixed_down[k], flux[k], act[k], (int32_t)act[k] - hyst[k], nd,
                                               &motion[k], &vetoes[k]);
            mix_i32(guarded);
            mix_i32(nd);
            mix_i32(down[k]);
            mix_i32(fixed_down[k]);
            mix_i32(protected_held[k]);
            mix_i32(vetoes[k]);
            mix_i32(motion[k].velocity);
            mix_i32(motion[k].delta);
            mix_i32(motion[k].previous_flux);
            mix_i32(motion[k].inhibit_press);
            mix_i32(motion[k].inhibit_release);
            mix_i32(state[k].last_independent_step);
            mix_i32(state[k].peak);
            mix_i32(state[k].trough);
            mix_i32(state[k].independent_scans);
            mix_i32(state[k].neighbor_scans);
            mix_i32(state[k].reversing);
            mix_i32(state[k].pending_down);
            mix_i32(state[k].pending_scans);
        }
        printf("rapid.%s %016llx\n", name, (unsigned long long)hash);
    }
}

static void condition_case(uint32_t seed) {
    rng = seed;
    for (unsigned i = 0; i < 400; i++) {
        uint8_t n = (uint8_t)(1 + pick(17));
        int32_t row[17];
        int32_t base = pick(2000) - 1000, slope = pick(81) - 40;
        unsigned shape = (unsigned)pick(4);
        for (unsigned c = 0; c < n; c++) {
            row[c] = shape == 0 ? base : base + slope * (int32_t)c + pick(41) - 20;
            if (shape == 2 && pick(4) == 0)
                row[c] += pick(3000) - 1500;
        }
        fw_condition_row_q4(row, n);
        begin();
        for (unsigned c = 0; c < n; c++)
            mix_i32(row[c]);
        printf("condition %016llx\n", (unsigned long long)hash);
    }
}

int main(void) {
    classic_case("ansi_top", 1, 1, 0, 17, true);
    classic_case("iso_row3", 2, 2, 3, 16, true);
    classic_case("jis_space", 3, 3, 5, 14, true);
    classic_case("ansi_space", 4, 1, 5, 12, true);
    classic_case("no_table", 5, 2, 2, 16, false);
    rapid_case("full_row", 6, 16);
    rapid_case("short_row", 7, 5);
    condition_case(8);
    return 0;
}
