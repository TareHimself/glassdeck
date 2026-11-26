-- Shared by keyboard.luau and host.luau. Load this plus the file for the half you're editing.

-- Plain data only: tables, numbers, strings and booleans. This is what crosses the Link.
export type Data = { [string]: any } | { any } | number | string | boolean

-- A file in this module, from keyboard.resource(name) or host.resource(name). The name is checked when
-- the handle is made: lowercase letters, digits, ".", "_" and "-", in folders split by "/".
declare extern type Resource with
    -- the file's bytes
    function read(self): string
end

-- assets/keyboard/<name>: deployed with the module, usable on both sides
declare keyboard: { resource: (name: string) -> Resource }
