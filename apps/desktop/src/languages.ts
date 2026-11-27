import type * as Monaco from "monaco-editor";

type Grammar = Monaco.languages.IMonarchLanguage;
type Configuration = Monaco.languages.LanguageConfiguration;

export const LUAU = "luau";
export const SLANG = "slang";

const IDENT = /[A-Za-z_]\w*/;

const luauGrammar: Grammar = {
  defaultToken: "",
  keywords: [
    "and", "break", "continue", "do", "else", "elseif", "end", "export", "false", "for", "function", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "type", "typeof", "until", "while",
  ],
  builtinTypes: ["any", "boolean", "buffer", "never", "nil", "number", "string", "thread", "unknown", "vector"],
  tokenizer: {
    root: [
      [/--\[(=*)\[/, { token: "comment", next: "@longComment.$1" }],
      [/--.*$/, "comment"],
      [/\[(=*)\[/, { token: "string", next: "@longString.$1" }],
      [/"/, { token: "string", next: "@doubleQuoted" }],
      [/'/, { token: "string", next: "@singleQuoted" }],
      [/`/, { token: "string", next: "@interpolated" }],
      [/0[xX][\da-fA-F_]+/, "number"],
      [/0[bB][01_]+/, "number"],
      [/(\d[\d_]*)?\.?\d[\d_]*([eE][-+]?\d+)?/, "number"],
      // `obj:method(...)` is a call; `name: Type` and `-> Type` are annotations
      [/(:)(\s*)([A-Za-z_]\w*)(?=\s*[({"'`[])/, ["delimiter", "", "identifier"]],
      [/(::|:|->)(\s*)/, { token: "delimiter", next: "@typeAnnotation" }],
      [/(type)(\s+)([A-Za-z_]\w*)/, ["keyword", "", "type"]],
      [IDENT, { cases: { "@keywords": "keyword", "@default": "identifier" } }],
      [/\.\.\.|\.\.=?|\/\/=?|[-+*/%^]=|==|~=|<=|>=|[-+*/%^#<>=]/, "delimiter"],
      [/[{}()[\],.;]/, "delimiter"],
      [/\s+/, ""],
    ],
    typeAnnotation: [
      [/\s+/, ""],
      [/[A-Za-z_][\w.]*/, { cases: { "@builtinTypes": "type", "@keywords": { token: "keyword", next: "@pop" }, "@default": "type" } }],
      [/[<>?|&,]/, "delimiter"],
      [/[{(]/, { token: "delimiter", next: "@pop" }],
      [/./, { token: "@rematch", next: "@pop" }],
    ],
    longComment: [
      [/\](=*)\]/, { cases: { "$1==$S2": { token: "comment", next: "@pop" }, "@default": "comment" } }],
      [/[^\]]+/, "comment"],
      [/./, "comment"],
    ],
    longString: [
      [/\](=*)\]/, { cases: { "$1==$S2": { token: "string", next: "@pop" }, "@default": "string" } }],
      [/[^\]]+/, "string"],
      [/./, "string"],
    ],
    doubleQuoted: [
      [/\\./, "string.escape"],
      [/"/, { token: "string", next: "@pop" }],
      [/[^\\"]+/, "string"],
    ],
    singleQuoted: [
      [/\\./, "string.escape"],
      [/'/, { token: "string", next: "@pop" }],
      [/[^\\']+/, "string"],
    ],
    interpolated: [
      [/\\./, "string.escape"],
      [/\{/, { token: "delimiter", next: "@interpolation" }],
      [/`/, { token: "string", next: "@pop" }],
      [/[^\\`{]+/, "string"],
    ],
    interpolation: [[/\}/, { token: "delimiter", next: "@pop" }], { include: "root" }],
  },
};

const luauConfiguration: Configuration = {
  comments: { lineComment: "--", blockComment: ["--[[", "]]"] },
  brackets: [
    ["{", "}"],
    ["[", "]"],
    ["(", ")"],
  ],
  autoClosingPairs: [
    { open: "{", close: "}" },
    { open: "[", close: "]" },
    { open: "(", close: ")" },
    { open: '"', close: '"', notIn: ["string", "comment"] },
    { open: "'", close: "'", notIn: ["string", "comment"] },
    { open: "`", close: "`", notIn: ["string", "comment"] },
  ],
  surroundingPairs: [
    { open: "{", close: "}" },
    { open: "[", close: "]" },
    { open: "(", close: ")" },
    { open: '"', close: '"' },
    { open: "'", close: "'" },
  ],
  indentationRules: {
    increaseIndentPattern: /^((?!--).)*((\b(else|function|then|do|repeat)\b((?!\b(end|until)\b).)*)|(\{\s*)|(\(\s*))$/,
    decreaseIndentPattern: /^\s*((\b(elseif|else|end|until)\b)|(\})|(\)))/,
  },
};

const slangGrammar: Grammar = {
  defaultToken: "",
  keywords: [
    "as", "associatedtype", "break", "case", "const", "continue", "default", "discard", "do", "else", "enum", "extension",
    "false", "for", "func", "if", "implementing", "import", "in", "inout", "interface", "internal", "is", "let", "module",
    "namespace", "out", "private", "public", "return", "static", "struct", "switch", "This", "this", "true", "typealias",
    "typedef", "uniform", "var", "where", "while", "__exported", "__generic", "__include", "__subscript", "__init",
  ],
  typeKeywords: ["bool", "double", "float", "half", "int", "uint", "void", "matrix", "vector", "Ptr", "Texture2D", "SamplerState", "RWTexture2D", "StructuredBuffer", "RWStructuredBuffer"],
  tokenizer: {
    root: [
      [/^\s*#\s*\w+/, "keyword"],
      // An attribute such as `[Canvas]` or `[Pass(1)]` starts its line; indexing like `a[i]` never does
      [/^(\s*)(\[)([A-Za-z_]\w*)/, ["", "annotation", { token: "annotation", next: "@attribute" }]],
      [/\/\/.*$/, "comment"],
      [/\/\*/, { token: "comment", next: "@blockComment" }],
      [/"/, { token: "string", next: "@string" }],
      [/0[xX][\da-fA-F]+[uUlL]*/, "number"],
      [/(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?[fFhHlLuU]*/, "number"],
      [/(bool|double|float|half|int|uint)([1-4](x[1-4])?)\b/, "type"],
      [/[A-Za-z_]\w*/, { cases: { "@keywords": "keyword", "@typeKeywords": "type", "@default": "identifier" } }],
      [/[{}()[\]]/, "delimiter"],
      [/[<>]=?|[-+*/%&|^!=]=?|&&|\|\||\+\+|--|<<|>>|[?:;,.~]/, "delimiter"],
      [/\s+/, ""],
    ],
    attribute: [
      [/\(/, { token: "delimiter", next: "@attributeArguments" }],
      [/\]/, { token: "annotation", next: "@pop" }],
      [/[^\]()]+/, "annotation"],
    ],
    attributeArguments: [[/\)/, { token: "delimiter", next: "@pop" }], { include: "root" }],
    blockComment: [
      [/\*\//, { token: "comment", next: "@pop" }],
      [/[^*]+/, "comment"],
      [/\*/, "comment"],
    ],
    string: [
      [/\\./, "string.escape"],
      [/"/, { token: "string", next: "@pop" }],
      [/[^\\"]+/, "string"],
    ],
  },
};

const slangConfiguration: Configuration = {
  comments: { lineComment: "//", blockComment: ["/*", "*/"] },
  brackets: [
    ["{", "}"],
    ["[", "]"],
    ["(", ")"],
  ],
  autoClosingPairs: [
    { open: "{", close: "}" },
    { open: "[", close: "]" },
    { open: "(", close: ")" },
    { open: '"', close: '"', notIn: ["string", "comment"] },
    { open: "/*", close: " */", notIn: ["string"] },
  ],
  surroundingPairs: [
    { open: "{", close: "}" },
    { open: "[", close: "]" },
    { open: "(", close: ")" },
    { open: '"', close: '"' },
  ],
};

export function registerLanguages(monaco: typeof Monaco) {
  monaco.languages.register({ id: LUAU, extensions: [".luau"] });
  monaco.languages.setMonarchTokensProvider(LUAU, luauGrammar);
  monaco.languages.setLanguageConfiguration(LUAU, luauConfiguration);
  monaco.languages.register({ id: SLANG, extensions: [".slang"] });
  monaco.languages.setMonarchTokensProvider(SLANG, slangGrammar);
  monaco.languages.setLanguageConfiguration(SLANG, slangConfiguration);
}
