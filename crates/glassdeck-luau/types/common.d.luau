-- Shared by keyboard.luau and host.luau. Load this plus the file for the half you're editing.

-- A file in this module, from keyboard.resource(name) or host.resource(name). The name is checked when
-- the handle is made: lowercase letters, digits, ".", "_" and "-", in folders split by "/".
declare extern type Resource with
    -- the file's bytes
    function read(self): string
end

