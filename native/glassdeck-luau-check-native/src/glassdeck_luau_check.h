#pragma once
#include <stddef.h>
#include <stdint.h>

#if defined(_MSC_VER)
#ifdef GD_LUAU_PRODUCER
#define GD_LUAU_API extern "C" __declspec(dllexport)
#else
#define GD_LUAU_API extern "C" __declspec(dllimport)
#endif
#else
#define GD_LUAU_API extern "C" __attribute__((visibility("default")))
#endif

// Mirrored by `Status` in crates/glassdeck-luau-check/src/lib.rs.
enum GdLuauStatus : int32_t
{
    GD_LUAU_OK = 0,
    GD_LUAU_INVALID_ARGUMENT,
    // A definition file didn't load; the text says why
    GD_LUAU_DEFINITIONS_FAILED,
    GD_LUAU_OUT_OF_MEMORY,
    GD_LUAU_PLATFORM_ERROR,
};

struct GdLuauFile
{
    const char* name;
    const char* text;
};

struct GdLuauText;

// Type-checks `script` in strict mode against the definition files. On GD_LUAU_OK the text holds one
// problem per line as "name:line:column: message", and is empty when the script checks clean.
GD_LUAU_API GdLuauStatus gd_luau_check(
    const GdLuauFile* definitions,
    int32_t count,
    const GdLuauFile* script,
    GdLuauText** outText);

GD_LUAU_API const char* gd_luau_text_data(const GdLuauText* text);
GD_LUAU_API size_t gd_luau_text_size(const GdLuauText* text);
GD_LUAU_API void gd_luau_text_free(GdLuauText* text);
