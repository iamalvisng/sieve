-- plain-root.lua: a root-level file with no matching concept slug.
-- The deep-tier golden uses this to exercise the phantom-concept path
-- in ask's loadCorpus (notes/deep-tier.md section 2.8) and the
-- isRootFileCard heading test (section 2.6), on a stem that never
-- collides with a concept.
local function noop()
  return nil
end

return { noop = noop }
