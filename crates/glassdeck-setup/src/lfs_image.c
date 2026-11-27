/* littlefs over a flash image in memory, with the key scanner's configuration (see
 * firmware/src/lfs_glue.c). Reads a backup's partition and builds test images. */
#include "lfs.h"

#include <stdint.h>
#include <string.h>

enum { BLOCK_SIZE = 4096, PAGE_SIZE = 256, LOOKAHEAD = 16 };

static uint8_t *g_image;
static uint32_t g_blocks;

static int read(const struct lfs_config *c, lfs_block_t block, lfs_off_t off, void *buffer, lfs_size_t size) {
    (void)c;
    if (block >= g_blocks || off + size > BLOCK_SIZE)
        return LFS_ERR_IO;
    memcpy(buffer, g_image + (size_t)block * BLOCK_SIZE + off, size);
    return LFS_ERR_OK;
}
static int prog(const struct lfs_config *c, lfs_block_t block, lfs_off_t off, const void *buffer, lfs_size_t size) {
    (void)c;
    if (block >= g_blocks || off + size > BLOCK_SIZE)
        return LFS_ERR_IO;
    memcpy(g_image + (size_t)block * BLOCK_SIZE + off, buffer, size);
    return LFS_ERR_OK;
}
static int erase(const struct lfs_config *c, lfs_block_t block) {
    (void)c;
    if (block >= g_blocks)
        return LFS_ERR_IO;
    memset(g_image + (size_t)block * BLOCK_SIZE, 0xFF, BLOCK_SIZE);
    return LFS_ERR_OK;
}
static int sync(const struct lfs_config *c) {
    (void)c;
    return LFS_ERR_OK;
}

static uint8_t g_read_buf[BLOCK_SIZE];
static uint8_t g_prog_buf[BLOCK_SIZE];
static uint8_t g_lookahead_buf[LOOKAHEAD];
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
static const struct lfs_file_config g_file_cfg = {.buffer = g_file_buf};

int gd_image_mount(uint8_t *image, uint32_t blocks) {
    g_image = image;
    g_blocks = blocks;
    g_cfg.block_count = blocks;
    return lfs_mount(&g_lfs, &g_cfg);
}

int gd_image_format(uint8_t *image, uint32_t blocks) {
    g_image = image;
    g_blocks = blocks;
    g_cfg.block_count = blocks;
    memset(image, 0xFF, (size_t)blocks * BLOCK_SIZE);
    return lfs_format(&g_lfs, &g_cfg);
}

int gd_image_unmount(void) {
    return lfs_unmount(&g_lfs);
}

/* Reads a whole file into buffer; returns its size, or a negative littlefs error. */
int32_t gd_image_read(const char *path, uint8_t *buffer, uint32_t capacity) {
    lfs_file_t file;
    int rc = lfs_file_opencfg(&g_lfs, &file, path, LFS_O_RDONLY, &g_file_cfg);
    if (rc < 0)
        return rc;
    int32_t size = lfs_file_size(&g_lfs, &file);
    if (size >= 0 && (uint32_t)size > capacity)
        size = LFS_ERR_FBIG;
    if (size > 0) {
        int32_t got = lfs_file_read(&g_lfs, &file, buffer, (lfs_size_t)size);
        if (got != size)
            size = got < 0 ? got : LFS_ERR_CORRUPT;
    }
    lfs_file_close(&g_lfs, &file);
    return size;
}

int gd_image_write(const char *path, const uint8_t *data, uint32_t len) {
    lfs_file_t file;
    int rc = lfs_file_opencfg(&g_lfs, &file, path, LFS_O_WRONLY | LFS_O_CREAT | LFS_O_TRUNC, &g_file_cfg);
    if (rc < 0)
        return rc;
    int32_t wrote = lfs_file_write(&g_lfs, &file, data, len);
    rc = lfs_file_close(&g_lfs, &file);
    return wrote < 0 ? wrote : rc;
}
