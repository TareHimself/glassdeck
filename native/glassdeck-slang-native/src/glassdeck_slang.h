#pragma once
#include <stddef.h>
#include <stdint.h>

#if defined(_MSC_VER)
#ifdef GD_SLANG_PRODUCER
#define GD_SLANG_API extern "C" __declspec(dllexport)
#else
#define GD_SLANG_API extern "C" __declspec(dllimport)
#endif
#else
#define GD_SLANG_API extern "C" __attribute__((visibility("default")))
#endif

// Mirrored by `Status` in crates/glassdeck-slang/src/native.rs. CompileFailed, EntryPointNotFound
// and LinkFailed describe the shader, with Slang's output in the diagnostics text.
enum GdStatus : int32_t
{
    GD_OK = 0,
    GD_INVALID_ARGUMENT,
    GD_COMPILE_FAILED,
    GD_ENTRY_POINT_NOT_FOUND,
    GD_LINK_FAILED,
    GD_SLANG_ERROR,
    GD_OUT_OF_MEMORY,
    GD_PLATFORM_ERROR,
};

enum GdTarget : int32_t
{
    GD_TARGET_SPIRV = 0,
    GD_TARGET_GLSL = 1,
};

// A module given as source text. Modules load in order, so a module may import any before it.
struct GdSource
{
    const char* name;
    const char* path;
    const char* text;
};

struct GdText;

// Calls share one Slang global session and take turns, so any thread may call.

// Describes the last module's structs and functions that carry user attributes, as JSON.
GD_SLANG_API GdStatus gd_slang_reflect(const GdSource* sources, int32_t count, GdText** outJson, GdText** outDiagnostics);

// Compiles the entry point `entry` of the last module for `target` with Slang profile `profile`.
GD_SLANG_API GdStatus gd_slang_compile(
    const GdSource* sources,
    int32_t count,
    const char* entry,
    GdTarget target,
    const char* profile,
    GdText** outCode,
    GdText** outDiagnostics);

GD_SLANG_API int32_t gd_slang_has_profile(const char* name);

GD_SLANG_API const char* gd_text_data(const GdText* text);
GD_SLANG_API size_t gd_text_size(const GdText* text);
GD_SLANG_API void gd_text_free(GdText* text);
