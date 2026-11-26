#include "glassdeck_slang.h"

#include <slang-com-ptr.h>
#include <slang.h>

#include <cstdio>
#include <mutex>
#include <new>
#include <string>
#include <vector>

struct GdText
{
    std::string data;
};

namespace
{

struct Failure
{
    GdStatus status;
};

void require(bool condition, GdStatus status)
{
    if (!condition) throw Failure{status};
}

// Loading Slang's core module is most of a compile's cost, so one global session serves every call.
// Slang's sessions aren't thread-safe, so calls take turns.
std::mutex& slangTurn()
{
    static std::mutex turn;
    return turn;
}

// Never released: it lives as long as the library, and tearing it down at exit gains nothing.
slang::IGlobalSession* globalSession()
{
    static slang::IGlobalSession* global = nullptr;
    if (!global) require(SLANG_SUCCEEDED(slang::createGlobalSession(&global)), GD_SLANG_ERROR);
    return global;
}

template <typename F> GdStatus guarded(F&& body) noexcept
{
    try
    {
        std::lock_guard<std::mutex> turn(slangTurn());
        body();
        return GD_OK;
    }
    catch (const Failure& failure)
    {
        return failure.status;
    }
    catch (const std::bad_alloc&)
    {
        return GD_OUT_OF_MEMORY;
    }
    catch (...)
    {
        return GD_PLATFORM_ERROR;
    }
}

GdText* text(std::string data)
{
    return new GdText{std::move(data)};
}

void appendDiagnostics(std::string& to, slang::IBlob* blob)
{
    if (blob) to.append(static_cast<const char*>(blob->getBufferPointer()), blob->getBufferSize());
}

struct Compilation
{
    Slang::ComPtr<slang::ISession> session;
    std::vector<slang::IModule*> modules;
    std::string diagnostics;

    Compilation(const GdSource* sources, int32_t count, const slang::TargetDesc* target)
    {
        require(sources != nullptr && count > 0, GD_INVALID_ARGUMENT);
        slang::SessionDesc desc{};
        desc.targets = target;
        desc.targetCount = target ? 1 : 0;
        require(SLANG_SUCCEEDED(globalSession()->createSession(desc, session.writeRef())), GD_SLANG_ERROR);
        for (int32_t i = 0; i < count; ++i)
        {
            require(sources[i].name && sources[i].path && sources[i].text, GD_INVALID_ARGUMENT);
            Slang::ComPtr<slang::IBlob> blob;
            slang::IModule* module = session->loadModuleFromSourceString(sources[i].name, sources[i].path, sources[i].text, blob.writeRef());
            appendDiagnostics(diagnostics, blob);
            if (!module) return;
            modules.push_back(module);
        }
    }

    bool loaded(int32_t count) const
    {
        return static_cast<int32_t>(modules.size()) == count;
    }
};

std::string quoted(const char* value, size_t size)
{
    std::string out = "\"";
    for (size_t i = 0; i < size; ++i)
    {
        const char c = value[i];
        switch (c)
        {
        case '"': out += "\\\""; break;
        case '\\': out += "\\\\"; break;
        case '\n': out += "\\n"; break;
        default:
            if (static_cast<unsigned char>(c) < 0x20)
            {
                char escaped[8];
                std::snprintf(escaped, sizeof escaped, "\\u%04x", c);
                out += escaped;
            }
            else
            {
                out += c;
            }
        }
    }
    return out + "\"";
}

std::string quoted(const char* value)
{
    return value ? quoted(value, std::char_traits<char>::length(value)) : "null";
}

const char* scalarName(slang::TypeReflection::ScalarType scalar)
{
    using S = slang::TypeReflection::ScalarType;
    switch (scalar)
    {
    case S::Bool: return "bool";
    case S::Int32: return "int";
    case S::UInt32: return "uint";
    case S::Float16: return "half";
    case S::Float32: return "float";
    default: return "?";
    }
}

std::string typeName(slang::TypeReflection* type)
{
    using K = slang::TypeReflection::Kind;
    switch (type->getKind())
    {
    case K::Scalar: return scalarName(type->getScalarType());
    case K::Vector: return scalarName(type->getElementType()->getScalarType()) + std::to_string(type->getElementCount());
    case K::Matrix:
        return scalarName(type->getElementType()->getScalarType()) + std::to_string(type->getRowCount()) + "x" +
               std::to_string(type->getColumnCount());
    case K::Array: return typeName(type->getElementType()) + "[" + std::to_string(type->getElementCount()) + "]";
    default:
    {
        Slang::ComPtr<slang::IBlob> name;
        if (SLANG_SUCCEEDED(type->getFullName(name.writeRef())))
            return std::string(static_cast<const char*>(name->getBufferPointer()), name->getBufferSize());
        return type->getName() ? type->getName() : "?";
    }
    }
}

std::string argument(slang::Attribute* attribute, uint32_t index)
{
    slang::TypeReflection* type = attribute->getArgumentType(index);
    if (type && type->getKind() == slang::TypeReflection::Kind::Scalar)
    {
        const auto scalar = type->getScalarType();
        if (scalar == slang::TypeReflection::ScalarType::Float32 || scalar == slang::TypeReflection::ScalarType::Float16)
        {
            float value = 0;
            if (SLANG_SUCCEEDED(attribute->getArgumentValueFloat(index, &value))) return std::to_string(value);
        }
        int value = 0;
        if (SLANG_SUCCEEDED(attribute->getArgumentValueInt(index, &value))) return std::to_string(value);
    }
    size_t size = 0;
    if (const char* value = attribute->getArgumentValueString(index, &size)) return quoted(value, size);
    return "null";
}

template <typename GetCount, typename GetAt> std::string attributes(GetCount count, GetAt at)
{
    std::string out = "[";
    for (unsigned int i = 0; i < count(); ++i)
    {
        slang::Attribute* attribute = at(i);
        if (i) out += ",";
        out += "{\"name\":" + quoted(attribute->getName()) + ",\"args\":[";
        for (uint32_t a = 0; a < attribute->getArgumentCount(); ++a)
        {
            if (a) out += ",";
            out += argument(attribute, a);
        }
        out += "]}";
    }
    return out + "]";
}

std::string function(slang::FunctionReflection* fn)
{
    std::string out = "{\"kind\":\"function\",\"name\":" + quoted(fn->getName()) + ",\"returns\":" + quoted(typeName(fn->getReturnType()).c_str()) +
                      ",\"params\":[";
    for (unsigned int i = 0; i < fn->getParameterCount(); ++i)
    {
        slang::VariableReflection* param = fn->getParameterByIndex(i);
        if (i) out += ",";
        out += "{\"name\":" + quoted(param->getName()) + ",\"type\":" + quoted(typeName(param->getType()).c_str()) + "}";
    }
    out += "],\"attributes\":" +
           attributes([&] { return fn->getUserAttributeCount(); }, [&](unsigned int i) { return fn->getUserAttributeByIndex(i); });
    return out + "}";
}

std::string structure(slang::DeclReflection* decl)
{
    slang::TypeReflection* type = decl->getType();
    std::string out = "{\"kind\":\"struct\",\"name\":" + quoted(decl->getName()) + ",\"fields\":[";
    for (unsigned int i = 0; i < type->getFieldCount(); ++i)
    {
        slang::VariableReflection* field = type->getFieldByIndex(i);
        if (i) out += ",";
        out += "{\"name\":" + quoted(field->getName()) + ",\"type\":" + quoted(typeName(field->getType()).c_str()) +
               ",\"default\":" + (field->hasDefaultValue() ? "true" : "false") + "}";
    }
    out += "],\"attributes\":" +
           attributes([&] { return type->getUserAttributeCount(); }, [&](unsigned int i) { return type->getUserAttributeByIndex(i); });
    return out + "}";
}

std::string variable(slang::VariableReflection* var)
{
    return "{\"kind\":\"variable\",\"name\":" + quoted(var->getName()) + ",\"type\":" + quoted(typeName(var->getType()).c_str()) +
           ",\"attributes\":" +
           attributes([&] { return var->getUserAttributeCount(); }, [&](unsigned int i) { return var->getUserAttributeByIndex(i); }) + "}";
}

std::string declarations(slang::DeclReflection* module)
{
    std::string out = "[";
    bool first = true;
    for (unsigned int i = 0; i < module->getChildrenCount(); ++i)
    {
        slang::DeclReflection* child = module->getChild(i);
        std::string entry;
        switch (child->getKind())
        {
        case slang::DeclReflection::Kind::Func: entry = function(child->asFunction()); break;
        case slang::DeclReflection::Kind::Struct: entry = structure(child); break;
        case slang::DeclReflection::Kind::Variable: entry = variable(child->asVariable()); break;
        default: continue;
        }
        if (!first) out += ",";
        first = false;
        out += entry;
    }
    return out + "]";
}

} // namespace

GdStatus gd_slang_reflect(const GdSource* sources, int32_t count, GdText** outJson, GdText** outDiagnostics)
{
    return guarded([&] {
        require(outJson && outDiagnostics, GD_INVALID_ARGUMENT);
        *outJson = nullptr;
        *outDiagnostics = nullptr;
        Compilation compilation(sources, count, nullptr);
        *outDiagnostics = text(compilation.diagnostics);
        require(compilation.loaded(count), GD_COMPILE_FAILED);
        *outJson = text(declarations(compilation.modules.back()->getModuleReflection()));
    });
}

GdStatus gd_slang_compile(
    const GdSource* sources,
    int32_t count,
    const char* entry,
    GdTarget target,
    const char* profile,
    GdText** outCode,
    GdText** outDiagnostics)
{
    return guarded([&] {
        require(entry && profile && outCode && outDiagnostics, GD_INVALID_ARGUMENT);
        require(target == GD_TARGET_SPIRV || target == GD_TARGET_GLSL, GD_INVALID_ARGUMENT);
        *outCode = nullptr;
        *outDiagnostics = nullptr;

        slang::TargetDesc desc{};
        desc.format = target == GD_TARGET_SPIRV ? SLANG_SPIRV : SLANG_GLSL;
        desc.profile = globalSession()->findProfile(profile);
        require(desc.profile != SLANG_PROFILE_UNKNOWN, GD_INVALID_ARGUMENT);

        Compilation compilation(sources, count, &desc);
        *outDiagnostics = text(compilation.diagnostics);
        require(compilation.loaded(count), GD_COMPILE_FAILED);

        Slang::ComPtr<slang::IEntryPoint> entryPoint;
        require(SLANG_SUCCEEDED(compilation.modules.back()->findEntryPointByName(entry, entryPoint.writeRef())), GD_ENTRY_POINT_NOT_FOUND);

        std::vector<slang::IComponentType*> parts(compilation.modules.begin(), compilation.modules.end());
        parts.push_back(entryPoint);
        Slang::ComPtr<slang::IComponentType> composed;
        Slang::ComPtr<slang::IBlob> blob;
        const auto composedResult = compilation.session->createCompositeComponentType(
            parts.data(), static_cast<SlangInt>(parts.size()), composed.writeRef(), blob.writeRef());
        appendDiagnostics((*outDiagnostics)->data, blob);
        require(SLANG_SUCCEEDED(composedResult), GD_LINK_FAILED);

        Slang::ComPtr<slang::IComponentType> linked;
        blob = nullptr;
        const auto linkResult = composed->link(linked.writeRef(), blob.writeRef());
        appendDiagnostics((*outDiagnostics)->data, blob);
        require(SLANG_SUCCEEDED(linkResult), GD_LINK_FAILED);

        Slang::ComPtr<slang::IBlob> code;
        blob = nullptr;
        const auto codeResult = linked->getEntryPointCode(0, 0, code.writeRef(), blob.writeRef());
        appendDiagnostics((*outDiagnostics)->data, blob);
        require(SLANG_SUCCEEDED(codeResult), GD_COMPILE_FAILED);
        *outCode = text(std::string(static_cast<const char*>(code->getBufferPointer()), code->getBufferSize()));
    });
}

int32_t gd_slang_has_profile(const char* name)
{
    bool found = false;
    const GdStatus status = guarded([&] {
        require(name != nullptr, GD_INVALID_ARGUMENT);
        found = globalSession()->findProfile(name) != SLANG_PROFILE_UNKNOWN;
    });
    return status == GD_OK && found;
}

const char* gd_text_data(const GdText* value)
{
    return value ? value->data.data() : nullptr;
}

size_t gd_text_size(const GdText* value)
{
    return value ? value->data.size() : 0;
}

void gd_text_free(GdText* value)
{
    delete value;
}
