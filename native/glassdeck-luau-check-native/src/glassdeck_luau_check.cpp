#include "glassdeck_luau_check.h"

#include "Luau/BuiltinDefinitions.h"
#include "Luau/Frontend.h"
#include "Luau/ToString.h"

#include <new>
#include <optional>
#include <string>

struct GdLuauText
{
    std::string data;
};

namespace
{

struct OneScript : Luau::FileResolver
{
    std::string name;
    std::string source;

    std::optional<Luau::SourceCode> readSource(const Luau::ModuleName& module) override
    {
        if (module != name) return std::nullopt;
        return Luau::SourceCode{source, Luau::SourceCode::Module};
    }
};

struct Strict : Luau::ConfigResolver
{
    Luau::Config config;

    Strict()
    {
        config.mode = Luau::Mode::Strict;
    }

    const Luau::Config& getConfig(const Luau::ModuleName&, const Luau::TypeCheckLimits&) const override
    {
        return config;
    }
};

std::string describe(const Luau::TypeError& error)
{
    return error.moduleName + ":" + std::to_string(error.location.begin.line + 1) + ":" +
           std::to_string(error.location.begin.column + 1) + ": " + Luau::toString(error) + "\n";
}

} // namespace

GdLuauStatus gd_luau_check(const GdLuauFile* definitions, int32_t count, const GdLuauFile* script, GdLuauText** outText)
{
    if (!outText || !script || !script->name || !script->text || count < 0 || (count > 0 && !definitions)) return GD_LUAU_INVALID_ARGUMENT;
    *outText = nullptr;
    try
    {
        OneScript files;
        files.name = script->name;
        files.source = script->text;
        Strict configs;
        Luau::Frontend frontend(Luau::SolverMode::New, &files, &configs);
        Luau::registerBuiltinGlobals(frontend, frontend.globals);

        std::string problems;
        for (int32_t i = 0; i < count; ++i)
        {
            if (!definitions[i].name || !definitions[i].text) return GD_LUAU_INVALID_ARGUMENT;
            Luau::LoadDefinitionFileResult loaded = frontend.loadDefinitionFile(
                frontend.globals, frontend.globals.globalScope, definitions[i].text, definitions[i].name, false, false);
            if (!loaded.success)
            {
                for (const Luau::ParseError& error : loaded.parseResult.errors)
                    problems += std::string(definitions[i].name) + ":" + std::to_string(error.getLocation().begin.line + 1) + ": " +
                                error.getMessage() + "\n";
                if (loaded.module)
                    for (const Luau::TypeError& error : loaded.module->errors)
                        problems += describe(error);
                *outText = new GdLuauText{problems.empty() ? std::string(definitions[i].name) + " didn't load\n" : problems};
                return GD_LUAU_DEFINITIONS_FAILED;
            }
        }
        Luau::freeze(frontend.globals.globalTypes);

        Luau::CheckResult result = frontend.check(files.name);
        for (const Luau::TypeError& error : result.errors)
            problems += describe(error);
        *outText = new GdLuauText{problems};
        return GD_LUAU_OK;
    }
    catch (const std::bad_alloc&)
    {
        return GD_LUAU_OUT_OF_MEMORY;
    }
    catch (...)
    {
        return GD_LUAU_PLATFORM_ERROR;
    }
}

const char* gd_luau_text_data(const GdLuauText* text)
{
    return text ? text->data.data() : nullptr;
}

size_t gd_luau_text_size(const GdLuauText* text)
{
    return text ? text->data.size() : 0;
}

void gd_luau_text_free(GdLuauText* text)
{
    delete text;
}
