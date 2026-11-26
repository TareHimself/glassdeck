-- options.luau: what the user can set for a package. It runs again whenever a value changes, with
-- `current` holding the values; every run must declare the same options, and values only change what's shown.

declare extern type ColorBuilder with
    -- false hides the alpha slider
    function alpha(self, shown: boolean): ColorBuilder
end

declare extern type RangeBuilder with
    function step(self, step: number): RangeBuilder
    function unit(self, unit: string): RangeBuilder
end

declare extern type NumberBuilder with
    function min(self, min: number): NumberBuilder
    function max(self, max: number): NumberBuilder
    function step(self, step: number): NumberBuilder
    function unit(self, unit: string): NumberBuilder
end

declare extern type ToggleBuilder with
end

declare extern type TextBuilder with
    function placeholder(self, text: string): TextBuilder
    function max_length(self, length: number): TextBuilder
end

declare extern type ChoiceBuilder with
end

export type Builder = ColorBuilder | RangeBuilder | NumberBuilder | ToggleBuilder | TextBuilder | ChoiceBuilder

declare extern type OptionHandle with
    -- purely visual: the option exists and keeps its value either way
    function visible(self, shown: boolean): OptionHandle
    function hint(self, text: string): OptionHandle
end

-- "#rgb", "#rrggbb", "#rrggbbaa", or a vector of r, g, b, a from 0 to 1
declare function color(default: string | vector): ColorBuilder
declare function range(default: number, min: number, max: number): RangeBuilder
declare function number(default: number): NumberBuilder
declare function toggle(default: boolean): ToggleBuilder
declare function text(default: string): TextBuilder
declare function choice(default: string, values: { string }): ChoiceBuilder

-- The label defaults from the key: "drift_speed" reads "Drift speed"
declare option: ((key: string, builder: Builder) -> OptionHandle) & ((key: string, label: string, builder: Builder) -> OptionHandle)
declare section: ((key: string, body: () -> ()) -> ()) & ((key: string, label: string, body: () -> ()) -> ())
declare function heading(text: string): ()
