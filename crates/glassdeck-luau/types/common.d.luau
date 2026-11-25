-- Shared by keyboard.luau and host.luau. Load this plus the file for the half you're editing.

-- Plain data only: tables, numbers, strings and booleans. This is what crosses the Link.
type Data = { [string]: any } | { any } | number | string | boolean

type Settings = { [string]: any }
