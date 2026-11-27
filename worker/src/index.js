// GameStream - servidor de encontro (Cloudflare Worker + Durable Objects, plano gratuito).
//
// Só troca endereços entre transmissor e espectador; o vídeo nunca passa por aqui.
// Nada é registrado em log nem armazenado além do hash do segredo de cada sala.
//
// Rotas:
//   GET  /v1/health                    -> {"ok":true}
//   GET  /v1/room/<CODE>/host          -> WebSocket do transmissor (mantém a sala aberta)
//   POST /v1/room/<CODE>/join          -> espectador: {"reply":[...]} -> {"invite":"GS-..."}
//   POST /v1/speedtest                 -> descarta o corpo e devolve {"bytes":N}
//   POST /v1/status                    -> {"codes":["KX792B",...]} -> {"live":{"KX792B":true,...}}
//   GET  /j/<CODE>                     -> página "entrar na sala" (abre gamestream://join/<CODE>)
//
// Proteção contra abuso (o plano gratuito tem cota diária): limite de pedidos por IP
// (60 por minuto; teste de conexão: 20 envios por minuto). Acima disso, responde 429.

const ALPHABET = "23456789ABCDEFGHJKMNPQRSTUVWXYZ"; // sem 0/O/1/I/L
const CODE_RE = /^[23456789ABCDEFGHJKMNPQRSTUVWXYZ]{6}$/;

const json = (obj, status = 200) =>
  new Response(JSON.stringify(obj), { status, headers: { "content-type": "application/json" } });

function b64urlDecode(s) {
  s = s.replace(/-/g, "+").replace(/_/g, "/");
  while (s.length % 4) s += "=";
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

async function sha256(bytes) {
  return new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
}

const hex = (bytes) => [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");

// Código da sala = 6 letras derivadas do SHA-256 da impressão digital do certificado do transmissor.
// (Mesmo algoritmo de src/room.rs.)
async function codeFromFingerprint(fp) {
  const h = await sha256(fp);
  let n = 0n;
  for (let i = 0; i < 8; i++) n = (n << 8n) | BigInt(h[i]);
  let code = "";
  for (let i = 0; i < 6; i++) {
    code += ALPHABET[Number(n % 31n)];
    n /= 31n;
  }
  return code;
}

// Extrai a impressão digital (32 bytes) de um convite GS-...
function fingerprintFromInvite(invite) {
  if (typeof invite !== "string" || !invite.startsWith("GS-") || invite.length > 4096) return null;
  const raw = new TextDecoder().decode(b64urlDecode(invite.slice(3)));
  const bar = raw.indexOf("|");
  if (bar < 0) return null;
  const fp = b64urlDecode(raw.slice(bar + 1));
  return fp.length === 32 ? fp : null;
}

// Limite por IP (binding de rate limiting da Cloudflare). Sem o binding (ex.: wrangler dev), não limita.
async function allowed(env, request, heavy) {
  const limiter = heavy ? env.HEAVY : env.LIMITER;
  if (!limiter) return true;
  const ip = request.headers.get("CF-Connecting-IP") || "unknown";
  try {
    const { success } = await limiter.limit({ key: ip });
    return success;
  } catch {
    return true; // falha do limitador não derruba o serviço
  }
}

const MAX_STATUS_CODES = 8;

const RELEASES = "https://github.com/hnl09/gamestream/releases/latest";

// Página do link de convite: abre o GameStream na sala (gamestream://join/CODE) e, se ele não estiver
// instalado, mostra onde baixar. Sem scripts externos nem cookies.
function joinPage(code, lang) {
  const pretty = code.slice(0, 3) + "-" + code.slice(3);
  const pt = !/^en/i.test(lang || "");
  const t = pt
    ? {
        title: `Entrar na sala ${pretty} - GameStream`,
        desc: `Clique para assistir à transmissão (sala ${pretty}).`,
        opening: "Abrindo o GameStream...",
        button: "Abrir no GameStream",
        or: "Não abriu? Abra o GameStream, vá em Assistir e digite o código:",
        dl: "Ainda não tem o GameStream? Baixar para Windows",
      }
    : {
        title: `Join room ${pretty} - GameStream`,
        desc: `Click to watch the stream (room ${pretty}).`,
        opening: "Opening GameStream...",
        button: "Open in GameStream",
        or: "Didn't open? Open GameStream, go to Watch and type the code:",
        dl: "Don't have GameStream yet? Download for Windows",
      };
  const app = `gamestream://join/${code}`;
  return `<!doctype html>
<html lang="${pt ? "pt-BR" : "en"}"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${t.title}</title>
<meta property="og:title" content="${t.title}">
<meta property="og:description" content="${t.desc}">
<meta name="theme-color" content="#7c5cff">
<style>
  :root { color-scheme: dark; }
  body { margin: 0; min-height: 100vh; display: grid; place-items: center; background: #16171c; color: #f2f2f5;
         font: 16px/1.5 "Segoe UI", system-ui, sans-serif; }
  main { max-width: 440px; padding: 32px 24px; text-align: center; }
  h1 { margin: 0 0 8px; font-size: 26px; color: #9b82ff; }
  .code { font: 700 34px/1.2 Consolas, monospace; letter-spacing: 2px; margin: 18px 0; }
  a.btn { display: inline-block; padding: 12px 22px; border-radius: 10px; background: #7c5cff; color: #fff;
          text-decoration: none; font-weight: 600; }
  p { color: #a0a4b2; }
  a { color: #9b82ff; }
</style></head>
<body><main>
  <h1>GameStream</h1>
  <p>${t.opening}</p>
  <a class="btn" href="${app}">${t.button}</a>
  <p>${t.or}</p>
  <div class="code">${pretty}</div>
  <p><a href="${RELEASES}">${t.dl}</a></p>
</main>
<script>setTimeout(function () { location.href = ${JSON.stringify(app)}; }, 150);</script>
</body></html>`;
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const parts = url.pathname.split("/").filter(Boolean); // ["v1", ...]

    // Link de convite: /j/KX7-92B
    if (parts[0] === "j" && parts.length === 2 && request.method === "GET") {
      const code = parts[1].toUpperCase().replace(/-/g, "");
      if (!CODE_RE.test(code)) return new Response("código inválido / invalid code", { status: 404 });
      if (!(await allowed(env, request, false))) return new Response("too many requests", { status: 429 });
      return new Response(joinPage(code, request.headers.get("Accept-Language")), {
        headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" },
      });
    }

    if (parts[0] !== "v1") return json({ error: "not found" }, 404);

    if (parts[1] === "health") return json({ ok: true });

    if (!(await allowed(env, request, parts[1] === "speedtest"))) {
      return json({ error: "too many requests" }, 429);
    }

    // Quais salas estão ao vivo agora (lista de salas recentes do app).
    if (parts[1] === "status" && request.method === "POST") {
      let body = {};
      try {
        body = await request.json();
      } catch {}
      const codes = (Array.isArray(body.codes) ? body.codes : [])
        .filter((c) => typeof c === "string")
        .map((c) => c.toUpperCase())
        .filter((c) => CODE_RE.test(c))
        .slice(0, MAX_STATUS_CODES);
      const live = {};
      await Promise.all(
        codes.map(async (code) => {
          try {
            const stub = env.ROOMS.get(env.ROOMS.idFromName(code));
            const r = await stub.fetch(new Request(`${url.origin}/v1/room/${code}/status`));
            live[code] = ((await r.json()) || {}).live === true;
          } catch {
            live[code] = false;
          }
        }),
      );
      return json({ live });
    }

    if (parts[1] === "speedtest" && request.method === "POST") {
      let bytes = 0;
      if (request.body) {
        const reader = request.body.getReader();
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          bytes += value.byteLength;
        }
      }
      return json({ bytes });
    }

    if (parts[1] === "room" && parts.length === 4) {
      const code = parts[2].toUpperCase();
      if (!CODE_RE.test(code)) return json({ error: "invalid code" }, 400);
      const stub = env.ROOMS.get(env.ROOMS.idFromName(code));
      return stub.fetch(request);
    }
    return json({ error: "not found" }, 404);
  },
};

export class Room {
  constructor(ctx, env) {
    this.ctx = ctx;
    // Ping de aplicação respondido pelo runtime, sem acordar a sala nem consumir cota.
    this.ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
  }

  async fetch(request) {
    const url = new URL(request.url);
    const [, , code, action] = url.pathname.split("/").filter(Boolean);

    if (action === "host") {
      if (request.headers.get("Upgrade") !== "websocket") return json({ error: "expected websocket" }, 426);
      const pair = new WebSocketPair();
      this.ctx.acceptWebSocket(pair[1]);
      pair[1].serializeAttachment({ code: code.toUpperCase(), ready: false });
      return new Response(null, { status: 101, webSocket: pair[0] });
    }

    if (action === "status") {
      return json({ live: this.hosts().length > 0 });
    }

    if (action === "join" && request.method === "POST") {
      let body = {};
      try {
        body = await request.json();
      } catch {}
      const host = this.hosts()[0];
      if (!host) return json({ error: "offline" }, 404);
      const { invite } = host.deserializeAttachment() || {};
      const reply = Array.isArray(body.reply) ? body.reply.filter((a) => typeof a === "string").slice(0, 16) : [];
      if (reply.length) host.send(JSON.stringify({ type: "punch", addrs: reply }));
      return json({ invite });
    }
    return json({ error: "not found" }, 404);
  }

  async webSocketMessage(ws, message) {
    let msg;
    try {
      msg = JSON.parse(typeof message === "string" ? message : new TextDecoder().decode(message));
    } catch {
      return;
    }
    if (msg.type !== "hello") return;
    const att = ws.deserializeAttachment() || {};
    const fp = fingerprintFromInvite(msg.invite);
    if (!fp || (await codeFromFingerprint(fp)) !== att.code) {
      ws.send(JSON.stringify({ type: "error", error: "invite does not match room code" }));
      ws.close(4000, "bad invite");
      return;
    }
    if (typeof msg.secret !== "string" || msg.secret.length < 32 || msg.secret.length > 128) {
      ws.close(4001, "bad secret");
      return;
    }
    // A primeira conexão define o dono da sala; as seguintes precisam do mesmo segredo.
    const secretHash = hex(await sha256(new TextEncoder().encode(msg.secret)));
    const owner = await this.ctx.storage.get("owner");
    if (owner && owner !== secretHash) {
      ws.send(JSON.stringify({ type: "error", error: "room owned by another host" }));
      ws.close(4003, "not owner");
      return;
    }
    if (!owner) await this.ctx.storage.put("owner", secretHash);
    // Reconexão do mesmo dono: fecha a conexão anterior.
    for (const old of this.hosts()) {
      if (old !== ws) old.close(4002, "replaced");
    }
    // O estado fica na própria conexão (sobrevive à hibernação da sala).
    ws.serializeAttachment({ code: att.code, ready: true, invite: msg.invite });
    ws.send(JSON.stringify({ type: "ok" }));
  }

  // Conexões de transmissor já identificadas ("hello" aceito).
  hosts() {
    return this.ctx.getWebSockets().filter((w) => (w.deserializeAttachment() || {}).ready);
  }

  webSocketClose(ws, code, reason) {
    try {
      ws.close(code, reason);
    } catch {}
  }
}
