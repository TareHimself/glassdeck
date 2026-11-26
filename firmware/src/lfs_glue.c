/* The littlefs instance, with the stock firmware's exact configuration (Keyglass lfs_store.c).
 * Flash access is in Rust (storage.rs). One file is open at a time, as in the stock store. */
#include "lfs.h"

#include <stdint.h>

int gd_flash_read(uint32_t block, uint32_t off, void *buffer, uint32_t size);
int gd_flash_prog(uint32_t block, uint32_t off, const void *buffer, uint32_t size);
int gd_flash_erase(uint32_t block);

enum { BLOCK_SIZE = 4096, PAGE_SIZE = 256, LOOKAHEAD = 16 };

static int read(const struct lfs_config *c, lfs_block_t block, lfs_off_t off, void *buffer, lfs_size_t size) {
    (void)c;
    return gd_flash_read(block, off, buffer, size);
}
static int prog(const struct lfs_config *c, lfs_block_t block, lfs_off_t off, const void *buffer, lfs_size_t size) {
    (void)c;
    return gd_flash_prog(block, off, buffer, size);
}
static int erase(const struct lfs_config *c, lfs_block_t block) {
    (void)c;
    return gd_flash_erase(block);
}
static int sync(const struct lfs_config *c) {
    (void)c;
    return LFS_ERR_OK;
}

static uint8_t g_read_buf[BLOCK_SIZE];
static uint8_t g_prog_buf[BLOCK_SIZE];
static uint8_t g_lookahead_buf[LOOKAHEAD];
/* The stock store lets littlefs malloc this per open file; one static buffer is the same size. */
static uint8_t g_file_buf[BLOCK_SIZE];

static struct lfs_config g_cfg = {
    .read = read,
    .prog = prog,
    .erase = erase,
    .sync = sync,
    .read_size = 1,
    .prog_size = PAGE_SIZE,
    .block_size = BLOCK_SIZE,
    .cache_size = BLOCK_SIZE,
    .lookahead_size = LOOKAHEAD,
    .block_cycles = 500,
    .read_buffer = g_read_buf,
    .prog_buffer = g_prog_buf,
    .lookahead_buffer = g_lookahead_buf,
};

static lfs_t g_lfs;
static lfs_file_t g_file;
static const struct lfs_file_config g_file_cfg = {.buffer = g_file_buf};

int gd_lfs_mount(uint32_t block_count) {
    g_cfg.block_count = block_count;
    return lfs_mount(&g_lfs, &g_cfg);
}

int gd_lfs_open(const char *path, int flags) {
    return lfs_file_opencfg(&g_lfs, &g_file, path, flags, &g_file_cfg);
}

int32_t gd_lfs_size(void) {
    return lfs_file_size(&g_lfs, &g_file);
}

int32_t gd_lfs_read(void *buffer, uint32_t size) {
    return lfs_file_read(&g_lfs, &g_file, buffer, size);
}

int32_t gd_lfs_write(const void *buffer, uint32_t size) {
    return lfs_file_write(&g_lfs, &g_file, buffer, size);
}

int gd_lfs_close(void) {
    return lfs_file_close(&g_lfs, &g_file);
}

int gd_lfs_rename(const char *from, const char *to) {
    return lfs_rename(&g_lfs, from, to);
}

int gd_lfs_remove(const char *path) {
    return lfs_remove(&g_lfs, path);
}

/* The three string functions littlefs uses that the Rust runtime doesn't provide. */
#include <stddef.h>
#include <string.h>

size_t strspn(const char *s, const char *accept) {
    size_t n = 0;
    while (s[n] && strchr(accept, s[n]))
        n++;
    return n;
}

size_t strcspn(const char *s, const char *reject) {
    size_t n = 0;
    while (s[n] && !strchr(reject, s[n]))
        n++;
    return n;
}

char *strchr(const char *s, int c) {
    for (;; s++) {
        if (*s == (char)c)
            return (char *)s;
        if (!*s)
            return NULL;
    }
}
