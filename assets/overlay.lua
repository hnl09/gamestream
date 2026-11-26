-- GameStream: painel de controles dentro da janela do vídeo (mpv).
--   * Scroll: zoom no ponto onde o mouse está (cima = aproxima, baixo = afasta).
--   * Arrastar com o botão esquerdo (com zoom): move a imagem.
--   * Clique do meio / Backspace: volta ao tamanho normal.
--   * Duplo clique / F: tela cheia.
--   * Painel no canto inferior direito (aparece ao mexer o mouse): som, volume, zoom, tela cheia.
-- O volume é guardado em user-data/gs/* e o GameStream (que toca o áudio) acompanha pela IPC.

local mode = mp.get_opt("gs-mode") or "viewer" -- "viewer" | "preview"
local has_audio = mode == "viewer"

local MAX_ZOOM = 3 -- log2: 2^3 = 800%
local overlay = mp.create_osd_overlay("ass-events")
local buttons = {}
local last_move = 0
local mouse = { x = -1, y = -1, hover = false }
local drag = nil -- { kind = "pan"|"vol", ... }

local volume = 1.0
local muted = false

if has_audio then
    mp.set_property_native("user-data/gs/volume", volume)
    mp.set_property_native("user-data/gs/muted", muted)
    mp.observe_property("user-data/gs/volume", "native", function(_, v)
        if type(v) == "number" then volume = v end
    end)
    mp.observe_property("user-data/gs/muted", "native", function(_, v)
        if type(v) == "boolean" then muted = v end
    end)
end

local function set_volume(v)
    volume = math.max(0, math.min(2, v))
    mp.set_property_native("user-data/gs/volume", volume)
    if muted and volume > 0 then
        muted = false
        mp.set_property_native("user-data/gs/muted", false)
    end
end

local function toggle_mute()
    muted = not muted
    mp.set_property_native("user-data/gs/muted", muted)
end

-- ---------- zoom ----------

local function dims()
    local d = mp.get_property_native("osd-dimensions")
    if not d or not d.w or d.w <= 0 then return nil end
    return d
end

local function clamp_center(c, size, win)
    if size <= win then return win / 2 end
    local m = (size - win) / 2
    return math.max(win / 2 - m, math.min(win / 2 + m, c))
end

-- Aplica zoom `nz` (log2) mantendo o ponto (mx, my) da tela parado.
local function zoom_to(nz, mx, my)
    local d = dims()
    if not d then return end
    local z = mp.get_property_number("video-zoom", 0)
    nz = math.max(0, math.min(MAX_ZOOM, nz))
    local vw, vh = d.w - d.ml - d.mr, d.h - d.mt - d.mb
    if vw <= 0 or vh <= 0 then return end
    mx = mx or d.w / 2
    my = my or d.h / 2
    local ux, uy = (mx - d.ml) / vw, (my - d.mt) / vh
    local k = 2 ^ (nz - z)
    local nvw, nvh = vw * k, vh * k
    local cx = clamp_center(mx - ux * nvw + nvw / 2, nvw, d.w)
    local cy = clamp_center(my - uy * nvh + nvh / 2, nvh, d.h)
    mp.set_property_number("video-zoom", nz)
    mp.set_property_number("video-pan-x", (cx - d.w / 2) / nvw)
    mp.set_property_number("video-pan-y", (cy - d.h / 2) / nvh)
end

local function zoom_step(dir, mx, my)
    local z = mp.get_property_number("video-zoom", 0)
    zoom_to(z + dir * math.log(1.25) / math.log(2), mx, my)
end

local function reset_zoom()
    mp.set_property_number("video-zoom", 0)
    mp.set_property_number("video-pan-x", 0)
    mp.set_property_number("video-pan-y", 0)
end

-- ---------- desenho (ASS) ----------

local function col(r, g, b) return string.format("&H%02X%02X%02X&", b, g, r) end
local C_BG = col(22, 23, 28)
local C_BTN = col(52, 55, 66)
local C_BTN_HOV = col(72, 76, 90)
local C_ACCENT = col(124, 92, 255)
local C_TXT = col(255, 255, 255)
local C_DIM = col(160, 164, 178)

local function rect(x, y, w, h, c, alpha)
    return string.format("{\\an7\\pos(%d,%d)\\bord0\\shad0\\1c%s\\1a&H%02X&\\p1}m 0 0 l %d 0 l %d %d l 0 %d{\\p0}",
        x, y, c, alpha or 0, w, w, h, h)
end

local function text(x, y, s, size, c, an)
    return string.format("{\\an%d\\pos(%d,%d)\\bord0\\shad0\\fs%d\\fnSegoe UI\\b1\\1c%s}%s",
        an or 5, x, y, size, c, s)
end

local function inside(b, x, y)
    return x >= b.x and x <= b.x + b.w and y >= b.y and y <= b.y + b.h
end

local function render(show)
    local d = dims()
    if not d or not show then
        overlay.data = ""
        overlay:update()
        buttons = {}
        return
    end
    overlay.res_x, overlay.res_y = d.w, d.h
    local H, pad, gap = 40, 10, 8
    local items = {}
    if has_audio then
        items[#items + 1] = { id = "mute", w = 78, label = muted and "Ativar som" or "Mutar" }
        items[#items + 1] = { id = "vol", w = 130 }
        items[#items + 1] = { id = "voltxt", w = 46 }
    end
    items[#items + 1] = { id = "zout", w = 34, label = "−" }
    items[#items + 1] = { id = "ztxt", w = 58 }
    items[#items + 1] = { id = "zin", w = 34, label = "+" }
    items[#items + 1] = { id = "zreset", w = 44, label = "1:1" }
    items[#items + 1] = { id = "fs", w = 96, label = mp.get_property_native("fullscreen") and "Sair" or "Tela cheia" }

    local total = pad * 2
    for i, it in ipairs(items) do total = total + it.w + (i > 1 and gap or 0) end
    local x0 = d.w - total - 16
    local y0 = d.h - H - 16 - pad * 2 + pad
    local ass = { rect(x0, y0 - pad, total, H + pad * 2, C_BG, 0x30) }
    buttons = {}
    local x = x0 + pad
    local zpct = math.floor(2 ^ mp.get_property_number("video-zoom", 0) * 100 + 0.5)
    for _, it in ipairs(items) do
        local b = { id = it.id, x = x, y = y0, w = it.w, h = H }
        buttons[#buttons + 1] = b
        local hov = inside(b, mouse.x, mouse.y)
        if it.id == "vol" then
            local by = y0 + H / 2 - 3
            ass[#ass + 1] = rect(x, by, it.w, 6, C_BTN)
            local fw = math.floor(it.w * (muted and 0 or volume) / 2)
            ass[#ass + 1] = rect(x, by, fw, 6, C_ACCENT)
            ass[#ass + 1] = rect(x + fw - 5, by - 5, 10, 16, C_TXT)
        elseif it.id == "voltxt" then
            ass[#ass + 1] = text(x + it.w / 2, y0 + H / 2, muted and "0%" or (math.floor(volume * 100 + 0.5) .. "%"), 17, C_DIM)
        elseif it.id == "ztxt" then
            ass[#ass + 1] = text(x + it.w / 2, y0 + H / 2, zpct .. "%", 17, zpct > 100 and C_ACCENT or C_DIM)
        else
            local active = (it.id == "mute" and muted)
            ass[#ass + 1] = rect(x, y0, it.w, H, active and C_ACCENT or (hov and C_BTN_HOV or C_BTN))
            ass[#ass + 1] = text(x + it.w / 2, y0 + H / 2, it.label, it.label:len() > 3 and 17 or 24, C_TXT)
        end
        x = x + it.w + gap
    end
    if zpct > 100 then
        ass[#ass + 1] = text(x0 + total, y0 - pad - 14, "Arraste para mover  ·  clique do meio volta ao normal", 15, C_DIM, 6)
    end
    overlay.data = table.concat(ass, "\n")
    overlay:update()
end

local function panel_hit(x, y)
    for _, b in ipairs(buttons) do
        if inside(b, x, y) then return b end
    end
    return nil
end

local function over_panel(x, y)
    if #buttons == 0 then return false end
    local first, last = buttons[1], buttons[#buttons]
    return x >= first.x - 12 and x <= last.x + last.w + 12 and y >= first.y - 12 and y <= first.y + first.h + 12
end

local function vol_from_x(b, x)
    set_volume((x - b.x) / b.w * 2)
end

local function refresh()
    local show = (mp.get_time() - last_move < 2.5) or (mouse.hover and over_panel(mouse.x, mouse.y)) or drag ~= nil
    render(show)
end

mp.observe_property("mouse-pos", "native", function(_, m)
    if not m then return end
    mouse = m
    last_move = mp.get_time()
    if drag and drag.kind == "vol" then
        vol_from_x(drag.b, m.x)
    elseif drag and drag.kind == "pan" then
        local d = dims()
        if d then
            local z = mp.get_property_number("video-zoom", 0)
            local vw, vh = d.w - d.ml - d.mr, d.h - d.mt - d.mb
            local cx = clamp_center(drag.cx + (m.x - drag.x), vw, d.w)
            local cy = clamp_center(drag.cy + (m.y - drag.y), vh, d.h)
            if z > 0 then
                mp.set_property_number("video-pan-x", (cx - d.w / 2) / vw)
                mp.set_property_number("video-pan-y", (cy - d.h / 2) / vh)
            end
        end
    end
    refresh()
end)

mp.add_periodic_timer(0.2, refresh)

local function on_click(e)
    if e.event == "up" then
        drag = nil
        return
    end
    if e.event ~= "down" then return end
    local b = panel_hit(mouse.x, mouse.y)
    if b then
        if b.id == "mute" then toggle_mute()
        elseif b.id == "vol" then drag = { kind = "vol", b = b }; vol_from_x(b, mouse.x)
        elseif b.id == "zin" then zoom_step(1)
        elseif b.id == "zout" then zoom_step(-1)
        elseif b.id == "zreset" then reset_zoom()
        elseif b.id == "fs" then mp.commandv("cycle", "fullscreen")
        end
        refresh()
        return
    end
    if mp.get_property_number("video-zoom", 0) > 0 then
        local d = dims()
        if d then
            local vw, vh = d.w - d.ml - d.mr, d.h - d.mt - d.mb
            drag = { kind = "pan", x = mouse.x, y = mouse.y, cx = d.ml + vw / 2, cy = d.mt + vh / 2 }
        end
    end
end

mp.add_forced_key_binding("MBTN_LEFT", "gs_click", on_click, { complex = true })
mp.add_forced_key_binding("MBTN_LEFT_DBL", "gs_dbl", function()
    if not panel_hit(mouse.x, mouse.y) then mp.commandv("cycle", "fullscreen") end
end)

local function wheel(dir)
    return function()
        local b = panel_hit(mouse.x, mouse.y)
        if b and (b.id == "vol" or b.id == "voltxt" or b.id == "mute") then
            set_volume(volume + dir * 0.05)
        else
            zoom_step(dir, mouse.x, mouse.y)
        end
        last_move = mp.get_time()
        refresh()
    end
end

mp.add_forced_key_binding("WHEEL_UP", "gs_wheel_up", wheel(1), { repeatable = true })
mp.add_forced_key_binding("WHEEL_DOWN", "gs_wheel_down", wheel(-1), { repeatable = true })
mp.add_forced_key_binding("MBTN_MID", "gs_reset", function() reset_zoom(); refresh() end)
mp.add_forced_key_binding("BS", "gs_reset_key", function() reset_zoom(); refresh() end)
mp.add_forced_key_binding("f", "gs_fs", function() mp.commandv("cycle", "fullscreen") end)
if has_audio then
    mp.add_forced_key_binding("m", "gs_mute", function() toggle_mute(); last_move = mp.get_time(); refresh() end)
    mp.add_forced_key_binding("9", "gs_vol_down", function() set_volume(volume - 0.05); last_move = mp.get_time(); refresh() end, { repeatable = true })
    mp.add_forced_key_binding("0", "gs_vol_up", function() set_volume(volume + 0.05); last_move = mp.get_time(); refresh() end, { repeatable = true })
end
mp.observe_property("osd-dimensions", "native", function() refresh() end)
