-- GameStream: painel de controles dentro da janela do vídeo (mpv).
--   * Scroll: zoom no ponto onde o mouse está (cima = aproxima, baixo = afasta).
--   * Arrastar com o botão esquerdo (com zoom): move a imagem.
--   * Botão do meio / Backspace: restaura o tamanho original.
--   * Duplo clique / F: tela cheia.
--   * I: fixa as estatísticas (ping, FPS, perda, Mbps) no canto da tela.
--   * Enter: escrever no chat · 1 a 6: reações (aparecem para todos na hora).
--   * Botão direito: aponta um lugar no vídeo para todos (some em ~1 s).
--   * Segurar o botão direito e arrastar: desenha um traço rápido para todos (some em ~2,5 s).
--   * Painel no canto inferior direito (aparece ao mexer o mouse): som, volume, zoom, tela cheia.
-- O volume é guardado em user-data/gs/* e o GameStream (que toca o áudio) acompanha pela IPC.

local mode = mp.get_opt("gs-mode") or "viewer" -- "viewer" | "preview"
local en = mp.get_opt("gs-lang") == "en"
local function L(pt, eng) return en and eng or pt end
local has_audio = mode == "viewer"

-- Janelinha de câmera: o nome de quem é, no canto. Arrastar move (o GameStream encaixa numa grade
-- invisível, sem sobrepor outra câmera); a roda do mouse aumenta/diminui. Sem tela cheia.
if mode == "cam" then
    local name = (mp.get_opt("gs-name") or ""):gsub("[{}\\]", "")
    local ov = mp.create_osd_overlay("ass-events")
    local hint_until = 0
    local hint_timer = nil
    local menu = nil -- { x, y, w, h } do menu do botão direito
    local menu_timer = nil
    local function draw()
        local d = mp.get_property_native("osd-dimensions")
        if not d or not d.w or d.w <= 0 then return end
        ov.res_x, ov.res_y = d.w, d.h
        local s = string.format("{\\an1\\pos(8,%d)\\fs15\\fnSegoe UI\\b1\\bord2\\shad0\\3c&H000000&\\1c&HFFFFFF&}%s", d.h - 6, name)
        if mp.get_time() < hint_until and not menu then
            s = s .. string.format("\n{\\an8\\pos(%d,6)\\fs12\\fnSegoe UI\\bord2\\shad0\\3c&H000000&\\1c&HFFFFFF&}%s",
                d.w / 2, L("arraste para mover · roda do mouse: tamanho · botão direito: opções",
                           "drag to move · mouse wheel: size · right-click: options"))
        end
        if menu then
            s = s .. string.format("\n{\\an7\\pos(%d,%d)\\bord1\\shad0\\3c&H505050&\\1c&H262626&\\1a&H00&\\p1}m 0 0 l %d 0 %d %d 0 %d{\\p0}",
                menu.x, menu.y, menu.w, menu.w, menu.h, menu.h)
            s = s .. string.format("\n{\\an4\\pos(%d,%d)\\fs16\\fnSegoe UI\\b1\\bord0\\shad0\\1c&H8C8CFF&}%s",
                menu.x + 10, menu.y + menu.h / 2, L("🚫 Desativar vídeo", "🚫 Turn off video"))
        end
        ov.data = s
        ov:update()
    end
    local function close_menu()
        if not menu then return end
        menu = nil
        if menu_timer then menu_timer:kill(); menu_timer = nil end
        mp.remove_key_binding("gs_cam_menu_click")
        mp.remove_key_binding("gs_cam_menu_esc")
        draw()
    end
    -- Botão direito: menu com "Desativar vídeo" (esconde esta câmera só para você, como no Discord).
    -- O clique esquerdo só é capturado com o menu aberto, para não atrapalhar o arrastar.
    mp.add_forced_key_binding("MBTN_RIGHT", "gs_cam_menu", function()
        local d = mp.get_property_native("osd-dimensions")
        local mx, my = mp.get_mouse_pos()
        if not d or not d.w or d.w <= 0 then return end
        local w, h = 170, 34
        menu = { x = math.max(0, math.min(mx, d.w - w)), y = math.max(0, math.min(my, d.h - h)), w = w, h = h }
        mp.add_forced_key_binding("MBTN_LEFT", "gs_cam_menu_click", function()
            local x, y = mp.get_mouse_pos()
            local m = menu
            close_menu()
            if m and x >= m.x and x <= m.x + m.w and y >= m.y and y <= m.y + m.h then
                mp.commandv("script-message", "gs-cam-hide")
            end
        end)
        mp.add_forced_key_binding("ESC", "gs_cam_menu_esc", close_menu)
        if menu_timer then menu_timer:kill() end
        menu_timer = mp.add_timeout(5, close_menu)
        draw()
    end)
    local function show_hint()
        hint_until = mp.get_time() + 1.6
        draw()
        if hint_timer then hint_timer:kill() end
        hint_timer = mp.add_timeout(1.7, draw)
    end
    mp.observe_property("osd-dimensions", "native", draw)
    mp.observe_property("mouse-pos", "native", function(_, m)
        if m and m.hover then show_hint() end
    end)
    local function size(d)
        return function()
            mp.commandv("script-message", "gs-cam-size", tostring(d))
            show_hint()
        end
    end
    mp.add_forced_key_binding("WHEEL_UP", "gs_cam_bigger", size(1))
    mp.add_forced_key_binding("WHEEL_DOWN", "gs_cam_smaller", size(-1))
    mp.add_forced_key_binding("MBTN_LEFT_DBL", "gs_cam_nofs", function() end)
    mp.add_forced_key_binding("f", "gs_cam_nofs_key", function() end)
    return
end

local MAX_ZOOM = 3 -- log2: 2^3 = 800%
local overlay = mp.create_osd_overlay("ass-events")
local buttons = {}
local last_move = 0
local mouse = { x = -1, y = -1, hover = false }
local drag = nil -- { kind = "pan"|"vol", ... }

local volume = 1.0
local muted = false
local stats = ""      -- "Ping 12 ms · 60 FPS · Perda 0.0% · 11.8 Mbps" (vem do GameStream)
local pinned = false  -- estatísticas fixas no canto (tecla I)

mp.observe_property("user-data/gs/stats", "native", function(_, v)
    if type(v) == "string" then stats = v end
end)

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
    if not d or (not show and not (pinned and stats ~= "")) then
        overlay.data = ""
        overlay:update()
        buttons = {}
        return
    end
    overlay.res_x, overlay.res_y = d.w, d.h
    if not show then
        -- Só as estatísticas fixas no canto superior direito (o esquerdo é dos avatares).
        buttons = {}
        local tw = math.floor(stats:len() * 8.2) + 24
        overlay.data = rect(d.w - tw - 12, 12, tw, 34, C_BG, 0x40) .. "\n" .. text(d.w - tw, 29, stats, 16, C_TXT, 4)
        overlay:update()
        return
    end
    local H, pad, gap = 40, 10, 8
    local items = {}
    if has_audio then
        items[#items + 1] = { id = "mute", w = 78, label = muted and L("Ativar som", "Unmute") or L("Silenciar", "Mute") }
        items[#items + 1] = { id = "vol", w = 130 }
        items[#items + 1] = { id = "voltxt", w = 46 }
    end
    items[#items + 1] = { id = "zout", w = 34, label = "−" }
    items[#items + 1] = { id = "ztxt", w = 58 }
    items[#items + 1] = { id = "zin", w = 34, label = "+" }
    items[#items + 1] = { id = "zreset", w = 44, label = "1:1" }
    items[#items + 1] = { id = "fs", w = 96, label = mp.get_property_native("fullscreen") and L("Sair", "Exit") or L("Tela cheia", "Fullscreen") }
    if stats ~= "" then
        items[#items + 1] = { id = "info", w = 34, label = "i" }
    end

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
            local active = (it.id == "mute" and muted) or (it.id == "info" and pinned)
            ass[#ass + 1] = rect(x, y0, it.w, H, active and C_ACCENT or (hov and C_BTN_HOV or C_BTN))
            ass[#ass + 1] = text(x + it.w / 2, y0 + H / 2, it.label, (it.label:len() > 3 or it.id == "info") and 17 or 24, C_TXT)
        end
        x = x + it.w + gap
    end
    local above = y0 - pad - 14
    if stats ~= "" then
        ass[#ass + 1] = text(x0 + total, above, stats, 16, C_TXT, 6)
        above = above - 22
    end
    if zpct > 100 then
        ass[#ass + 1] = text(x0 + total, above, L("Arraste para mover  ·  botão do meio restaura", "Drag to pan  ·  middle button resets"), 15, C_DIM, 6)
        above = above - 22
    end
    ass[#ass + 1] = text(x0 + total, above, L("Enter: chat  ·  1 a 6: reações  ·  botão direito: apontar", "Enter: chat  ·  1 to 6: reactions  ·  right click: point"), 15, C_DIM, 6)
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

-- O mpv move a janela quando você clica e arrasta no vídeo ("window-dragging"). Isso é útil no vídeo
-- normal, mas atrapalha no painel (arrastar o volume levava a janela junto) e com zoom (o arraste
-- serve para mover a imagem). Liga/desliga conforme onde o mouse está, ANTES do clique acontecer.
local win_drag = true
local function update_window_dragging()
    local zoomed = mp.get_property_number("video-zoom", 0) > 0
    local want = not (drag ~= nil or zoomed or over_panel(mouse.x, mouse.y))
    if want ~= win_drag then
        win_drag = want
        mp.set_property_bool("window-dragging", want)
    end
end

local function refresh()
    local show = (mp.get_time() - last_move < 2.5) or (mouse.hover and over_panel(mouse.x, mouse.y)) or drag ~= nil
    render(show)
    update_window_dragging()
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
        elseif b.id == "info" then pinned = not pinned
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
mp.add_forced_key_binding("i", "gs_info", function() pinned = not pinned; refresh() end)
if has_audio then
    mp.add_forced_key_binding("m", "gs_mute", function() toggle_mute(); last_move = mp.get_time(); refresh() end)
    mp.add_forced_key_binding("9", "gs_vol_down", function() set_volume(volume - 0.05); last_move = mp.get_time(); refresh() end, { repeatable = true })
    mp.add_forced_key_binding("0", "gs_vol_up", function() set_volume(volume + 0.05); last_move = mp.get_time(); refresh() end, { repeatable = true })
end
mp.observe_property("osd-dimensions", "native", function() refresh() end)

-- ---------- chat, reações, lista de quem assiste e status (vêm do GameStream) ----------

local social = mp.create_osd_overlay("ass-events")
social.z = 10
local REACTIONS = { "👍", "😂", "🔥", "❤", "😮", "GG" }
local chat_lines = {} -- { from, text, t }
local reacts = {}     -- { r, from, t, x }
local status_text = ""
local typing = false
local CHAT_SECS = 15

-- Texto vindo de outras pessoas: sem comandos ASS.
local function esc(s)
    s = tostring(s or "")
    -- Barra invertida + espaço de largura zero: não forma comandos (\N, \h...).
    s = s:gsub("\\", "\\\226\128\139"):gsub("{", "\\{"):gsub("}", "\\}"):gsub("\n", " ")
    return s
end

mp.register_script_message("gs-chat", function(from, msg)
    chat_lines[#chat_lines + 1] = { from = from or "", text = msg or "", t = mp.get_time() }
    if #chat_lines > 8 then table.remove(chat_lines, 1) end
end)
mp.register_script_message("gs-react", function(from, r)
    if #reacts < 40 then
        reacts[#reacts + 1] = { r = r or "", from = from or "", t = mp.get_time(), x = math.random() }
    end
end)
mp.register_script_message("gs-status", function(s) status_text = s or "" end)

-- Apontar na tela: marcador rápido (some em ~1 s) na posição do vídeo, com o nome de quem apontou.
local pings = {} -- { from, x, y, t }
local PING_SECS = 1.1
mp.register_script_message("gs-ping", function(from, x, y)
    x, y = tonumber(x), tonumber(y)
    if not x or not y then return end
    if #pings >= 12 then table.remove(pings, 1) end
    pings[#pings + 1] = { from = from or "", x = x, y = y, t = mp.get_time() }
end)
-- Desenhos rápidos (traços): { from, pts = {{x, y}...}, t }
local strokes = {}
local DRAW_SECS = 2.5
mp.register_script_message("gs-draw", function(from, s)
    local pts = {}
    for x, y in tostring(s or ""):gmatch("([%d%.]+),([%d%.]+)") do
        pts[#pts + 1] = { tonumber(x), tonumber(y) }
    end
    if #pts < 2 then return end
    if #strokes >= 12 then table.remove(strokes, 1) end
    strokes[#strokes + 1] = { from = from or "", pts = pts, t = mp.get_time() }
end)
local drawing = nil -- traço sendo desenhado agora: { pts = {{u, v}}, x0, y0, moved }

-- Modo privacidade do transmissor: "Volto já" cobrindo o vídeo.
local privacy = nil -- nome do transmissor quando ligado
mp.register_script_message("gs-privacy", function(on, host)
    privacy = (on == "1") and (host or "") or nil
end)

-- Linha (traço) em ASS: ida e volta pelo mesmo caminho, só o contorno aparece.
local function polyline(pts, c, width, alpha)
    if #pts < 2 then return "" end
    local path = { string.format("m %d %d", pts[1][1], pts[1][2]) }
    for i = 2, #pts do path[#path + 1] = string.format("l %d %d", pts[i][1], pts[i][2]) end
    for i = #pts - 1, 1, -1 do path[#path + 1] = string.format("l %d %d", pts[i][1], pts[i][2]) end
    return string.format("{\\an7\\pos(0,0)\\bord%.1f\\shad0\\1a&HFF&\\3c%s\\3a&H%02X&\\p1}%s{\\p0}",
        width, c, alpha or 0, table.concat(path, " "))
end

-- Avatares de quem está na sala (vêm do GameStream como JSON). Fotos: imagens BGRA desenhadas com
-- overlay-add; quem não tem foto ganha um círculo colorido com a inicial.
local utils = require("mp.utils")
local members = {}
local extra_members, members_bottom = 0, 0
local MAX_SHOWN = 10
local used_overlays = 0

local function circle(cx, cy, r, c, alpha)
    local k = r * 0.5523
    return string.format("{\\an7\\pos(0,0)\\bord0\\shad0\\1c%s\\1a&H%02X&\\p1}m %d %d b %d %d %d %d %d %d b %d %d %d %d %d %d b %d %d %d %d %d %d b %d %d %d %d %d %d{\\p0}",
        c, alpha or 0,
        cx, cy - r,
        cx + k, cy - r, cx + r, cy - k, cx + r, cy,
        cx + r, cy + k, cx + k, cy + r, cx, cy + r,
        cx - k, cy + r, cx - r, cy + k, cx - r, cy,
        cx - r, cy - k, cx - k, cy - r, cx, cy - r)
end

local function ring(cx, cy, r, c)
    local k = r * 0.5523
    return string.format("{\\an7\\pos(0,0)\\bord2.5\\shad0\\1a&HFF&\\3c%s\\p1}m %d %d b %d %d %d %d %d %d b %d %d %d %d %d %d b %d %d %d %d %d %d b %d %d %d %d %d %d{\\p0}",
        c,
        cx, cy - r,
        cx + k, cy - r, cx + r, cy - k, cx + r, cy,
        cx + r, cy + k, cx + k, cy + r, cx, cy + r,
        cx - k, cy + r, cx - r, cy + k, cx - r, cy,
        cx - r, cy - k, cx - k, cy - r, cx, cy - r)
end

local function name_color(name)
    local h = 5381
    for i = 1, #name do h = (h * 33 + name:byte(i)) % 4294967296 end
    local palette = { col(124, 92, 255), col(232, 96, 140), col(64, 170, 230), col(70, 190, 120), col(240, 160, 60), col(150, 110, 220), col(60, 180, 180) }
    return palette[(h % #palette) + 1]
end

mp.register_script_message("gs-members", function(json)
    local list = utils.parse_json(json or "[]") or {}
    for i = 1, used_overlays do mp.commandv("overlay-remove", tostring(i)) end
    used_overlays = 0
    members = {}
    local y = 16
    for i, m in ipairs(list) do
        if i > MAX_SHOWN then break end
        local size = tonumber(m.size) or 40
        local e = {
            id = m.id, name = m.name or "?", host = m.host == true, ctl = tonumber(m.ctl) or 0,
            size = size, x = 16, y = y, color = name_color(m.name or "?"),
            initial = (m.name or "?"):match("[%z\1-\127\194-\244][\128-\191]*") or "?",
        }
        if type(m.file) == "string" then
            e.file = m.file
            used_overlays = used_overlays + 1
            mp.commandv("overlay-add", tostring(used_overlays), tostring(e.x), tostring(e.y), m.file, "0", "bgra",
                tostring(size), tostring(size), tostring(size * 4))
        end
        members[#members + 1] = e
        y = y + size + 8
    end
    extra_members = math.max(0, #list - MAX_SHOWN)
    members_bottom = y
end)

local function render_social()
    local d = dims()
    if not d then return end
    social.res_x, social.res_y = d.w, d.h
    local now = mp.get_time()
    local ass = {}

    -- Status (reconectando, controle remoto) no topo, centralizado.
    if status_text ~= "" then
        ass[#ass + 1] = string.format("{\\an8\\pos(%d,%d)\\fs18\\fnSegoe UI\\b1\\bord3\\shad0\\1c%s\\3c%s}%s",
            d.w / 2, 18, C_TXT, C_BG, status_text)
    end

    -- Quem está na sala: coluna de avatares (passe o mouse para ver o nome).
    for _, m in ipairs(members) do
        local cx, cy, r = m.x + m.size / 2, m.y + m.size / 2, m.size / 2
        if not m.file then
            ass[#ass + 1] = circle(cx, cy, r, m.color, 0)
            ass[#ass + 1] = string.format("{\\an5\\pos(%d,%d)\\fs%d\\fnSegoe UI\\b1\\bord0\\shad0\\1c%s}%s",
                cx, cy, math.floor(m.size * 0.45), C_TXT, esc(m.initial))
        end
        if m.host or m.ctl > 0 then
            -- Anel: vermelho = transmitindo, verde = jogando (controle remoto).
            ass[#ass + 1] = ring(cx, cy, r + 2, m.host and col(232, 64, 72) or col(70, 200, 120))
        end
        if mouse.hover and (mouse.x - cx) ^ 2 + (mouse.y - cy) ^ 2 <= (r + 2) ^ 2 then
            local label = esc(m.name)
            if m.host then label = label .. L("  ·  transmitindo", "  ·  streaming") end
            if m.ctl > 0 then label = label .. L("  ·  🎮 jogando", "  ·  🎮 playing") end
            ass[#ass + 1] = string.format("{\\an4\\pos(%d,%d)\\fs17\\fnSegoe UI\\b1\\bord4\\shad0\\1c%s\\3c%s}%s",
                m.x + m.size + 12, cy, C_TXT, C_BG, label)
        end
    end
    if extra_members > 0 then
        ass[#ass + 1] = string.format("{\\an7\\pos(%d,%d)\\fs15\\fnSegoe UI\\b1\\bord2\\shad0\\3c&H000000&\\1c%s}+%d",
            16, members_bottom + 4, C_TXT, extra_members)
    end

    -- Chat no canto inferior esquerdo (some depois de alguns segundos; fica visível enquanto digita).
    local y = typing and (d.h - 70) or (d.h - 24)
    for i = #chat_lines, 1, -1 do
        local m = chat_lines[i]
        local age = now - m.t
        if typing or age < CHAT_SECS then
            local a = 0
            if not typing and age > CHAT_SECS - 3 then a = math.floor((age - (CHAT_SECS - 3)) / 3 * 255) end
            ass[#ass + 1] = string.format(
                "{\\an1\\pos(%d,%d)\\fs19\\fnSegoe UI\\bord2\\shad1\\3c&H000000&\\alpha&H%02X&}{\\b1\\1c%s}%s:{\\b0\\1c%s} %s",
                16, y, math.min(255, a), C_ACCENT, esc(m.from), C_TXT, esc(m.text))
            y = y - 28
        end
    end

    -- Reações subindo pelo lado direito.
    local keep = {}
    for _, r in ipairs(reacts) do
        local age = now - r.t
        if age < 3 then
            keep[#keep + 1] = r
            local ry = d.h - 110 - age * 170
            local rx = d.w - 260 + r.x * 160
            local a = age > 2 and math.floor((age - 2) * 255) or 0
            local font = (r.r == "GG") and "\\fnSegoe UI\\b1\\fs44" or "\\fnSegoe UI Emoji\\fs56"
            ass[#ass + 1] = string.format("{\\an2\\pos(%d,%d)%s\\bord2\\shad1\\3c&H000000&\\1c%s\\alpha&H%02X&}%s",
                rx, ry, font, C_TXT, a, esc(r.r))
            ass[#ass + 1] = string.format("{\\an8\\pos(%d,%d)\\fs14\\fnSegoe UI\\bord1\\shad0\\3c&H000000&\\1c%s\\alpha&H%02X&}%s",
                rx, ry + 4, C_DIM, a, esc(r.from))
        end
    end
    reacts = keep

    -- Marcadores de "apontar": anel que abre e some rápido.
    local keep_p = {}
    local vw, vh = d.w - d.ml - d.mr, d.h - d.mt - d.mb
    for _, pg in ipairs(pings) do
        local age = now - pg.t
        if age < PING_SECS then
            keep_p[#keep_p + 1] = pg
            local px, py = d.ml + pg.x * vw, d.mt + pg.y * vh
            local k = age / PING_SECS
            local a = math.floor(k * k * 255)
            local c = name_color(pg.from)
            ass[#ass + 1] = ring(px, py, 8 + k * 26, c):gsub("\\1a&HFF&", "\\1a&HFF&\\3a&H" .. string.format("%02X", a) .. "&", 1)
            ass[#ass + 1] = circle(px, py, 5, c, a)
            ass[#ass + 1] = string.format("{\\an2\\pos(%d,%d)\\fs15\\fnSegoe UI\\b1\\bord2\\shad0\\3c&H000000&\\1c%s\\alpha&H%02X&}%s",
                px, py - 14, C_TXT, a, esc(pg.from))
        end
    end
    pings = keep_p

    -- Traços desenhados (de todo mundo) e o que você está desenhando agora.
    local keep_s = {}
    for _, st in ipairs(strokes) do
        local age = now - st.t
        if age < DRAW_SECS then
            keep_s[#keep_s + 1] = st
            local a = age > DRAW_SECS - 0.8 and math.floor((age - (DRAW_SECS - 0.8)) / 0.8 * 255) or 0
            local pts = {}
            for i, p in ipairs(st.pts) do pts[i] = { d.ml + p[1] * vw, d.mt + p[2] * vh } end
            local c = name_color(st.from)
            ass[#ass + 1] = polyline(pts, "&H000000&", 5, math.min(255, a + 60))
            ass[#ass + 1] = polyline(pts, c, 3, a)
            local last = pts[#pts]
            ass[#ass + 1] = string.format("{\\an1\\pos(%d,%d)\\fs15\\fnSegoe UI\\b1\\bord2\\shad0\\3c&H000000&\\1c%s\\alpha&H%02X&}%s",
                last[1] + 6, last[2] - 6, C_TXT, a, esc(st.from))
        end
    end
    strokes = keep_s
    if drawing then
        -- Amostra a posição do mouse enquanto o botão direito está pressionado.
        local u, v = (mouse.x - d.ml) / vw, (mouse.y - d.mt) / vh
        if u >= 0 and u <= 1 and v >= 0 and v <= 1 then
            local lp = drawing.pts[#drawing.pts]
            if not lp or math.abs((u - lp[1]) * vw) + math.abs((v - lp[2]) * vh) > 6 then
                if #drawing.pts < 64 then drawing.pts[#drawing.pts + 1] = { u, v } end
            end
        end
        if math.abs(mouse.x - drawing.x0) + math.abs(mouse.y - drawing.y0) > 10 then drawing.moved = true end
        if drawing.moved then
            local pts = {}
            for i, p in ipairs(drawing.pts) do pts[i] = { d.ml + p[1] * vw, d.mt + p[2] * vh } end
            ass[#ass + 1] = polyline(pts, C_ACCENT, 3, 0)
        end
    end

    -- Modo privacidade: quem assiste vê "Volto já" no lugar do vídeo; o transmissor, um aviso.
    if privacy then
        if mode == "preview" then
            ass[#ass + 1] = string.format("{\\an7\\pos(0,0)\\bord0\\shad0\\1c&H1C1C24&\\1a&H30&\\p1}m 0 0 l %d 0 %d 64 0 64{\\p0}", d.w, d.w)
            ass[#ass + 1] = string.format("{\\an5\\pos(%d,32)\\fs20\\fnSegoe UI\\b1\\bord0\\shad0\\1c%s}%s", d.w / 2, C_TXT,
                L("⏸ Modo privacidade ligado: os espectadores veem \"Volto já\" (Ctrl+F9 desliga)",
                  "⏸ Privacy mode on: viewers see \"Be right back\" (Ctrl+F9 turns it off)"))
        else
            ass[#ass + 1] = string.format("{\\an7\\pos(0,0)\\bord0\\shad0\\1c&H1C1C24&\\1a&H00&\\p1}m 0 0 l %d 0 %d %d 0 %d{\\p0}", d.w, d.w, d.h, d.h)
            ass[#ass + 1] = string.format("{\\an5\\pos(%d,%d)\\fs64\\fnSegoe UI\\b1\\bord0\\shad0\\1c%s}%s", d.w / 2, d.h / 2 - 24, C_TXT,
                L("⏸ Volto já", "⏸ Be right back"))
            local who = privacy ~= "" and esc(privacy) or L("O transmissor", "The streamer")
            ass[#ass + 1] = string.format("{\\an5\\pos(%d,%d)\\fs22\\fnSegoe UI\\bord0\\shad0\\1c%s}%s", d.w / 2, d.h / 2 + 36, C_DIM,
                who .. L(" pausou a imagem por um instante.", " paused the picture for a moment."))
        end
    end

    social.data = table.concat(ass, "\n")
    social:update()
end

mp.add_periodic_timer(1 / 30, render_social)

-- Enter: caixa de texto do chat (mpv 0.38+). Esc cancela.
local input_ok, input = pcall(require, "mp.input")
local function open_chat()
    if not input_ok or typing then return end
    typing = true
    input.get({
        prompt = L("Mensagem: ", "Message: "),
        submit = function(t)
            if t and t:match("%S") then mp.commandv("script-message", "gs-out-chat", t) end
            input.terminate()
        end,
        closed = function() typing = false end,
    })
end
mp.add_forced_key_binding("ENTER", "gs_chat", open_chat)

-- Botão direito: clique = aponta; segurar e arrastar = desenha um traço (fora do painel; os pontos
-- são enviados em relação ao vídeo).
mp.add_forced_key_binding("MBTN_RIGHT", "gs_ping", function(e)
    local d = dims()
    if not d then return end
    local vw, vh = d.w - d.ml - d.mr, d.h - d.mt - d.mb
    if vw <= 0 or vh <= 0 then return end
    if e.event == "down" then
        if panel_hit(mouse.x, mouse.y) then return end
        local u, v = (mouse.x - d.ml) / vw, (mouse.y - d.mt) / vh
        if u < 0 or u > 1 or v < 0 or v > 1 then return end
        drawing = { pts = { { u, v } }, x0 = mouse.x, y0 = mouse.y, moved = false }
    elseif e.event == "up" and drawing then
        local st = drawing
        drawing = nil
        if st.moved and #st.pts >= 2 then
            local parts = {}
            for i, p in ipairs(st.pts) do parts[i] = string.format("%.4f,%.4f", p[1], p[2]) end
            mp.commandv("script-message", "gs-out-draw", table.concat(parts, ";"))
        else
            local p = st.pts[1]
            mp.commandv("script-message", "gs-out-ping", string.format("%.4f", p[1]), string.format("%.4f", p[2]))
        end
    end
end, { complex = true })
mp.add_forced_key_binding("KP_ENTER", "gs_chat_kp", open_chat)
-- S/Shift+S/Ctrl+S do mpv salvam capturas (mpv-shotNNNN.jpg) na pasta do GameStream sem querer.
for _, k in ipairs({ "s", "S", "Ctrl+s", "Alt+s" }) do
    mp.add_forced_key_binding(k, "gs_noshot_" .. k, function() end)
end
for i, r in ipairs(REACTIONS) do
    mp.add_forced_key_binding(tostring(i), "gs_react_" .. i, function()
        if typing then return end
        mp.commandv("script-message", "gs-out-react", r)
    end)
end
