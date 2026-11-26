#include "fw_scanner.h"

#include <stdio.h>
#include <string.h>

typedef struct {
    uint32_t layers, keys, modes;
    uint32_t macros;
    const uint32_t *macro_lens;
} section_spec_t;

static uint32_t blob[FW_MAX_KEYMAP_BYTES / 4 + 8];

static size_t put_section(uint32_t *w, const section_spec_t *s, uint32_t seed) {
    uint32_t pool = 0;
    if (s->macros) {
        pool = s->macros;
        for (uint32_t m = 0; m < s->macros; m++)
            pool += 1 + s->macro_lens[m];
    }
    uint32_t kc = pool;
    uint32_t act = kc + s->keys * s->layers;
    w[0] = s->layers;
    w[1] = s->keys;
    w[2] = s->modes;
    w[3] = s->macros;
    w[4] = s->macros;
    w[5] = kc;
    w[6] = act;
    uint32_t *p = w + 7;
    uint32_t at = s->macros;
    for (uint32_t m = 0; m < s->macros; m++) {
        p[m] = at;
        p[at] = s->macro_lens[m];
        for (uint32_t i = 0; i < s->macro_lens[m]; i++)
            p[at + 1 + i] = ((i & 1U) << 31) | ((10U * i + m) << 8) | (0x04U + i);
        at += 1 + s->macro_lens[m];
    }
    static const uint32_t kinds[] = {0x00020004U, 0x10000000U, 0x21000001U, 0x400000E9U, 0x50000004U,
                                     0xF0000000U, 0x00000000U, 0x50000000U, 0x32000001U};
    for (uint32_t i = 0; i < s->keys * s->layers; i++)
        p[kc + i] = kinds[(i + seed) % (sizeof(kinds) / sizeof(kinds[0]))];
    for (uint32_t i = 0; i < s->keys * s->modes; i++)
        p[act + i] = ((i % 4U) << 30) | (((i * 7U + seed) & 0xFFFU) << 12) | ((i * 13U + 600U) & 0xFFFU);
    return 7 + act + s->keys * s->modes;
}

/* Lays out a blob with the given sections (NULL for an absent layout). Returns its length in words. */
static size_t build(const section_spec_t *sections[3]) {
    memset(blob, 0, sizeof(blob));
    uint32_t count = 0, mask = 0, off = 0;
    size_t pos = 6;
    for (unsigned l = 0; l < 3; l++) {
        if (!sections[l])
            continue;
        count++;
        mask |= 1U << l;
        blob[3 + l] = off;
        size_t n = put_section(blob + pos, sections[l], l * 5U);
        pos += n;
        off += (uint32_t)n;
    }
    blob[0] = count;
    blob[1] = mask;
    return pos;
}

static void print_case(const char *name, size_t len_bytes) {
    printf("keymap.%s ", name);
    const uint8_t *b = (const uint8_t *)blob;
    for (size_t i = 0; i < len_bytes; i++)
        printf("%02x", b[i]);
    static fw_runtime_t rt;
    memset(&rt, 0, sizeof(rt));
    int ok = fw_runtime_load_keymap(&rt, b, len_bytes) == 0;
    printf("\nkeymap.%s.view", name);
    if (!ok) {
        printf(" refused\n");
        return;
    }
    for (unsigned lid = 1; lid <= 3; lid++) {
        const fw_layout_view_t *v = &rt.layouts[lid];
        if (!v->valid) {
            printf(" -");
            continue;
        }
        printf(" %u:%u:%u:%u:%u:%u:%u", v->num_layers, v->num_keys, v->num_act_modes, v->num_macros,
               v->macro_pool_words, v->keycodes_word_off, v->actuations_word_off);
    }
    printf("\n");
}

static void profile_case(const char *name, const uint8_t *xfer, size_t len) {
    printf("profiles.%s ", name);
    for (size_t i = 0; i < len; i++)
        printf("%02x", xfer[i]);
    static fw_runtime_t rt;
    memset(&rt, 0, sizeof(rt));
    printf("\nprofiles.%s.export ", name);
    if (fw_runtime_load_profile_settings(&rt, xfer, len) != 0) {
        printf("refused\n");
        return;
    }
    static uint8_t out[16 + FW_MAX_PROFILES * 8];
    size_t out_len = 0;
    if (fw_runtime_export_profile_settings(&rt, out, sizeof(out), &out_len) != 0) {
        printf("export-failed\n");
        return;
    }
    for (size_t i = 0; i < out_len; i++)
        printf("%02x", out[i]);
    printf("\n");
}

int main(void) {
    static const uint32_t lens[] = {3, 0, 5};
    const section_spec_t ansi = {2, 85, 2, 0, NULL};
    const section_spec_t iso = {4, 87, 3, 3, lens};
    const section_spec_t jis = {8, 96, 8, 1, lens};
    const section_spec_t *only_ansi[3] = {&ansi, NULL, NULL};
    const section_spec_t *all[3] = {&ansi, &iso, &jis};
    const section_spec_t *iso_jis[3] = {NULL, &iso, &jis};

    size_t n = build(only_ansi);
    print_case("ansi", n * 4);
    n = build(iso_jis);
    print_case("iso_jis", n * 4);
    size_t full = build(all);
    print_case("all", full * 4);
    size_t iso_at = 6 + blob[4];

#define MUTATE(name, len, stmt)                                                                                         \
    do {                                                                                                               \
        build(all);                                                                                                    \
        stmt;                                                                                                          \
        print_case(name, len);                                                                                         \
    } while (0)

    MUTATE("odd_length", full * 4 + 1, (void)0);
    MUTATE("short", 20, (void)0);
    MUTATE("trailing_word", full * 4 + 4, blob[full] = 0xABCDU);
    MUTATE("truncated", full * 4 - 4, (void)0);
    MUTATE("no_layouts", full * 4, blob[0] = 0);
    MUTATE("four_layouts", full * 4, blob[0] = 4);
    MUTATE("count_mismatch", full * 4, blob[0] = 2);
    MUTATE("word2_set", full * 4, blob[2] = 1);
    MUTATE("wrong_offset", full * 4, blob[4] += 1);
    MUTATE("extra_mask_bit", full * 4, blob[1] |= 8U);
    MUTATE("absent_with_offset", full * 4, (blob[1] = 1U, blob[0] = 1U, blob[4] = 9U));
    MUTATE("zero_layers", full * 4, blob[iso_at + 0] = 0);
    MUTATE("nine_layers", full * 4, blob[iso_at + 0] = 9);
    MUTATE("zero_keys", full * 4, blob[iso_at + 1] = 0);
    MUTATE("too_many_keys", full * 4, blob[iso_at + 1] = 97);
    MUTATE("zero_modes", full * 4, blob[iso_at + 2] = 0);
    MUTATE("nine_modes", full * 4, blob[iso_at + 2] = 9);
    MUTATE("too_many_macros", full * 4, blob[iso_at + 3] = 513);
    MUTATE("macro_count_copy_ignored", full * 4, blob[iso_at + 4] = 77);
    MUTATE("act_offset_wrong", full * 4, blob[iso_at + 6] += 1);
    MUTATE("kc_below_macros", full * 4, blob[iso_at + 5] = 2);
    MUTATE("macro_offset_wrong", full * 4, blob[iso_at + 7 + 1] += 1);
    MUTATE("macro_too_long", full * 4, blob[iso_at + 7 + 3] = 50);
    MUTATE("macro_pool_gap", full * 4, blob[iso_at + 7 + 3] = 2);
    MUTATE("kc_without_macros", full * 4, (blob[6 + 0 + 5] = 1, blob[6 + 0 + 6] += 1));

    uint8_t xfer[32 + 8 * 4];
    memset(xfer, 0, sizeof(xfer));
    xfer[16] = 3;
    for (unsigned i = 0; i < 3; i++) {
        xfer[32 + 8 * i] = (uint8_t)(i + 1);
        xfer[32 + 8 * i + 4] = (uint8_t)(1U << i);
    }
    profile_case("three", xfer, 32 + 24);
    xfer[0] = 9;
    xfer[10] = 9;
    profile_case("version_and_reserved_ignored", xfer, 32 + 24);
    xfer[0] = 0;
    xfer[10] = 0;
    xfer[4] = 1;
    profile_case("wrong_type", xfer, 32 + 24);
    xfer[4] = 0;
    profile_case("wrong_length", xfer, 32 + 32);
    profile_case("short", xfer, 31);
    xfer[16] = 0;
    profile_case("none", xfer, 32);
    xfer[16] = 65;
    profile_case("too_many", xfer, 32);
    return 0;
}
