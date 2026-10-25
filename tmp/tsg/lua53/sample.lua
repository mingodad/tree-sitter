-- a line comment
--[[ a long
     comment ]]
local x = 1 + 2 * 3 ^ 2 - -4
local s = [[hello
world]]
local t = { 1, 2, name = "x", [3] = 4; }
function f(a, b, ...) return a + b end
for i = 1, 10 do x = x + 1 end
for k, v in pairs(t) do x = k end
while x < 10 do x = x + 1 end
repeat x = x - 1 until x == 0
if x then x = 1 elseif x == 0 then x = 2 else x = 3 end
::top:: goto top
local a, b = f(1, 2), {1,2,3}
t.x, t[1] = 10, 20
print("done", 1 // 2, x & 3 | 4 ~ 5 << 1)
