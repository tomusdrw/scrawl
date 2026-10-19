-- Optional Omarchy 4 bindings. Load from ~/.config/hypr/bindings.lua:
-- dofile("/usr/share/doc/scrawl/omarchy.lua")
-- Check existing shortcuts before enabling. Copy and edit this file in the
-- user configuration directory to customize it; pacman owns this copy.
o.bind("ALT + SHIFT + S", "Scrawl record", "/usr/bin/scrawl toggle")
o.bind("ALT + SHIFT + D", "Scrawl draw", "/usr/bin/scrawl draw")
o.bind("ALT + SHIFT + V", "Scrawl copy latest", "/usr/bin/scrawl send latest --clipboard")
