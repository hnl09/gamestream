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

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const parts = url.pathname.split("/").filter(Boolean); // ["v1", ...]
    if (parts[0] !== "v1") return json({ error: "not found" }, 404);

    if (parts[1] === "health") return json({ ok: true });

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
