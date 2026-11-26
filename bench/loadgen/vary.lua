-- Search pages with a different word on each request, so no page cache
-- answers instead of the application (the same words for every system).
local words = {"iron", "forge", "anvil", "hammer", "tongs", "quench", "temper",
  "slag", "railing", "gate", "hinge", "rivet", "weld", "coal", "bellows",
  "scroll", "chisel", "walnut", "dovetail", "patina"}
local i = 0
request = function()
  i = i + 1
  local w = words[(i % #words) + 1]
  local path = string.gsub(wrk.path, "WORD", w .. "&n=" .. i)
  return wrk.format("GET", path)
end
